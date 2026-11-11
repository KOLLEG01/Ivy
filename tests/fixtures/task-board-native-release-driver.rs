use super::*;

fn ticket_operation(phase: &str) -> Json {
    Json::object([
        ("request", Json::object([("taskId", Json::String(id(100)))])),
        ("phase", phase.into()),
        ("error", Json::Null),
        ("nativeCalls", Json::Array(vec![])),
    ])
}

#[test]
fn release_pending_work_uses_current_ticket_heads_and_exact_operation_exclusion() {
    let db = Connection::open_in_memory().unwrap();
    local_journal::initialize(&db).unwrap();
    let journal = LocalJournal {
        connection: &db,
        root_object_id: Some("root".into()),
    };
    let mut value = ticket_operation("accepted");
    journal
        .create(
            "task-board/operation",
            &id(900),
            Some(&"root".into()),
            &value,
            &TIME.into(),
            &id(900),
        )
        .unwrap();
    let mut revision = 1.0;
    for phase in ["accepted", "preparing", "publishing", "needs_attention"] {
        value.insert("phase", phase.into());
        if phase != "accepted" {
            journal
                .update(
                    "task-board/operation",
                    &id(900),
                    revision,
                    &value,
                    &TIME.into(),
                )
                .unwrap();
            revision += 1.0;
        }
        assert!(journal.task_operations_pending(&id(100), None).unwrap());
        assert!(
            !journal
                .task_operations_pending(&id(100), Some(&id(900)))
                .unwrap()
        );
        assert!(!journal.task_operations_pending(&id(101), None).unwrap());
    }
    value.insert("phase", "failed".into());
    value.insert("error", Json::object([("execution", "unknown".into())]));
    journal
        .update(
            "task-board/operation",
            &id(900),
            revision,
            &value,
            &TIME.into(),
        )
        .unwrap();
    revision += 1.0;
    assert!(journal.task_operations_pending(&id(100), None).unwrap());
    value.insert("error", Json::Null);
    value.insert(
        "nativeCalls",
        Json::Array(vec![Json::object([(
            "observation",
            Json::object([("phase", "outcome_unknown".into())]),
        )])]),
    );
    journal
        .update(
            "task-board/operation",
            &id(900),
            revision,
            &value,
            &TIME.into(),
        )
        .unwrap();
    revision += 1.0;
    assert!(journal.task_operations_pending(&id(100), None).unwrap());
    value.insert("nativeCalls", Json::Array(vec![]));
    journal
        .update(
            "task-board/operation",
            &id(900),
            revision,
            &value,
            &TIME.into(),
        )
        .unwrap();
    assert!(
        !journal.task_operations_pending(&id(100), None).unwrap(),
        "Retained older accepted revisions are not pending current work."
    );
}

#[tokio::test(flavor = "current_thread")]
async fn completed_release_driver_preserves_original_identity_evidence_and_local_recovery() {
    let catalogs =
        CheckedNativeContracts::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."));
    let source = data()
        .as_array()
        .unwrap()
        .iter()
        .find(|group| {
            group
                .required("base")
                .unwrap()
                .required("status")
                .unwrap()
                .get("nativeVersion")
                == Some(&"0.163.0".into())
        })
        .unwrap();
    for mode in [
        "logical", "native", "lost", "pending", "new-work", "archived", "epoch", "unknown",
    ] {
        let mut base = source.required("base").unwrap().clone();
        let backend = Json::object([
            ("backendId", "codex".into()),
            ("provider", "codex".into()),
            ("accountId", "default".into()),
        ]);
        let mut run = base.required("run").unwrap().clone();
        run.get_mut("target")
            .unwrap()
            .insert("backend", backend.clone());
        run.get_mut("plan")
            .unwrap()
            .insert("backend", backend.clone());
        run.insert("phase", "completed".into());
        run.insert("result", pin(901, 1.0));
        run.insert("finishedAt", TIME.into());
        run.insert(
            "externalOutcome",
            Json::object([("state", "succeeded".into())]),
        );
        run.insert(
            "contextRelease",
            Json::object([
                ("state", "pending".into()),
                ("expectedOperationId", "last-turn".into()),
                ("commentCount", 0.0.into()),
                ("workRevision", 3.0.into()),
                ("call", Json::Null),
                ("code", Json::Null),
            ]),
        );
        let mut task = base.required("task").unwrap().clone();
        task.insert("claim", Json::Null);
        task.insert("primaryBackend", backend.clone());
        task.insert("workRevision", 3.0.into());
        task.insert("lastRun", pin(200, 2.0));
        task.insert("contextRelease", pin(200, 2.0));
        base.get_mut("status").unwrap().insert("backend", backend);
        let phases = match mode {
            "lost" => vec!["absent".into(), "succeeded".into()],
            "pending" => vec!["absent".into(), "absent".into()],
            "archived" => vec!["succeeded".into()],
            "unknown" => vec!["outcome_unknown".into()],
            _ => vec!["absent".into()],
        };
        let observation = if mode == "unknown" {
            Json::Object(vec![])
        } else if mode == "native" {
            Json::object([
                ("requestId", 42.0.into()),
                (
                    "reply",
                    Json::object([("result", Json::object([("status", "unsubscribed".into())]))]),
                ),
                (
                    "release",
                    Json::object([("kind", "native".into()), ("status", "unsubscribed".into())]),
                ),
            ])
        } else {
            Json::object([
                ("requestId", Json::Null),
                ("reply", Json::Null),
                (
                    "release",
                    Json::object([
                        ("kind", "logical".into()),
                        ("otherSubscribers", true.into()),
                    ]),
                ),
            ])
        };
        let mut input = Json::object([
            ("slot", "release".into()),
            ("operations", Json::Array(phases)),
            ("releaseResult", "succeeded".into()),
            ("observation", observation),
        ]);
        if mode == "lost" {
            input.insert(
                "fault",
                Json::object([("invoke", "outcome_unknown".into())]),
            );
        }
        if mode == "epoch" {
            input.insert("absence", Json::object([("epoch", "replacement".into())]));
            input.insert("status", Json::object([("epoch", "replacement".into())]));
        }
        let db = Connection::open_in_memory().unwrap();
        local_journal::initialize(&db).unwrap();
        let allowed = Rc::new(RefCell::new(HashSet::new()));
        let state = State {
            log: Arc::default(),
            fault: RefCell::new(Json::Null),
            scopes: RefCell::default(),
            identities: Cell::new(0),
            named: Cell::new(0),
            operation: Some("original".into()),
        };
        let next = Cell::new(300);
        let prepared_store = JournalStore {
            journal: LocalJournal {
                connection: &db,
                root_object_id: Some("root".into()),
            },
            client: Arc::new(NoRpc),
            contracts: Policy {
                allowed: allowed.clone(),
            },
            limits: HistoryLimits::default(),
            now: || TIME.to_owned(),
            new_id: || {
                let value = next.get();
                next.set(value + 1);
                id(value)
            },
            cache: None,
        };
        for revision in [1.0, 2.0] {
            seed(
                &prepared_store.journal,
                &allowed,
                "task-board/run",
                200,
                revision,
                &run,
            );
            seed(
                &prepared_store.journal,
                &allowed,
                "task-board/task",
                100,
                revision,
                &task,
            );
        }
        let prepared_trace = Trace {
            inner: &prepared_store,
            state: &state,
        };
        let preparer = NativeDriver {
            store: &prepared_trace,
            runtime: &state,
            prompts: &NoPrompts,
            catalogs: &catalogs,
        };
        if mode == "pending" {
            prepared_store
                .journal
                .create(
                    "task-board/operation",
                    &id(900),
                    Some(&"root".into()),
                    &ticket_operation("accepted"),
                    &TIME.into(),
                    &id(900),
                )
                .unwrap();
            assert_eq!(
                preparer
                    .prepare(
                        &id(200),
                        NativeSlot::Release,
                        Some(&Json::object([("kind", "release".into())]))
                    )
                    .await
                    .unwrap_err()
                    .code,
                "task_board_context_release_pending"
            );
            assert_eq!(state.identities.get(), 0);
            assert!(calls(&db).as_array().unwrap().is_empty());
            prepared_store
                .journal
                .update(
                    "task-board/operation",
                    &id(900),
                    1.0,
                    &ticket_operation("succeeded"),
                    &TIME.into(),
                )
                .unwrap();
        }
        let call_pin = preparer
            .prepare(
                &id(200),
                NativeSlot::Release,
                Some(&Json::object([("kind", "release".into())])),
            )
            .await
            .unwrap();
        let call = Store::read(&prepared_store, "task-board/native-call", &call_pin, false)
            .await
            .unwrap();
        assert_eq!(call.value.get("operationId"), Some(&Json::from("original")));
        run.get_mut("contextRelease")
            .unwrap()
            .insert("call", call_pin.clone());
        task.insert("lastRun", pin(200, 3.0));
        task.insert("contextRelease", pin(200, 3.0));
        if mode == "new-work" {
            task.insert("workRevision", 4.0.into());
        }
        seed(
            &prepared_store.journal,
            &allowed,
            "task-board/run",
            200,
            3.0,
            &run,
        );
        seed(
            &prepared_store.journal,
            &allowed,
            "task-board/task",
            100,
            3.0,
            &task,
        );
        base.get_mut("calls")
            .unwrap()
            .insert("release", call.value.clone());
        let client = Arc::new(Client::new(&input, &base, state.log.clone(), &catalogs));
        let store = JournalStore {
            journal: LocalJournal {
                connection: &db,
                root_object_id: Some("root".into()),
            },
            client: client.clone(),
            contracts: Policy {
                allowed: allowed.clone(),
            },
            limits: HistoryLimits::default(),
            now: || TIME.to_owned(),
            new_id: || {
                let value = next.get();
                next.set(value + 1);
                id(value)
            },
            cache: None,
        };
        let trace = Trace {
            inner: &store,
            state: &state,
        };
        let runtime = Runtime {
            state: &state,
            tools: Arc::new(ToolClients::new(client.clone())),
            principal: "workflow-owner".into(),
            owners: Cell::new(0),
            input: &input,
            changed: |_| Ok(()),
        };
        let driver = NativeDriver {
            store: &trace,
            runtime: &runtime,
            prompts: &NoPrompts,
            catalogs: &catalogs,
        };
        if mode == "archived" {
            state
                .fault
                .replace(Json::object([("releaseArchived", true.into())]));
        }
        if mode == "pending" {
            store
                .journal
                .update(
                    "task-board/operation",
                    &id(900),
                    2.0,
                    &ticket_operation("accepted"),
                    &TIME.into(),
                )
                .unwrap();
            assert_eq!(
                driver
                    .advance(&id(200), NativeSlot::Release)
                    .await
                    .unwrap_err()
                    .code,
                "task_board_context_release_pending"
            );
            assert_eq!(*client.invokes.lock().unwrap(), 0);
            assert!(
                Store::read(&store, "task-board/native-call", &call_pin, false)
                    .await
                    .unwrap()
                    .value
                    .required("ownerPhase")
                    .unwrap()
                    .is_null()
            );
            store
                .journal
                .update(
                    "task-board/operation",
                    &id(900),
                    3.0,
                    &ticket_operation("succeeded"),
                    &TIME.into(),
                )
                .unwrap();
        }
        state.log.lock().unwrap().clear();
        let result = driver.advance(&id(200), NativeSlot::Release).await;
        let retained = ["new-work", "epoch"].contains(&mode);
        if retained {
            assert_eq!(
                result.unwrap_err().code,
                "task_board_context_retained",
                "{mode}"
            );
            assert_eq!(*client.invokes.lock().unwrap(), 0);
            assert!(
                state
                    .log
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|event| event
                        .as_array()
                        .is_some_and(|a| a.first() == Some(&Json::from("update"))
                            && a[1].required("change").unwrap().get("kind")
                                == Some(&Json::from("retainContext"))))
            );
        } else {
            result.unwrap_or_else(|e| panic!("{mode}: {e:?}"));
            let latest = Store::read(
                &store,
                "task-board/native-call",
                call_pin.required("objectId").unwrap(),
                true,
            )
            .await
            .unwrap();
            assert_eq!(latest.pin.revision, 2.0, "{mode}");
            assert_eq!(
                latest.value.get("operationId"),
                Some(&Json::from("original"))
            );
            assert_eq!(latest.value.get("request"), call.value.get("request"));
            assert_eq!(
                latest.value.get("ownerPhase"),
                Some(&Json::from(if mode == "unknown" {
                    "outcome_unknown"
                } else {
                    "succeeded"
                }))
            );
            let identity = NativeIntent {
                store: &trace,
                catalogs: &catalogs,
            }
            .evidence_identity(&run, &latest.value, &"workflow-owner".into())
            .unwrap();
            let observed = NativeEvidence {
                store: &trace,
                catalogs: &catalogs,
            }
            .read(
                &latest.pin.to_json(),
                latest.value.required("evidence").unwrap(),
                &identity,
            )
            .await
            .unwrap();
            if mode == "native" {
                assert_eq!(observed.get("requestId"), Some(&42.0.into()));
                assert!(observed.required("reply").unwrap().get("result").is_some());
            } else if mode != "unknown" {
                assert!(observed.required("requestId").unwrap().is_null());
                assert!(observed.required("reply").unwrap().is_null());
            }
            let before = (
                *client.invokes.lock().unwrap(),
                *client.operations.lock().unwrap(),
            );
            let resumed = NativeDriver {
                store: &trace,
                runtime: &runtime,
                prompts: &NoPrompts,
                catalogs: &catalogs,
            };
            resumed
                .advance(&id(200), NativeSlot::Release)
                .await
                .unwrap();
            assert_eq!(
                (
                    *client.invokes.lock().unwrap(),
                    *client.operations.lock().unwrap()
                ),
                before,
                "Saved local observation is republished without another owner call: {mode}"
            );
            assert_eq!(
                before.0,
                u64::from(!["archived", "unknown"].contains(&mode))
            );
        }
        assert_eq!(
            state.identities.get(),
            1,
            "Advance never replaces the original identity: {mode}"
        );
        assert_eq!(
            hash_json(
                Store::read(&store, "task-board/task", &Json::String(id(100)), true)
                    .await
                    .unwrap()
                    .value
                    .required("primaryResourceRef")
                    .unwrap()
            )
            .unwrap(),
            hash_json(run.required("primaryResourceRef").unwrap()).unwrap(),
            "Primary identity is preserved independent of object key order: {mode}"
        );
        assert!(db.is_autocommit());
    }
}
