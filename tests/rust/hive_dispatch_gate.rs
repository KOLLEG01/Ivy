use ivy_contracts::IvyError;
use ivy_hive::admission::DispatchGate;
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
            assert_eq!(
                expired.await.unwrap().unwrap_err().code,
                "deadline_exceeded"
            );
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
