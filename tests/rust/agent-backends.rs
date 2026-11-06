use super::*;
use ivy_contracts::{
    hash::tool_definition_hash,
    native_backend::{BackendIdentity, NativeProvider},
    native_contract::NativeContract,
};
use ivy_sdk::service::ToolHandler;
use std::collections::HashMap;

fn identity(id: &str, provider: NativeProvider, account: &str) -> BackendIdentity {
    BackendIdentity::new(id.into(), provider, account.into()).unwrap()
}
fn context() -> InvocationContext {
    InvocationContext::detached(
        "caller".into(),
        Some("original-operation".into()),
        1,
        Default::default(),
        IvyError::new("cancelled", "Cancelled."),
    )
}
fn small_catalog() -> Arc<IvyJson> {
    let mut value = catalog("specs/native/codex-0.163.0/catalog.json");
    let mut method = field(&value, "clientRequests")
        .as_array()
        .unwrap()
        .iter()
        .find(|method| {
            method
                .get("method")
                .is_some_and(|method| method.is_str("model/list"))
        })
        .unwrap()
        .clone();
    method.insert("inputSchema", true.into());
    method.insert("outputSchema", true.into());
    value.insert("clientRequests", IvyJson::Array(vec![method]));
    for name in [
        "serverRequests",
        "serverNotifications",
        "clientNotifications",
        "derivedLegacyTypes",
    ] {
        value.insert(name, IvyJson::Array(vec![]));
    }
    Arc::new(value)
}

#[test]
fn backend_topics_preserve_published_payload_versions() {
    let published = parse(
        std::str::from_utf8(&reference_data::read(include_bytes!(
            "../fixtures/agent-published-topics-v1.json.gz"
        )))
        .unwrap(),
        BYTES,
    )
    .unwrap();
    let registry = ivy_agent_manager::registry::agent_registry(&small_catalog()).unwrap();
    let topics = field(&registry, "namespaces")
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|namespace| field(namespace, "topics").as_array().unwrap());
    let topics: Vec<_> = topics.collect();
    for old in published.as_array().unwrap() {
        let current = topics
            .iter()
            .find(|topic| topic.get("topic") == old.get("topic"))
            .unwrap();
        ivy_contracts::schema::admit_schema(field(current, "payloadSchema")).unwrap();
        if current.get("version") == old.get("version") {
            same(field(current, "payloadSchema"), field(old, "payloadSchema"));
        } else {
            assert_eq!(current.get("version"), Some(&"1.1.0".into()));
            let mut previous = field(current, "payloadSchema").clone();
            let IvyJson::Object(fields) = &mut previous else {
                unreachable!()
            };
            for (name, value) in fields {
                if name.eq_str("properties") {
                    assert!(value.remove("backend").is_some());
                } else if name.eq_str("$defs") {
                    assert!(value.remove("AgentBackendIdentity").is_some());
                }
            }
            same(&previous, field(old, "payloadSchema"));
        }
    }
}

fn route(
    root: &Root,
    catalog: Arc<IvyJson>,
    backend: BackendIdentity,
    visits: Arc<AtomicUsize>,
) -> (BackendRoute, NativeRpc) {
    let journal = Arc::new(Mutex::new(
        NativeJournal::new_scoped(
            Owner {
                service_node_id: "fixture.agent".into(),
                native_version: text(&catalog, "version"),
                native_executable_hash: text(&catalog, "nativeExecutableHash"),
            },
            Limits {
                max_operations: 100,
                max_journal_bytes: 64 * 1024 * 1024,
                max_pending_inputs: 8,
            },
            Some(backend.clone()),
        )
        .unwrap(),
    ));
    journal
        .lock()
        .unwrap()
        .begin_epoch(&"epoch".into())
        .unwrap();
    let state = local(root);
    state
        .lock()
        .unwrap()
        .bind_backend(backend.provider(), Some(&backend))
        .unwrap();
    let operations =
        NativeOperations::new(Arc::new(SqliteOperationStore::new(state).unwrap()), journal);
    let (rpc, mut requests, replies) =
        NativeRpc::start_frames(catalog.clone(), Events::default()).unwrap();
    let id = backend.id().to_owned();
    tokio::spawn(async move {
        while let Some(frame) = requests.recv().await {
            visits.fetch_add(1, Ordering::SeqCst);
            replies
                .try_send(IvyJson::object([
                    ("id", frame.required("id").unwrap().clone()),
                    (
                        "result",
                        IvyJson::object([
                            ("owner", id.clone().into()),
                            ("echo", frame.required("params").unwrap().clone()),
                        ]),
                    ),
                ]))
                .unwrap();
        }
    });
    let management = NativeManagement::new(
        NativeManagementOwner {
            backend: Some(backend.clone()),
            service_node_id: "fixture.agent".into(),
            epoch: "epoch".into(),
            catalog: catalog.clone(),
            rpc: rpc.clone(),
            closing: Arc::new(AtomicBool::new(false)),
            platform: "fixture".into(),
        },
        operations,
        NativeInteractions::new("epoch".into(), InteractionLimits::default()),
    )
    .unwrap();
    let registry = management.registry().clone();
    let work = AgentWork::default();
    let mut handlers = management
        .handlers()
        .into_iter()
        .map(|(name, handler)| {
            let handler = if name.eq_str("agent.interact") {
                work.handler(handler)
            } else {
                handler
            };
            (name, handler)
        })
        .collect::<std::collections::HashMap<_, _>>();
    let status_rpc = rpc.clone();
    let mut status = IvyJson::object([
        ("serviceNodeId", "fixture.agent".into()),
        ("hostId", "fixture".into()),
        ("nativeVersion", field(&catalog, "version").clone()),
        (
            "nativeExecutableHash",
            field(&catalog, "nativeExecutableHash").clone(),
        ),
        ("catalogHash", hash_json(&catalog).unwrap().into()),
        ("epoch", "epoch".into()),
        ("pid", IvyJson::Null),
        ("state", "ready".into()),
        ("observedAt", "2026-11-01T00:00:00.000Z".into()),
        ("code", IvyJson::Null),
        ("initialized", IvyJson::Null),
        ("pendingInputs", IvyJson::Number(0.0)),
        (
            "operations",
            IvyJson::object([
                ("retained", IvyJson::Number(0.0)),
                ("maximum", IvyJson::Number(100.0)),
                ("bytes", IvyJson::Number(0.0)),
                ("maximumBytes", IvyJson::Number(1048576.0)),
            ]),
        ),
        ("observedMethods", IvyJson::Array(vec![])),
    ]);
    status.insert("serverType", backend.provider().as_str().into());
    let status_handler: ToolHandler = Arc::new(move |_, _| {
        let mut value = status.clone();
        value.insert(
            "state",
            if status_rpc.connected() {
                "ready"
            } else {
                "failed"
            }
            .into(),
        );
        Box::pin(async move { Ok(value) })
    });
    handlers.insert("agent.status".into(), status_handler);
    for namespace in field(&registry, "namespaces").as_array().unwrap() {
        if namespace
            .get("namespace")
            .is_some_and(|name| name.is_str("agent"))
        {
            for definition in field(namespace, "tools").as_array().unwrap() {
                let name = format!("agent.{}", field(definition, "name").as_str().unwrap()).into();
                handlers.entry(name).or_insert_with(|| {
                    Arc::new(|_, _| Box::pin(async { Ok(IvyJson::object::<&str>([])) }))
                });
            }
        }
    }
    (
        BackendRoute {
            preparation_failure: None,
            identity: Some(backend),
            registry,
            handlers: ivy_agent_manager::service_kernel::HandlerSet {
                management: handlers,
                native: management.json_handlers(),
            },
        },
        rpc,
    )
}

#[tokio::test]
async fn equal_catalogs_and_operation_ids_route_only_to_the_selected_account() {
    let a_root = Root::new();
    let b_root = Root::new();
    let catalog = small_catalog();
    let a = identity("codex", NativeProvider::Codex, "work");
    let b = identity("claude", NativeProvider::Claude, "personal");
    let visits_a = Arc::new(AtomicUsize::new(0));
    let visits_b = Arc::new(AtomicUsize::new(0));
    let (route_a, rpc_a) = route(&a_root, catalog.clone(), a.clone(), visits_a.clone());
    let (route_b, rpc_b) = route(&b_root, catalog.clone(), b.clone(), visits_b.clone());
    let routes = BackendRoutes::new(vec![route_a, route_b]).unwrap();
    let handlers = routes.handlers();
    let namespaces: Vec<_> = field(routes.registry(), "namespaces")
        .as_array()
        .unwrap()
        .iter()
        .map(|n| text(n, "namespace"))
        .collect();
    assert_eq!(
        namespaces,
        vec![JsString::from("codex"), "agent".into(), "claude".into()]
    );
    assert!(handlers.native.contains_key(&"codex.model/list".into()));
    assert!(handlers.native.contains_key(&"claude.model/list".into()));
    let native_params = parse(
        r#"{"model":"same-model","backend":{"opaque":"\ud800"}}"#,
        1000,
    )
    .unwrap();
    let call = handlers
        .management
        .get(&JsString::from("agent.interact"))
        .unwrap();
    for backend in [&a, &b] {
        let contract = NativeContract::for_backend(&catalog, Some(backend.clone())).unwrap();
        let args = backend
            .arguments(IvyJson::object([
                ("operationId", "original-operation".into()),
                ("nativeVersion", field(&catalog, "version").clone()),
                ("method", "model/list".into()),
                (
                    "expectedDefinitionHash",
                    contract
                        .definition_hash(&"model/list".into())
                        .unwrap()
                        .into(),
                ),
                ("params", native_params.clone()),
            ]))
            .unwrap();
        let result = if backend == &a {
            drop(call(args, context()));
            tokio::time::timeout(Duration::from_secs(2), async {
                while visits_a.load(Ordering::SeqCst) == 0 {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            handlers.management[&JsString::from("agent.interaction")](
                backend
                    .arguments(IvyJson::object([(
                        "operationId",
                        "original-operation".into(),
                    )]))
                    .unwrap(),
                context(),
            )
            .await
            .unwrap()
        } else {
            call(args, context()).await.unwrap()
        };
        assert!(BackendIdentity::matches(&result, Some(backend)));
        assert_eq!(
            result
                .required("reply")
                .unwrap()
                .required("result")
                .unwrap()
                .get("owner"),
            Some(&backend.id().into())
        );
        assert_eq!(
            result
                .required("reply")
                .unwrap()
                .required("result")
                .unwrap()
                .get("echo"),
            Some(&native_params)
        );
        // Retry only observes the original in-memory result, independently per backend.
        let cached = handlers.management[&JsString::from("agent.interaction")](
            backend
                .arguments(IvyJson::object([(
                    "operationId",
                    "original-operation".into(),
                )]))
                .unwrap(),
            context(),
        )
        .await
        .unwrap();
        assert_eq!(cached, result);
    }
    assert_eq!(visits_a.load(Ordering::SeqCst), 1);
    assert_eq!(visits_b.load(Ordering::SeqCst), 1);
    let discover = handlers.management[&JsString::from("agent.discover")](
        b.arguments(IvyJson::object([("method", "model/list".into())]))
            .unwrap(),
        context(),
    )
    .await
    .unwrap();
    let contract = NativeContract::for_backend(&catalog, Some(b.clone())).unwrap();
    assert_eq!(
        field(
            field(&discover, "items")
                .as_array()
                .unwrap()
                .first()
                .unwrap(),
            "expectedDefinitionHash"
        )
        .as_str()
        .unwrap(),
        contract.definition_hash(&"model/list".into()).unwrap()
    );
    assert_eq!(
        tool_definition_hash(&contract.definition(&"model/list".into()).unwrap()).unwrap(),
        contract.definition_hash(&"model/list".into()).unwrap()
    );
    rpc_a.close("fixture_closed").await;
    let backends = handlers.management[&JsString::from("agent.backends")](
        IvyJson::object::<&str>([]),
        context(),
    )
    .await
    .unwrap();
    ivy_contracts::agent_schema::validate_agent("BackendsResult", &backends).unwrap();
    let states = field(&backends, "backends").as_array().unwrap();
    assert_eq!(states[0].get("state"), Some(&"failed".into()));
    assert_eq!(states[1].get("state"), Some(&"ready".into()));
    let status = handlers.management[&JsString::from("agent.status")](
        b.arguments(IvyJson::object::<&str>([])).unwrap(),
        context(),
    )
    .await
    .unwrap();
    assert!(BackendIdentity::matches(&status, Some(&b)));
    // Both origins remain readable after one native engine disconnects. The
    // protocol catalog's provider label cannot substitute for stored provenance.
    for backend in [&a, &b] {
        let origin = handlers.management[&JsString::from("agent.storageIdentity")](
            backend.arguments(IvyJson::object::<&str>([])).unwrap(),
            context(),
        )
        .await
        .unwrap();
        ivy_contracts::agent_schema::validate_agent("StorageIdentity", &origin).unwrap();
        assert!(BackendIdentity::matches(&origin, Some(backend)));
        assert_eq!(origin.get("serviceNodeId"), Some(&"fixture.agent".into()));
        assert_eq!(
            origin.get("originalServiceNodeId"),
            Some(&"fixture.agent".into())
        );
        assert_eq!(
            origin.get("provider"),
            Some(&backend.provider().as_str().into())
        );
    }
    assert_eq!(visits_a.load(Ordering::SeqCst), 1);
    assert_eq!(visits_b.load(Ordering::SeqCst), 1);
    rpc_b.close("fixture_closed").await;
}

#[tokio::test]
async fn absent_stale_and_malformed_selections_are_rejected_before_dispatch() {
    let root = Root::new();
    let visits = Arc::new(AtomicUsize::new(0));
    let backend = identity("claude", NativeProvider::Claude, "work");
    let (route, rpc) = route(&root, small_catalog(), backend.clone(), visits.clone());
    let handlers = BackendRoutes::new(vec![route]).unwrap().handlers();
    for bad in [
        IvyJson::object::<&str>([]),
        IvyJson::object([("backend", IvyJson::Null)]),
        identity("claude", NativeProvider::Claude, "other")
            .arguments(IvyJson::object::<&str>([]))
            .unwrap(),
        identity("claude", NativeProvider::Codex, "work")
            .arguments(IvyJson::object::<&str>([]))
            .unwrap(),
        identity("missing", NativeProvider::Claude, "work")
            .arguments(IvyJson::object::<&str>([]))
            .unwrap(),
    ] {
        for method in ["agent.interact", "agent.storageIdentity"] {
            let error = handlers.management[&JsString::from(method)](bad.clone(), context())
                .await
                .unwrap_err();
            assert_eq!(error.outcome, ivy_contracts::Outcome::NotExecuted);
        }
    }
    assert_eq!(visits.load(Ordering::SeqCst), 0);
    assert!(
        BackendIdentity::select_arguments(
            None,
            backend.arguments(IvyJson::object::<&str>([])).unwrap()
        )
        .is_err()
    );
    rpc.close("fixture_closed").await;
}

#[test]
fn persisted_account_identity_cannot_be_removed_or_reassigned_after_reopen() {
    let root = Root::new();
    let backend = identity("codex", NativeProvider::Codex, "work");
    let path = root.0.join("state.sqlite");
    let open = || {
        LocalAgentState::open(
            &path,
            "fixture.agent".into(),
            StateLimits {
                max_operations: 100,
                max_journal_bytes: 64 * 1024 * 1024,
            },
        )
        .unwrap()
    };
    let mut state = open();
    state
        .bind_backend(NativeProvider::Codex, Some(&backend))
        .unwrap();
    state.close().unwrap();
    let mut state = open();
    state
        .bind_backend(NativeProvider::Codex, Some(&backend))
        .unwrap();
    for selected in [
        None,
        Some(identity("codex", NativeProvider::Codex, "personal")),
        Some(identity("codex", NativeProvider::Claude, "work")),
    ] {
        assert_eq!(
            state
                .bind_backend(NativeProvider::Codex, selected.as_ref())
                .unwrap_err()
                .code,
            "native_backend_mismatch"
        );
    }
    state
        .bind_backend(NativeProvider::Codex, Some(&backend))
        .unwrap();
    state.close().unwrap();
}

#[tokio::test]
async fn moving_the_route_preserves_storage_pins_and_original_receipts_without_replay() {
    use ivy_agent_manager::native_journal::Identity;
    let root = Root::new();
    let backend = identity("claude", NativeProvider::Claude, "work");
    let selected = small_catalog();
    let path = root.0.join("journal/agent-runtime.sqlite");
    let open = |node: &str| {
        Arc::new(Mutex::new(
            LocalAgentState::open(
                &path,
                node.into(),
                StateLimits {
                    max_operations: 100,
                    max_journal_bytes: 64 * 1024 * 1024,
                },
            )
            .unwrap(),
        ))
    };
    let old = open("old.agent");
    old.lock()
        .unwrap()
        .set_runtime_epoch("original-hive".into())
        .unwrap();
    let original_journal = Arc::new(Mutex::new(
        NativeJournal::new(
            Owner {
                service_node_id: "old.agent".into(),
                native_version: text(&selected, "version"),
                native_executable_hash: text(&selected, "nativeExecutableHash"),
            },
            Limits {
                max_operations: 100,
                max_journal_bytes: 64 * 1024 * 1024,
                max_pending_inputs: 8,
            },
        )
        .unwrap(),
    ));
    let terminal_id = Identity {
        caller_principal_id: "caller".into(),
        operation_id: "original-hive:1:terminal".into(),
    };
    let pending_id = Identity {
        caller_principal_id: "caller".into(),
        operation_id: "original-hive:1:pending".into(),
    };
    let params = parse(r#"{"unknown":"\ud800"}"#, 1000).unwrap();
    let original = original_journal
        .lock()
        .unwrap()
        .prevent(&terminal_id, &"model/list".into(), &params)
        .unwrap();
    let mut pending = original_journal
        .lock()
        .unwrap()
        .accept(&pending_id, &"model/list".into(), &params)
        .unwrap()
        .0;
    pending.insert("phase", "dispatched".into());
    pending.insert("epoch", "original-epoch".into());
    pending.insert("requestId", "original-native-request".into());
    let key = |id: &Identity| hash_json(&id.value()).unwrap();
    let terminal_pin = old
        .lock()
        .unwrap()
        .write("operation", &key(&terminal_id), &original, None)
        .unwrap()
        .pin;
    let pending_pin = old
        .lock()
        .unwrap()
        .write("operation", &key(&pending_id), &pending, None)
        .unwrap()
        .pin;
    let unseeded = NativeOperations::new(
        Arc::new(SqliteOperationStore::new(old.clone()).unwrap()),
        original_journal.clone(),
    );
    let error = unseeded.storage_identity().unwrap_err();
    assert_eq!(error.code, "native_state_migration_required");
    assert_eq!(error.outcome, ivy_contracts::Outcome::NotExecuted);
    drop(unseeded);
    old.lock().unwrap().close_shared().unwrap();
    drop(old);

    // An existing installation from before the storage-owner metadata must be
    // prepared under its real owner, rather than inferred from its new route.
    {
        let db = ivy_store::rusqlite::Connection::open(&path).unwrap();
        db.execute("DELETE FROM state_metadata WHERE key='storage_owner'", [])
            .unwrap();
    }
    let refused = LocalAgentState::open(
        &path,
        "fixture.agent".into(),
        StateLimits {
            max_operations: 100,
            max_journal_bytes: 64 * 1024 * 1024,
        },
    );
    assert_eq!(
        refused.err().unwrap().code,
        "native_state_migration_required"
    );
    {
        let db = ivy_store::rusqlite::Connection::open(&path).unwrap();
        let count: i64 = db
            .query_row(
                "SELECT count(*) FROM state_metadata WHERE key='storage_owner'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 0, "Rejected migration must not seed a new owner");
    }
    let seeded = open("old.agent");
    seeded
        .lock()
        .unwrap()
        .bind_backend(NativeProvider::Claude, None)
        .unwrap();
    let origin = NativeOperations::new(
        Arc::new(SqliteOperationStore::new(seeded.clone()).unwrap()),
        original_journal,
    )
    .storage_identity()
    .unwrap();
    same(
        &origin,
        &IvyJson::object([
            ("serviceNodeId", "old.agent".into()),
            ("originalServiceNodeId", "old.agent".into()),
            ("provider", "claude".into()),
        ]),
    );
    seeded.lock().unwrap().close_shared().unwrap();
    drop(seeded);

    let state = open("fixture.agent");
    state
        .lock()
        .unwrap()
        .set_runtime_epoch("current-hive".into())
        .unwrap();
    state
        .lock()
        .unwrap()
        .bind_backend(NativeProvider::Claude, Some(&backend))
        .unwrap();
    for (id, value, pin) in [
        (&terminal_id, &original, &terminal_pin),
        (&pending_id, &pending, &pending_pin),
    ] {
        let saved = state
            .lock()
            .unwrap()
            .read("operation", &key(id))
            .unwrap()
            .unwrap();
        same(&saved.value, value);
        assert_eq!(&saved.pin, pin);
    }
    let current_journal = Arc::new(Mutex::new(
        NativeJournal::new_scoped(
            Owner {
                service_node_id: "fixture.agent".into(),
                native_version: text(&selected, "version"),
                native_executable_hash: text(&selected, "nativeExecutableHash"),
            },
            Limits {
                max_operations: 100,
                max_journal_bytes: 64 * 1024 * 1024,
                max_pending_inputs: 8,
            },
            Some(backend.clone()),
        )
        .unwrap(),
    ));
    let operations = NativeOperations::new(
        Arc::new(SqliteOperationStore::new(state.clone()).unwrap()),
        current_journal,
    );
    let mut moved_origin = origin;
    moved_origin.insert("serviceNodeId", "fixture.agent".into());
    same(&operations.storage_identity().unwrap(), &moved_origin);
    same(
        &operations.read(&terminal_id).await.unwrap().unwrap(),
        &original,
    );
    let unknown = operations.read(&pending_id).await.unwrap().unwrap();
    let mut expected_unknown = pending.clone();
    expected_unknown.insert("phase", "outcome_unknown".into());
    expected_unknown.insert("code", "native_connection_lost".into());
    same(&unknown, &expected_unknown);
    // Duplicate invoke and prevention return the saved owner; no provider action.
    for (id, expected) in [(&terminal_id, &original), (&pending_id, &expected_unknown)] {
        let receipt = operations
            .run(
                id.clone(),
                "model/list".into(),
                params.clone(),
                |_| async { panic!("Migration must never replay an original operation") },
                false,
            )
            .unwrap()
            .wait()
            .await
            .unwrap();
        same(&receipt, expected);
    }
    let observe = operations.clone();
    let handler: ToolHandler = Arc::new(move |args, ctx| {
        let observe = observe.clone();
        Box::pin(async move {
            let id = Identity {
                caller_principal_id: ctx.caller_principal_id,
                operation_id: text(&args, "operationId"),
            };
            Ok(observe.read(&id).await?.unwrap())
        })
    });
    let routes = BackendRoutes::new(vec![BackendRoute {
        preparation_failure: None,
        identity: Some(backend.clone()),
        registry: ivy_agent_manager::registry::backend_registry(&selected, Some(&backend)).unwrap(),
        handlers: ivy_agent_manager::service_kernel::HandlerSet {
            management: HashMap::from([("agent.operation".into(), handler)]),
            native: HashMap::new(),
        },
    }])
    .unwrap();
    let observed = routes.handlers().management[&JsString::from("agent.operation")](
        backend
            .arguments(IvyJson::object([(
                "operationId",
                IvyJson::String(terminal_id.operation_id.clone()),
            )]))
            .unwrap(),
        context(),
    )
    .await
    .unwrap();
    same(&observed, &original);
    let current_id = Identity {
        caller_principal_id: "caller".into(),
        operation_id: "current-hive:2:new".into(),
    };
    let new = operations
        .run(
            current_id.clone(),
            "model/list".into(),
            params.clone(),
            |_| async { unreachable!() },
            true,
        )
        .unwrap()
        .wait()
        .await
        .unwrap();
    assert!(BackendIdentity::matches(&new, Some(&backend)));
    assert_eq!(new.get("serviceNodeId"), Some(&"fixture.agent".into()));
    same(&operations.read(&current_id).await.unwrap().unwrap(), &new);
    // Foreign owner/account data cannot acquire authority by sharing an operation ID.
    let store = SqliteOperationStore::new(state.clone()).unwrap();
    use ivy_agent_manager::native_operations::NativeOperationStore;
    let mut forged = original.clone();
    forged.insert("serviceNodeId", "foreign.agent".into());
    assert!(!store.owns_receipt(&forged));
    let mut wrong_account = original.clone();
    wrong_account.insert(
        "backend",
        identity("claude", NativeProvider::Claude, "personal").json(),
    );
    assert!(!store.owns_receipt(&wrong_account));
    let saved = state
        .lock()
        .unwrap()
        .read("operation", &key(&pending_id))
        .unwrap()
        .unwrap();
    same(&saved.value, &pending);
    assert_eq!(saved.pin, pending_pin);
    state.lock().unwrap().close_shared().unwrap();
}
