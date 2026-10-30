use ivy_contracts::IvyError;
#[path = "../fixtures/hive-dispatch-queue.rs"]
mod fixture_dispatch;
use fixture_dispatch::DispatchGate;
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{runtime::Builder, sync::oneshot};
async fn flush() {
    for _ in 0..32 {
        tokio::task::yield_now().await;
    }
}

#[test]
fn dispatch_does_not_start_expired_work_before_its_timer_is_polled() {
    Builder::new_current_thread().enable_time().start_paused(true).build().unwrap().block_on(async {
        let gate = DispatchGate::new(1, 2);
        let called = Arc::new(AtomicUsize::new(0));
        let observed = called.clone();
        let reply = gate.run(move || async move { observed.fetch_add(1, Ordering::SeqCst); Ok(()) });
        // Advance the clock before polling the initially admitted task.
        tokio::time::advance(Duration::from_millis(2)).await;
        assert_eq!(reply.await.unwrap().unwrap_err().outcome, ivy_contracts::Outcome::NotExecuted);
        assert_eq!(called.load(Ordering::SeqCst), 0);
    });
}

#[test]
fn dispatch_is_eager_releases_completed_slots_and_retains_work_without_receivers() {
    Builder::new_current_thread()
        .enable_time()
        .start_paused(true)
        .build()
        .unwrap()
        .block_on(async {
            let gate = DispatchGate::new(1, 1);
            let first = gate.run(|| async { Ok(7) });
            // Slot admission happens before either future or caller is polled.
            let rejected = gate.run(|| async { Ok(9) });
            assert_eq!(rejected.await.unwrap().unwrap_err().code, "limit_exceeded");
            assert_eq!(first.await.unwrap().unwrap(), 7);
            assert_eq!(gate.run(|| async { Ok(10) }).await.unwrap().unwrap(), 10);
            flush().await;
            assert_eq!(
                gate.run(|| async { Ok("new result type") })
                    .await
                    .unwrap()
                    .unwrap(),
                "new result type"
            );
            flush().await;
            let (input, reply) = oneshot::channel();
            drop(gate.run(|| async move { Ok(reply.await.unwrap()) }));
            assert_eq!(
                gate.run(|| async { Ok(0) })
                    .await
                    .unwrap()
                    .unwrap_err()
                    .code,
                "limit_exceeded"
            );
            input.send(42).unwrap();
            flush().await;
            assert_eq!(gate.run(|| async { Ok(11) }).await.unwrap().unwrap(), 11);

            struct Stalled {
                executions: AtomicUsize,
                released: tokio::sync::watch::Receiver<bool>,
            }
            impl ivy_hive::server_rpc::Storage for Stalled {
                fn dispatch(&self, mut request: ivy_hive::storage::Request, options: ivy_hive::storage::RequestOptions) -> ivy_hive::server_rpc::Reply {
                    if let Err(error) = request.prepare() {
                        return Box::pin(async move { Err(error) });
                    }
                    let reply = self.request(crate::payload_fixture::json(request.take_payload()), options);
                    Box::pin(async move { request.complete(reply.await) })
                }
                fn request(
                    &self,
                    _: ivy_contracts::IvyJson,
                    _: ivy_hive::storage::RequestOptions,
                ) -> ivy_hive::server_rpc::Reply {
                    self.executions.fetch_add(1, Ordering::SeqCst);
                    let mut released = self.released.clone();
                    Box::pin(async move {
                        released
                            .wait_for(|ready| *ready)
                            .await
                            .map_err(|_| IvyError::internal())?;
                        Ok(ivy_contracts::IvyJson::object([(
                            "value",
                            "original".into(),
                        )]))
                    })
                }
            }
            let (release, released) = tokio::sync::watch::channel(false);
            let storage = Arc::new(Stalled {
                executions: AtomicUsize::new(0),
                released,
            });
            let invocation = ivy_hive::server_rpc::Invocation::new(
                storage.clone(),
                Arc::new(|_, _| Ok(None)),
                Arc::new(|| 100000.0),
                ivy_hive::admission::RequestBudget::default(),
                ivy_hive::server_rpc::Options::default(),
            );
            for bytes in [usize::MAX - 1024, usize::MAX - 1023, usize::MAX] {
                let error = invocation
                    .invoke(
                        ivy_contracts::IvyJson::object([("credentialDigest", "overflow".into())]),
                        ivy_contracts::IvyJson::Null,
                        bytes,
                    )
                    .await
                    .unwrap_err();
                assert_eq!(error.code, "limit_exceeded");
                assert_eq!(error.outcome, ivy_contracts::Outcome::NotExecuted);
            }
            assert_eq!(storage.executions.load(Ordering::SeqCst), 0);
            let calls = (0..2100)
                .map(|index| {
                    invocation.invoke(
                        ivy_contracts::IvyJson::object([
                            ("credentialDigest", format!("consumer-{index}").into()),
                            ("principalId", format!("consumer-{index}").into()),
                            ("transport", "http".into()),
                        ]),
                        ivy_contracts::IvyJson::object([(
                            "id",
                            ivy_contracts::IvyJson::Number(index as f64),
                        )]),
                        0,
                    )
                })
                .collect::<Vec<_>>();
            flush().await;
            assert_eq!(storage.executions.load(Ordering::SeqCst), 2048);
            release.send(true).unwrap();
            let (mut completed, mut requests, mut dispatches) = (0, 0, 0);
            for call in calls {
                match call.await {
                    Ok(value) => {
                        assert_eq!(value, "original".into());
                        completed += 1;
                    }
                    Err(error) => {
                        assert_eq!(error.code, "limit_exceeded");
                        assert_eq!(error.outcome, ivy_contracts::Outcome::NotExecuted);
                        match error
                            .details
                            .unwrap()
                            .get("budget")
                            .and_then(ivy_contracts::IvyJson::as_js_string)
                            .unwrap()
                            .to_utf8()
                            .unwrap()
                            .as_str()
                        {
                            "request" => requests += 1,
                            "dispatch" => dispatches += 1,
                            value => panic!("Unexpected refusal budget {value}"),
                        }
                    }
                }
            }
            assert_eq!((completed, requests, dispatches), (2048, 52, 0));
            assert_eq!(storage.executions.load(Ordering::SeqCst), 2048);
        });
}

#[test]
fn dispatch_native_task_failure_drains_queue_and_executor_loss_is_distinct() {
    Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap()
        .block_on(async {
            let gate = DispatchGate::new(2, 30_000);
            let failed = gate.run(|| async {
                panic!("synthetic native task failure");
                #[allow(unreachable_code)]
                Ok::<_, IvyError>(())
            });
            let next = gate.run(|| async { Ok(19) });
            assert!(failed.await.is_err());
            assert_eq!(next.await.unwrap().unwrap(), 19);
        });
    let runtime = Builder::new_current_thread().enable_time().build().unwrap();
    let (unpolled, gate) = runtime.block_on(async {
        let gate = DispatchGate::default();
        let result = gate.run(|| async {
            std::future::pending::<()>().await;
            Ok::<_, IvyError>(())
        });
        (result, gate)
    });
    drop(runtime);
    assert!(unpolled.blocking_recv().is_err());
    // The completion guard also exists before the task's first poll.
    Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap()
        .block_on(async {
            assert_eq!(gate.run(|| async { Ok(23) }).await.unwrap().unwrap(), 23);
        });
}

#[test]
fn dispatch_drops_expired_captures_and_retains_queue_after_gate_handles_are_dropped() {
    struct Capture(Arc<AtomicUsize>);
    impl Drop for Capture {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }
    Builder::new_current_thread()
        .enable_time()
        .start_paused(true)
        .build()
        .unwrap()
        .block_on(async {
            let gate = DispatchGate::new(2, 2);
            let (finish, reply) = oneshot::channel();
            let active = gate.run(|| async move {
                reply.await.unwrap();
                Ok(())
            });
            let released = Arc::new(AtomicUsize::new(0));
            let capture = Capture(released.clone());
            let expired = gate.run(move || async move {
                drop(capture);
                panic!("Expired work must never start");
                #[allow(unreachable_code)]
                Ok(())
            });
            flush().await;
            tokio::time::advance(Duration::from_millis(2)).await;
            flush().await;
            let expired = expired.await.unwrap().unwrap_err();
            assert_eq!(expired.code, "deadline_exceeded");
            assert_eq!(expired.outcome, ivy_contracts::Outcome::NotExecuted);
            assert_eq!(released.load(Ordering::SeqCst), 1);
            let accepted = gate.run(|| async { Ok(37) });
            drop(gate);
            finish.send(()).unwrap();
            active.await.unwrap().unwrap();
            assert_eq!(accepted.await.unwrap().unwrap(), 37);
            assert_eq!(released.load(Ordering::SeqCst), 1);
        });
}

#[test]
fn dispatch_clones_allow_only_one_active_job_under_concurrent_submission() {
    Builder::new_multi_thread()
        .worker_threads(4)
        .enable_time()
        .build()
        .unwrap()
        .block_on(async {
            let gate = DispatchGate::new(64, 30_000);
            let active = Arc::new(AtomicUsize::new(0));
            let barrier = Arc::new(tokio::sync::Barrier::new(33));
            let mut callers = Vec::new();
            for id in 0..32 {
                let gate = gate.clone();
                let active = active.clone();
                let barrier = barrier.clone();
                callers.push(tokio::spawn(async move {
                    barrier.wait().await;
                    gate.run(move || async move {
                        assert_eq!(
                            active.fetch_add(1, Ordering::SeqCst),
                            0,
                            "Overlapping dispatch work"
                        );
                        tokio::task::yield_now().await;
                        assert_eq!(active.fetch_sub(1, Ordering::SeqCst), 1);
                        Ok(id)
                    })
                    .await
                    .unwrap()
                    .unwrap()
                }));
            }
            barrier.wait().await;
            for (id, caller) in callers.into_iter().enumerate() {
                assert_eq!(caller.await.unwrap(), id);
            }
            assert_eq!(active.load(Ordering::SeqCst), 0);
        });
}

#[test]
fn dispatch_fifo_and_failure_keep_the_following_job_runnable() {
    Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap()
        .block_on(async {
            let gate = DispatchGate::new(4, 30_000);
            let (finish, reply) = oneshot::channel();
            let first = gate.run(|| async move {
                reply.await.unwrap();
                Ok(0)
            });
            let order = Arc::new(Mutex::new(vec![]));
            let mut callers = vec![];
            for id in 1..=3 {
                let order = order.clone();
                callers.push(gate.run(move || async move {
                    order.lock().unwrap().push(id);
                    if id == 2 {
                        Err(IvyError::new("provider_error", "Expected test failure."))
                    } else {
                        Ok(id)
                    }
                }));
            }
            assert_eq!(
                gate.run(|| async { Ok(4) })
                    .await
                    .unwrap()
                    .unwrap_err()
                    .code,
                "limit_exceeded"
            );
            finish.send(()).unwrap();
            assert_eq!(first.await.unwrap().unwrap(), 0);
            for (index, caller) in callers.into_iter().enumerate() {
                let result = caller.await.unwrap();
                if index == 1 {
                    assert_eq!(result.unwrap_err().code, "provider_error");
                } else {
                    assert_eq!(result.unwrap(), index + 1);
                }
            }
            assert_eq!(*order.lock().unwrap(), vec![1, 2, 3]);
        });
}
#[test]
fn dispatch_uses_integer_capacity_and_full_width_native_wait_durations() {
    Builder::new_current_thread()
        .enable_time()
        .start_paused(true)
        .build()
        .unwrap()
        .block_on(async {
            assert_eq!(
                DispatchGate::new(0, 0)
                    .run(|| async { Ok(0) })
                    .await
                    .unwrap()
                    .unwrap_err()
                    .code,
                "limit_exceeded"
            );
            let gate = DispatchGate::new(2, 2_147_483_648);
            let (finish, reply) = oneshot::channel();
            let first = gate.run(|| async move {
                reply.await.unwrap();
                Ok(1)
            });
            let mut waiting = gate.run(|| async { Ok(2) });
            flush().await;
            tokio::time::advance(Duration::from_millis(10)).await;
            flush().await;
            assert!(matches!(
                waiting.try_recv(),
                Err(oneshot::error::TryRecvError::Empty)
            ));
            finish.send(()).unwrap();
            assert_eq!(first.await.unwrap().unwrap(), 1);
            assert_eq!(waiting.await.unwrap().unwrap(), 2);
        });
}
