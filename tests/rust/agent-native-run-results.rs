use super::*;
use ivy_agent_manager::{
    local_state::{Pin as StatePin, SavedState},
    native_operations::{NativeOperationStore, NativeRun, StoreFuture},
};

#[tokio::test]
async fn a_lost_native_producer_keeps_original_receipts_before_and_after_the_late_reply() {
    for completed_reply in [false, true] {
        let Fixture {
            _root,
            operations,
            receipts,
            rpc,
            mut peer,
            input,
            dispatch: _,
        } = fixture(128 * 1024 * 1024);
        let id = identity(if completed_reply {
            "lost-after-reply"
        } else {
            "lost-before-reply"
        });
        let request_id: IvyJson = operations.request_id(&id).unwrap().into();
        let action_id = id.clone();
        let owner = operations.clone();
        let client = rpc.clone();
        let (entered, ready) = oneshot::channel();
        let (release, released) = oneshot::channel();
        let calls = Arc::new(AtomicUsize::new(0));
        let counted = calls.clone();
        let run = operations
            .run(
                id.clone(),
                "account/logout".into(),
                IvyJson::Null,
                move |params| async move {
                    counted.fetch_add(1, Ordering::SeqCst);
                    let memory = owner.journal();
                    let before_send = {
                        let memory = memory.clone();
                        let id = action_id.clone();
                        Arc::new(move |request| {
                            let memory = memory.clone();
                            let id = id.clone();
                            Box::pin(async move {
                                memory
                                    .lock()
                                    .unwrap()
                                    .dispatch(&id, &"epoch".into(), &request)?;
                                Ok(())
                            })
                                as ivy_host_runtime::codex_rpc::HookFuture
                        })
                    };
                    let before_resolve = {
                        let id = action_id.clone();
                        Arc::new(move |request, reply| {
                            let memory = memory.clone();
                            let id = id.clone();
                            Box::pin(async move {
                                memory.lock().unwrap().finish(
                                    &id,
                                    &"epoch".into(),
                                    &request,
                                    &reply,
                                )?;
                                Ok(())
                            })
                                as ivy_host_runtime::codex_rpc::HookFuture
                        })
                    };
                    let request = client.request(
                        "account/logout".into(),
                        params,
                        RequestOptions {
                            request_id: Some(owner.request_id(&action_id)?.into()),
                            before_send: Some(before_send),
                            before_resolve: Some(before_resolve),
                            ..Default::default()
                        },
                    );
                    tokio::pin!(request);
                    if completed_reply {
                        request.as_mut().await.unwrap();
                    } else {
                        poll_fn(|cx| {
                            assert!(request.as_mut().poll(cx).is_pending());
                            Poll::Ready(())
                        })
                        .await;
                    }
                    entered.send(()).unwrap();
                    released.await.unwrap();
                    panic!(
                        "Fixture native producer lost with its original request already dispatched"
                    );
                },
                false,
            )
            .unwrap();
        let frame = tokio::time::timeout(Duration::from_secs(5), peer.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(frame.get("id"), Some(&request_id));
        assert_eq!(frame.get("method"), Some(&IvyJson::from("account/logout")));
        let reply = IvyJson::object([
            ("id", request_id.clone()),
            ("result", IvyJson::object::<&str>([])),
        ]);
        if completed_reply {
            input.try_send(reply.clone()).unwrap();
        }
        ready.await.unwrap();
        release.send(()).unwrap();
        let error = tokio::time::timeout(Duration::from_secs(5), run.wait())
            .await
            .unwrap()
            .unwrap_err();
        assert_eq!(error.code, "internal_error");
        assert_eq!(error.outcome, Outcome::Unknown);
        if !completed_reply {
            let prior = operations.read(&id).await.unwrap().unwrap();
            assert_eq!(prior.get("phase"), Some(&IvyJson::from("dispatched")));
            assert_eq!(prior.get("requestId"), Some(&request_id));
            let duplicate = operations
                .run(
                    id.clone(),
                    "account/logout".into(),
                    IvyJson::Null,
                    |_| async { panic!("A dispatched original must never be replayed") },
                    false,
                )
                .unwrap()
                .wait()
                .await
                .unwrap();
            assert_eq!(hash_json(&duplicate).unwrap(), hash_json(&prior).unwrap());
            input.try_send(reply).unwrap();
        }
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if operations
                    .journal()
                    .lock()
                    .unwrap()
                    .get(&id)
                    .unwrap()
                    .is_some_and(|v| v.get("phase") == Some(&IvyJson::from("succeeded")))
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        operations.settle(&id).await.unwrap();
        receipts.drain().await.unwrap();
        let original = operations.read(&id).await.unwrap().unwrap();
        assert_eq!(original.get("phase"), Some(&IvyJson::from("succeeded")));
        assert_eq!(original.get("requestId"), Some(&request_id));
        assert_eq!(
            original.get("serviceNodeId"),
            Some(&IvyJson::from("fixture.agent"))
        );
        for prevent in [false, true] {
            let observed = operations
                .run(
                    id.clone(),
                    "account/logout".into(),
                    IvyJson::Null,
                    |_| async { panic!("Terminal original must not execute again") },
                    prevent,
                )
                .unwrap()
                .wait()
                .await
                .unwrap();
            assert_eq!(hash_json(&observed).unwrap(), hash_json(&original).unwrap());
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(
            tokio::time::timeout(Duration::from_millis(20), peer.recv())
                .await
                .is_err()
        );
        rpc.close("fixture_done").await;
        receipts.drain().await.unwrap();
    }
}

fn identity(suffix: &str) -> Identity {
    Identity {
        caller_principal_id: "caller".into(),
        operation_id: format!("epoch:{}:{suffix}", chrono::Utc::now().timestamp_millis()).into(),
    }
}
fn payload(mebibytes: usize, filled: bool) -> IvyJson {
    let mut units = Vec::with_capacity(mebibytes * 1024 * 1024 / 2);
    if filled {
        units.resize(units.capacity(), 65);
    } else {
        units.extend([65, 0xdfff]);
    }
    IvyJson::String(JsString(units))
}
fn execute(
    operations: &NativeOperations,
    id: Identity,
    params: IvyJson,
    reply: IvyJson,
    calls: Arc<AtomicUsize>,
) -> NativeRun {
    let original = id.clone();
    let owner = operations.clone();
    operations
        .run(
            id,
            "account/logout".into(),
            params,
            move |_| async move {
                calls.fetch_add(1, Ordering::SeqCst);
                let request: IvyJson = owner.request_id(&original)?.into();
                let memory = owner.journal();
                let mut memory = memory.lock().unwrap();
                memory.dispatch(&original, &"epoch".into(), &request)?;
                let mut saved = memory.finish(
                    &original,
                    &"epoch".into(),
                    &request,
                    &IvyJson::object([("result", reply.clone())]),
                )?;
                // Keep the actual original allocation in the returned receipt. SQLite
                // and the journal still retain exactly the same logical wire value.
                saved.insert("reply", IvyJson::object([("result", reply)]));
                Ok(saved)
            },
            false,
        )
        .unwrap()
}
async fn close(fixture: Fixture) {
    fixture.rpc.close("fixture_done").await;
    fixture.receipts.drain().await.unwrap();
}

#[tokio::test]
async fn result_capacity_refusal_keeps_the_original_durable_success_and_never_replays_it() {
    let fixture = fixture(128 * 1024 * 1024);
    let calls = Arc::new(AtomicUsize::new(0));
    let first_id = identity("result-30");
    let second_id = identity("result-20");
    let last_id = identity("result-7");
    let first = execute(
        &fixture.operations,
        first_id.clone(),
        IvyJson::Null,
        payload(30, false),
        calls.clone(),
    );
    fixture.operations.settle(&first_id).await.unwrap();
    let second = execute(
        &fixture.operations,
        second_id.clone(),
        IvyJson::Null,
        payload(20, false),
        calls.clone(),
    );
    fixture.operations.settle(&second_id).await.unwrap();
    let last = execute(
        &fixture.operations,
        last_id.clone(),
        IvyJson::Null,
        payload(7, false),
        calls.clone(),
    );
    let outcome = last.wait().await;
    let scenario = std::panic::AssertUnwindSafe(async {
        let error = match outcome {
            Err(error) => error,
            Ok(_) => {
                panic!("The retained result must be refused without losing its original receipt")
            }
        };
        assert_eq!(error.code, "agent_reply_unavailable");
        assert_eq!(error.outcome, Outcome::Unknown);
        let saved = fixture.operations.read(&last_id).await.unwrap().unwrap();
        assert_eq!(saved.get("phase"), Some(&"succeeded".into()));
        assert_eq!(
            saved.get("requestId"),
            Some(&IvyJson::from(
                fixture.operations.request_id(&last_id).unwrap()
            ))
        );
        assert_eq!(
            saved
                .get("reply")
                .unwrap()
                .get("result")
                .unwrap()
                .as_js_string()
                .unwrap()
                .0,
            vec![65, 0xdfff]
        );
        let original = fixture
            .operations
            .run(
                last_id,
                "account/logout".into(),
                IvyJson::Null,
                |_| async { panic!("Original receipt must never replay") },
                false,
            )
            .unwrap()
            .wait()
            .await
            .unwrap();
        assert_eq!(hash_json(&saved).unwrap(), hash_json(&original).unwrap());
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    })
    .catch_unwind()
    .await;
    drop(first);
    drop(second);
    close(fixture).await;
    if let Err(error) = scenario {
        std::panic::resume_unwind(error);
    }
}

#[tokio::test]
async fn unread_native_receipts_keep_slots_but_leave_cleanup_and_original_reads_available() {
    let mut fixture = fixture(128 * 1024 * 1024);
    let calls = Arc::new(AtomicUsize::new(0));
    let original = identity("held-original");
    let first = execute(
        &fixture.operations,
        original.clone(),
        IvyJson::Null,
        IvyJson::Null,
        calls.clone(),
    );
    fixture.operations.settle(&original).await.unwrap();
    let saved = fixture.operations.read(&original).await.unwrap().unwrap();
    let mut held = vec![first];
    for _ in 1..56 {
        held.push(
            fixture
                .operations
                .run(
                    original.clone(),
                    "account/logout".into(),
                    IvyJson::Null,
                    |_| async { panic!("Observing the same durable original must never replay") },
                    false,
                )
                .unwrap(),
        );
        fixture.operations.settle(&original).await.unwrap();
    }
    let extra = identity("held-refused");
    let rejected = fixture.operations.run(
        extra.clone(),
        "account/logout".into(),
        IvyJson::Null,
        |_| async { panic!("Refused work must not execute") },
        false,
    );
    let scenario = std::panic::AssertUnwindSafe(async {
        match rejected {
            Err(error) => {
                assert_eq!(error.code, "native_capacity");
                assert_eq!(error.outcome, Outcome::NotExecuted);
            }
            Ok(_) => panic!("Completed unread native receipts must retain their admission"),
        }
        assert!(fixture.operations.read(&extra).await.unwrap().is_none());
        assert_eq!(
            hash_json(&fixture.operations.read(&original).await.unwrap().unwrap()).unwrap(),
            hash_json(&saved).unwrap()
        );
        let cleanup = fixture
            .dispatch
            .invoke(
                identity("held-cleanup"),
                "thread/unsubscribe".into(),
                IvyJson::object([("threadId", "owned".into())]),
            )
            .unwrap();
        let frame = tokio::time::timeout(Duration::from_secs(5), fixture.peer.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(frame.get("method"), Some(&"thread/unsubscribe".into()));
        fixture
            .input
            .try_send(IvyJson::object([
                ("id", frame.required("id").unwrap().clone()),
                (
                    "result",
                    IvyJson::object([("status", "unsubscribed".into())]),
                ),
            ]))
            .unwrap();
        assert_eq!(
            cleanup.wait().await.unwrap().get("phase"),
            Some(&"succeeded".into())
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    })
    .catch_unwind()
    .await;
    drop(held.pop());
    if scenario.is_ok() {
        let fresh = execute(
            &fixture.operations,
            extra,
            IvyJson::Null,
            IvyJson::Null,
            calls.clone(),
        )
        .wait()
        .await
        .unwrap();
        assert_eq!(fresh.get("phase"), Some(&"succeeded".into()));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }
    drop(held);
    close(fixture).await;
    if let Err(error) = scenario {
        std::panic::resume_unwind(error);
    }
}

#[tokio::test]
async fn a_full_claim_budget_refuses_before_storage_or_action_and_keeps_the_original_id_reusable() {
    let fixture = fixture(128 * 1024 * 1024);
    let calls = Arc::new(AtomicUsize::new(0));
    let first_id = identity("claim-30");
    let second_id = identity("claim-20");
    let blocked_id = identity("claim-refused");
    let first = execute(
        &fixture.operations,
        first_id.clone(),
        IvyJson::Null,
        payload(30, false),
        calls.clone(),
    );
    fixture.operations.settle(&first_id).await.unwrap();
    let second = execute(
        &fixture.operations,
        second_id.clone(),
        IvyJson::Null,
        payload(20, false),
        calls.clone(),
    );
    fixture.operations.settle(&second_id).await.unwrap();
    let blocked = execute(
        &fixture.operations,
        blocked_id.clone(),
        payload(7, true),
        IvyJson::Null,
        calls.clone(),
    );
    let outcome = blocked.wait().await;
    let scenario = std::panic::AssertUnwindSafe(async {
        let error = match outcome {
            Err(error) => error,
            Ok(_) => panic!("A full claim budget must refuse before the durable claim"),
        };
        assert_eq!(error.code, "native_capacity");
        assert_eq!(error.outcome, Outcome::NotExecuted);
        assert!(
            fixture
                .operations
                .read(&blocked_id)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            fixture
                .operations
                .journal()
                .lock()
                .unwrap()
                .get(&blocked_id)
                .unwrap()
                .is_none()
        );
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    })
    .catch_unwind()
    .await;
    drop(first);
    drop(second);
    if scenario.is_ok() {
        let accepted = execute(
            &fixture.operations,
            blocked_id,
            payload(7, true),
            IvyJson::Null,
            calls.clone(),
        )
        .wait()
        .await
        .unwrap();
        assert_eq!(accepted.get("phase"), Some(&"succeeded".into()));
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }
    close(fixture).await;
    if let Err(error) = scenario {
        std::panic::resume_unwind(error);
    }
}

struct Hidden {
    store: SqliteOperationStore,
    hide: AtomicBool,
    writes: AtomicUsize,
}
#[tokio::test]
async fn a_known_journal_original_is_never_forgotten_or_reclaimed_when_the_store_read_is_missing() {
    for unfinished in [true, false] {
        known_original(unfinished).await;
    }
}
async fn known_original(unfinished: bool) {
    let selected = Arc::new(Mutex::new(None));
    let target = selected.clone();
    let fixture = fixture_with_store(128 * 1024 * 1024, move |state| {
        let store = Arc::new(Hidden {
            store: SqliteOperationStore::new(state).unwrap(),
            hide: AtomicBool::new(false),
            writes: AtomicUsize::new(0),
        });
        *target.lock().unwrap() = Some(store.clone());
        store
    });
    let store = selected.lock().unwrap().take().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let first_id = identity("known-held-30");
    let second_id = identity("known-held-20");
    let original = identity("known-original");
    let params = if unfinished {
        payload(7, true)
    } else {
        IvyJson::Null
    };
    if unfinished {
        let owner = fixture.operations.clone();
        let action_id = original.clone();
        let counted = calls.clone();
        let pending = fixture
            .operations
            .run(
                original.clone(),
                "account/logout".into(),
                params.clone(),
                move |_| async move {
                    counted.fetch_add(1, Ordering::SeqCst);
                    let request: IvyJson = owner.request_id(&action_id)?.into();
                    owner.journal().lock().unwrap().dispatch(
                        &action_id,
                        &"epoch".into(),
                        &request,
                    )?;
                    Err(
                        IvyError::new("fixture_connection_lost", "Original response was lost")
                            .with_outcome(Outcome::Unknown),
                    )
                },
                false,
            )
            .unwrap()
            .wait()
            .await
            .unwrap_err();
        assert_eq!(pending.code, "fixture_connection_lost");
        assert_eq!(pending.outcome, Outcome::Unknown);
    }
    let first = execute(
        &fixture.operations,
        first_id.clone(),
        IvyJson::Null,
        payload(30, false),
        calls.clone(),
    );
    fixture.operations.settle(&first_id).await.unwrap();
    let second = execute(
        &fixture.operations,
        second_id.clone(),
        IvyJson::Null,
        payload(20, false),
        calls.clone(),
    );
    fixture.operations.settle(&second_id).await.unwrap();
    if !unfinished {
        let original_run = execute(
            &fixture.operations,
            original.clone(),
            IvyJson::Null,
            payload(7, true),
            calls.clone(),
        );
        assert_eq!(
            original_run.wait().await.unwrap_err().code,
            "agent_reply_unavailable"
        );
    }
    fixture.receipts.drain().await.unwrap();
    let saved = fixture.operations.read(&original).await.unwrap().unwrap();
    let saved_hash = hash_json(&saved).unwrap();
    let writes = store.writes.load(Ordering::SeqCst);
    store.hide.store(true, Ordering::SeqCst);
    let scenario = std::panic::AssertUnwindSafe(async {
        let conflicting = fixture
            .operations
            .run(
                original.clone(),
                "account/logout".into(),
                true.into(),
                |_| async { panic!("Conflicting original must not replay") },
                false,
            )
            .unwrap()
            .wait()
            .await
            .unwrap_err();
        assert_eq!(conflicting.code, "mutation_conflict");
        for prevent in [false, true] {
            let observed = fixture
                .operations
                .run(
                    original.clone(),
                    "account/logout".into(),
                    params.clone(),
                    |_| async {
                        panic!("Known journal original must never execute or be prevented again")
                    },
                    prevent,
                )
                .unwrap()
                .wait()
                .await;
            let known = fixture
                .operations
                .journal()
                .lock()
                .unwrap()
                .get(&original)
                .unwrap();
            assert!(
                known.is_some(),
                "A missing store observation must never forget a known original"
            );
            assert_eq!(hash_json(&known.unwrap()).unwrap(), saved_hash);
            if prevent {
                // Prevention can deliver the known original using the control
                // bytes even while ordinary result retention is occupied.
                assert_eq!(hash_json(&observed.unwrap()).unwrap(), saved_hash);
            } else {
                let error = match observed {
                    Err(error) => error,
                    Ok(_) => panic!("The full ordinary result pool must report uncertain delivery"),
                };
                assert_eq!(error.code, "agent_reply_unavailable");
                assert_eq!(error.outcome, Outcome::Unknown);
            }
            assert_eq!(
                store.writes.load(Ordering::SeqCst),
                writes,
                "A known original must not acquire a new durable claim"
            );
        }
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    })
    .catch_unwind()
    .await;
    store.hide.store(false, Ordering::SeqCst);
    drop(first);
    drop(second);
    if scenario.is_ok() {
        let recovered = fixture
            .operations
            .run(
                original,
                "account/logout".into(),
                params,
                |_| async { panic!("Recovery must read the original") },
                false,
            )
            .unwrap()
            .wait()
            .await
            .unwrap();
        assert_eq!(hash_json(&recovered).unwrap(), saved_hash);
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }
    close(fixture).await;
    if let Err(error) = scenario {
        std::panic::resume_unwind(error);
    }
}
impl NativeOperationStore for Hidden {
    fn service_node_id(&self) -> &JsString {
        self.store.service_node_id()
    }
    fn read(&self, key: String) -> StoreFuture<'_, Option<SavedState>> {
        Box::pin(async move {
            if self.hide.load(Ordering::SeqCst) {
                Ok(None)
            } else {
                self.store.read(key).await
            }
        })
    }
    fn write(
        &self,
        key: String,
        value: IvyJson,
        pin: Option<StatePin>,
    ) -> StoreFuture<'_, SavedState> {
        self.writes.fetch_add(1, Ordering::SeqCst);
        self.store.write(key, value, pin)
    }
}
#[tokio::test]
async fn duplicate_reads_keep_the_original_claim_when_storage_is_temporarily_missing() {
    let selected = Arc::new(Mutex::new(None));
    let target = selected.clone();
    let fixture = fixture_with_store(128 * 1024 * 1024, move |state| {
        let store = Arc::new(Hidden {
            store: SqliteOperationStore::new(state).unwrap(),
            hide: AtomicBool::new(false),
            writes: AtomicUsize::new(0),
        });
        *target.lock().unwrap() = Some(store.clone());
        store
    });
    let store = selected.lock().unwrap().take().unwrap();
    let original = identity("claim-fallback");
    let owner = fixture.operations.clone();
    let action_id = original.clone();
    let params = payload(2, false);
    let replay = params.clone();
    let (entered, running) = oneshot::channel();
    let (release, released) = oneshot::channel();
    let run = fixture
        .operations
        .run(
            original.clone(),
            "account/logout".into(),
            params,
            move |_| async move {
                entered.send(()).unwrap();
                released.await.unwrap();
                owner
                    .journal()
                    .lock()
                    .unwrap()
                    .reject_before_dispatch(&action_id, &"fixture_done".into())
            },
            false,
        )
        .unwrap();
    running.await.unwrap();
    store.hide.store(true, Ordering::SeqCst);
    let scenario = std::panic::AssertUnwindSafe(async {
        let fallback = fixture
            .operations
            .run(
                original.clone(),
                "account/logout".into(),
                replay.clone(),
                |_| async { panic!("A duplicate observation must never execute") },
                false,
            )
            .unwrap()
            .wait()
            .await
            .unwrap();
        assert_eq!(fallback.get("phase"), Some(&"accepted".into()));
        assert_eq!(
            fallback.get("operationId"),
            Some(&IvyJson::String(original.operation_id.clone()))
        );
        assert_eq!(
            hash_json(fallback.get("params").unwrap()).unwrap(),
            hash_json(&replay).unwrap()
        );
    })
    .catch_unwind()
    .await;
    store.hide.store(false, Ordering::SeqCst);
    release.send(()).unwrap();
    let saved = run.wait().await.unwrap();
    assert_eq!(saved.get("code"), Some(&"fixture_done".into()));
    close(fixture).await;
    if let Err(error) = scenario {
        std::panic::resume_unwind(error);
    }
}
