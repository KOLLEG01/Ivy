use super::*;
use ivy_contracts::builtin_schema::SchemaBundle;
use ivy_store::rusqlite::Connection;
use ivy_task_board::{
    journal_store::{Contracts, JournalStore, ReadCache},
    lifecycle_journal::{LifecycleJournal, LifecyclePages},
    local_journal::{self, LocalJournal},
    local_retention::HistoryLimits,
    native_lifecycle::LifecycleRuntime,
    operations::Operations,
    publication::{Publication, TaskWritten},
};
use std::{
    future::Future,
    pin::Pin,
    sync::atomic::{AtomicBool, Ordering},
};
fn validate(name: &str, v: &Json) -> IvyResult<()> {
    static B: OnceLock<SchemaBundle> = OnceLock::new();
    let b = B.get_or_init(|| {
        SchemaBundle::new(&[
            include_str!("../../specs/schemas/task-board.schema.json"),
            include_str!("../../specs/schemas/chat.schema.json"),
            include_str!("../../specs/schemas/agent.schema.json"),
            include_str!("../../specs/schemas/hive-wire.schema.json"),
        ])
        .unwrap()
    });
    if b.matches(
        &format!("https://ivy.invalid/schemas/task-board.schema.json#/$defs/{name}"),
        v,
    )? {
        Ok(())
    } else {
        Err(IvyError::new(
            "invalid_arguments",
            format!("Fixture {name} admission failed."),
        ))
    }
}
struct Policy {
    task: Json,
    local_task: bool,
}
impl Contracts for Policy {
    fn version(&self, _: &str) -> IvyResult<String> {
        Ok("1.0.0".into())
    }
    fn local(&self, k: &str) -> IvyResult<bool> {
        Ok(k != "task-board/task" || self.local_task)
    }
    fn readable_version(&self, _: &str, v: &JsString) -> IvyResult<bool> {
        Ok(v == &JsString::from("1.0.0"))
    }
    fn validate_record(&self, k: &str, _: &JsString, v: &Json) -> IvyResult<()> {
        match k {
            "task-board/operation" => validate("Operation", v),
            "task-board/history" => validate("History", v),
            "task-board/task" => {
                // The normative full current Task overlay remains an integration gate.
                // Admit this fixture's unchanged static fields and finite lifecycle projections.
                let dynamic = [
                    "primaryResourceRef",
                    "publication",
                    "updatedAt",
                    "lastHistory",
                    "workflowState",
                    "waiting",
                    "acceptedReview",
                    "fields",
                ];
                let mut a = v.clone();
                let mut b = self.task.clone();
                for k in dynamic {
                    a.remove(k);
                    b.remove(k);
                }
                assert_eq!(hash_json(&a)?, hash_json(&b)?);
                assert!(
                    [Json::from("waiting"), Json::from("todo")].contains(f(v, "workflowState"))
                );
                assert_eq!(f(f(v, "fields"), "control"), &Json::from("agent"));
                Ok(())
            }
            _ => panic!("Unexpected real journal contract {k}"),
        }
    }
    fn validate_definition(&self, n: &str, v: &Json) -> IvyResult<()> {
        validate(n, v)
    }
}
struct Client {
    owner_valid: AtomicBool,
    trace: Mutex<Vec<Json>>,
    remote: Mutex<Option<Json>>,
}
impl RpcClient for Client {
    fn request<'a>(&'a self, m: &'a str, a: Json, _: RequestOptions) -> RequestFuture<'a> {
        Box::pin(async move {
            self.trace
                .lock()
                .unwrap()
                .push(Json::Array(vec![m.into(), a.clone()]));
            match m {
                "system.status" => Ok(Json::object([("runtimeEpoch", "epoch".into())])),
                "serviceNodes.get" => {
                    assert_eq!(a.required("serviceNodeId")?, &Json::from("task-board-node"));
                    Ok(Json::object([
                        (
                            "principalId",
                            if self.owner_valid.load(Ordering::Relaxed) {
                                "caller"
                            } else {
                                "replacement"
                            }
                            .into(),
                        ),
                        ("serviceName", "task-board".into()),
                        ("connected", true.into()),
                        ("synced", true.into()),
                    ]))
                }
                "objects.read" => Ok(self
                    .remote
                    .lock()
                    .unwrap()
                    .as_ref()
                    .expect("Unexpected remote read")
                    .clone()),
                other => panic!("Retained terminal lifecycle replay must not call {other}: {a:?}"),
            }
        })
    }
}
struct Pages;
impl LifecyclePages for Pages {
    async fn page(&self, k: &str, o: &Json) -> IvyResult<Json> {
        assert!(["task-board/run", "task-board/history"].contains(&k));
        assert_eq!(num(o, "limit"), 100.0);
        Ok(Json::object([
            ("items", Json::Array(vec![])),
            ("nextCursor", Json::Null),
        ]))
    }
}
struct Policies;
impl LifecyclePolicies for Policies {
    fn scheduler_enabled(&self) -> bool {
        true
    }
    async fn aggregate(&self, t: &Json, id: &JsString) -> IvyResult<()> {
        assert_eq!(id, &super::id(100));
        assert_eq!(f(t, "claim"), &Json::Null);
        Ok(())
    }
    async fn allocate(&self, _: &Document, _: &JsString) -> IvyResult<Json> {
        panic!("Retained call cannot allocate again")
    }
    async fn unstarted_missing_context(&self, _: &Document) -> IvyResult<bool> {
        panic!("Explicit new context is not recovery")
    }
}
fn clock() -> String {
    "2026-01-02T03:04:05.006Z".to_owned()
}
fn fresh_id() -> JsString {
    uuid::Uuid::new_v4().to_string().into()
}
#[tokio::test(flavor = "current_thread")]
async fn retained_lifecycle_survives_actual_partial_publication_reopen_and_replacement_owner() {
    struct Temp(std::path::PathBuf);
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    let temp = Temp(std::env::temp_dir().join(format!(
        "task-board-lifecycle-{}.sqlite",
        uuid::Uuid::new_v4()
    )));
    let group = data().as_array().unwrap().last().unwrap();
    let row = group
        .required("rows")
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .find(|r| {
            s(r, "name")
                == &JsString::from("retained terminal call avoids preparation and inspection")
        })
        .unwrap();
    let task = row
        .required("initial")
        .unwrap()
        .required("task")
        .unwrap()
        .required("value")
        .unwrap();
    let call = &row
        .required("initial")
        .unwrap()
        .required("liveOperation")
        .unwrap()
        .required("value")
        .unwrap()
        .required("nativeCalls")
        .unwrap()
        .as_array()
        .unwrap()[0];
    let catalogs =
        CheckedNativeContracts::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."));
    let mut operation_pin = None;
    let mut expected = None;
    for round in 0..2 {
        let db = Connection::open(&temp.0).unwrap();
        local_journal::initialize(&db).unwrap();
        let client = Arc::new(Client {
            owner_valid: AtomicBool::new(true),
            trace: Mutex::default(),
            remote: Mutex::new(None),
        });
        let cache = ReadCache::default();
        let store = JournalStore {
            journal: LocalJournal {
                connection: &db,
                root_object_id: Some("root".into()),
            },
            client: client.clone(),
            contracts: Policy {
                task: task.clone(),
                local_task: true,
            },
            limits: HistoryLimits::default(),
            now: clock as fn() -> String,
            new_id: fresh_id as fn() -> JsString,
            cache: Some(&cache),
        };
        let pages = Pages;
        let operations = Operations {
            store: LifecycleJournal {
                journal: &store,
                pages: &pages,
            },
            now: clock as fn() -> String,
        };
        let engine = LifecycleRuntime::new(
            client.clone(),
            "caller".into(),
            "task-board-node".into(),
            Policies,
        );
        engine.verify_owner().await.unwrap();
        if round == 0 {
            store
                .journal
                .create(
                    "task-board/task",
                    &id(100),
                    Some(&"root".into()),
                    task,
                    &clock().into(),
                    &id(100),
                )
                .unwrap();
            store
                .journal
                .update("task-board/task", &id(100), 1.0, task, &clock().into())
                .unwrap();
            let actor = Json::object([
                ("principalId", "caller".into()),
                ("serviceNodeId", "task-board-node".into()),
                ("generation", Json::Number(1.0)),
                ("source", "user".into()),
            ]);
            let request = Json::object([
                ("action", "newThread".into()),
                ("taskId", Json::String(id(100))),
                ("expectedRevision", Json::Number(2.0)),
                ("operationId", "retained-action".into()),
                ("reason", "requested".into()),
            ]);
            let begun = operations.begin(&request, &actor).await.unwrap();
            operation_pin = Some(begun.pin.clone());
            let mut proposed = call.clone();
            proposed.insert("observation", Json::Null);
            operations
                .prepare_native(s(&begun.pin, "objectId"), &proposed)
                .await
                .unwrap();
            operations
                .observe_native(
                    s(&begun.pin, "objectId"),
                    s(call, "key"),
                    call.required("observation").unwrap(),
                )
                .await
                .unwrap();
            let operation = Store::read(
                &operations.store,
                "task-board/operation",
                begun.pin.required("objectId").unwrap(),
                true,
            )
            .await
            .unwrap();
            let subject = NativeLifecycle {
                store: &operations.store,
                engine: &engine,
                operations: &operations,
                catalogs: &catalogs,
            };
            let fail = Cell::new(true);
            let callback =
                |_: Json, _: Json| -> Pin<Box<dyn Future<Output = IvyResult<()>> + '_>> {
                    let fail = fail.replace(false);
                    Box::pin(async move {
                        if fail {
                            Err(IvyError::new(
                                "fixture_failed",
                                "Retained Task callback failure.",
                            ))
                        } else {
                            Ok(())
                        }
                    })
                };
            let mut steps = Publication {
                operations: &operations,
                operation: &operation,
                task_written: Some(&callback as &TaskWritten<'_>),
                index: 0.0,
            };
            let initial = Json::object([
                ("task", Json::Null),
                ("history", Json::Null),
                ("archive", Json::Null),
            ]);
            let failure = subject
                .publish(&operation, &mut steps, initial.clone())
                .await
                .err()
                .unwrap();
            assert_eq!(failure.code, "fixture_failed", "{failure:?}");
            let current = store
                .journal
                .read("task-board/task", &Json::String(id(100)), true)
                .unwrap();
            assert_eq!(num(&current.pin, "revision"), 3.0);
            let operation = Store::read(
                &operations.store,
                "task-board/operation",
                begun.pin.required("objectId").unwrap(),
                true,
            )
            .await
            .unwrap();
            assert_eq!(
                operation
                    .value
                    .required("writes")
                    .unwrap()
                    .as_array()
                    .unwrap()
                    .len(),
                1
            );
            let mut steps = Publication {
                operations: &operations,
                operation: &operation,
                task_written: Some(&callback as &TaskWritten<'_>),
                index: 0.0,
            };
            let result = subject
                .publish(&operation, &mut steps, initial)
                .await
                .unwrap();
            assert_eq!(num(result.required("task").unwrap(), "revision"), 4.0);
            let current = store
                .journal
                .read("task-board/task", &Json::String(id(100)), true)
                .unwrap();
            assert_eq!(f(&current.value, "workflowState"), &Json::from("todo"));
            assert_eq!(f(&current.value, "publication"), &Json::Null);
            assert_eq!(
                f(&current.value, "commentDeliveries"),
                f(task, "commentDeliveries")
            );
            expected = Some(result);
        } else {
            let operation = Store::read(
                &operations.store,
                "task-board/operation",
                operation_pin
                    .as_ref()
                    .unwrap()
                    .required("objectId")
                    .unwrap(),
                true,
            )
            .await
            .unwrap();
            let before = db.total_changes();
            let subject = NativeLifecycle {
                store: &operations.store,
                engine: &engine,
                operations: &operations,
                catalogs: &catalogs,
            };
            let mut steps = Publication {
                operations: &operations,
                operation: &operation,
                task_written: None,
                index: 0.0,
            };
            let result = subject
                .publish(
                    &operation,
                    &mut steps,
                    Json::object([
                        ("task", Json::Null),
                        ("history", Json::Null),
                        ("archive", Json::Null),
                    ]),
                )
                .await
                .unwrap();
            assert_eq!(result, expected.clone().unwrap());
            assert_eq!(db.total_changes(), before);
            client.owner_valid.store(false, Ordering::Relaxed);
            assert_eq!(
                engine.verify_owner().await.err().unwrap().code,
                "task_board_principal_mismatch"
            );
            assert_eq!(db.total_changes(), before);
        }
        assert!(
            !client
                .trace
                .lock()
                .unwrap()
                .iter()
                .any(|v| v.as_array().unwrap()[0] == Json::from("tools.call"))
        );
        assert!(db.is_autocommit());
    }
}
#[tokio::test(flavor = "current_thread")]
async fn lifecycle_metadata_preserves_current_head_archive_flags_cache_and_exact_pin() {
    let group = data().as_array().unwrap().last().unwrap();
    let task = group
        .required("base")
        .unwrap()
        .required("task")
        .unwrap()
        .clone();
    let db = Connection::open_in_memory().unwrap();
    local_journal::initialize(&db).unwrap();
    let before = db.total_changes();
    let object = Json::object([
        ("id", Json::String(id(100))),
        ("parentId", "root".into()),
        ("contractKey", "task-board/task".into()),
        ("currentRevision", Json::Number(7.0)),
        ("archivedAt", "2026-01-02T03:04:05.006Z".into()),
        ("effectivelyArchived", true.into()),
    ]);
    let reply = Json::object([
        ("object", object.clone()),
        (
            "revision",
            Json::object([
                ("objectId", Json::String(id(100))),
                ("revision", Json::Number(2.0)),
                ("contractVersion", "1.0.0".into()),
                ("mediaType", "application/json".into()),
                ("contentHash", hash_json(&task).unwrap().into()),
                (
                    "byteLength",
                    Json::Number(canonical(&task, MANAGEMENT_FRAME_BYTES).unwrap().len() as f64),
                ),
            ]),
        ),
        (
            "content",
            Json::object([("encoding", "json".into()), ("value", task.clone())]),
        ),
    ]);
    let client = Arc::new(Client {
        owner_valid: AtomicBool::new(true),
        trace: Mutex::default(),
        remote: Mutex::new(Some(reply)),
    });
    let cache = ReadCache::default();
    let store = JournalStore {
        journal: LocalJournal {
            connection: &db,
            root_object_id: Some("root".into()),
        },
        client: client.clone(),
        contracts: Policy {
            task,
            local_task: false,
        },
        limits: HistoryLimits::default(),
        now: clock as fn() -> String,
        new_id: fresh_id as fn() -> JsString,
        cache: Some(&cache),
    };
    let (document, metadata) = store
        .read_with_metadata("task-board/task", &pin(100, 2.0), true, true)
        .await
        .unwrap();
    assert_eq!(document.pin, pin(100, 2.0));
    assert_eq!(metadata, object);
    let (_, again) = store
        .read_with_metadata("task-board/task", &pin(100, 2.0), true, true)
        .await
        .unwrap();
    assert_eq!(again, object);
    assert_eq!(client.trace.lock().unwrap().len(), 1);
    assert_eq!(
        store
            .read_with_metadata("task-board/task", &pin(100, 2.0), true, false)
            .await
            .err()
            .unwrap()
            .code,
        "task_board_record_archived"
    );
    assert_eq!(
        store
            .read_with_metadata("task-board/task", &pin(100, 3.0), true, true)
            .await
            .err()
            .unwrap()
            .code,
        "task_board_evidence_mismatch"
    );
    assert_eq!(db.total_changes(), before);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ArchiveFault {
    None,
    Before,
    After,
    Identity,
}
struct ArchiveState {
    fault: ArchiveFault,
    armed: bool,
    metadata: Json,
    trace: Vec<Json>,
    requests: Vec<Json>,
    commits: usize,
    identities: usize,
}
fn archive_error(fault: ArchiveFault) -> IvyError {
    IvyError::new("revision_conflict", "Archive fixture failure.")
        .with_outcome(if fault == ArchiveFault::After {
            ivy_contracts::Outcome::Unknown
        } else {
            ivy_contracts::Outcome::NotExecuted
        })
        .with_details(Json::object([("marker", Json::Number(172.0))]))
}
struct ArchiveClient {
    task: Json,
    state: Arc<Mutex<ArchiveState>>,
}
impl RpcClient for ArchiveClient {
    fn request<'a>(&'a self, method: &'a str, args: Json, _: RequestOptions) -> RequestFuture<'a> {
        Box::pin(async move {
            let mut state = self.state.lock().unwrap();
            state
                .trace
                .push(Json::Array(vec![method.into(), args.clone()]));
            match method {
                "system.status" => {
                    state.identities += 1;
                    if state.armed && state.fault == ArchiveFault::Identity {
                        state.armed = false;
                        return Err(archive_error(state.fault));
                    }
                    Ok(Json::object([("runtimeEpoch", "epoch".into())]))
                }
                "objects.read" => {
                    assert_eq!(args.required("objectId")?, &Json::String(id(100)));
                    Ok(Json::object([
                        ("object", state.metadata.clone()),
                        (
                            "revision",
                            Json::object([
                                ("objectId", Json::String(id(100))),
                                ("revision", Json::Number(2.0)),
                                ("contractVersion", "1.0.0".into()),
                                ("mediaType", "application/json".into()),
                                ("contentHash", hash_json(&self.task)?.into()),
                                (
                                    "byteLength",
                                    Json::Number(
                                        canonical(&self.task, MANAGEMENT_FRAME_BYTES)?.len() as f64,
                                    ),
                                ),
                            ]),
                        ),
                        (
                            "content",
                            Json::object([
                                ("encoding", "json".into()),
                                ("value", self.task.clone()),
                            ]),
                        ),
                    ]))
                }
                "objects.archive" => {
                    assert_eq!(args.required("objectId")?, &Json::String(id(100)));
                    assert_eq!(args.required("expectedRevision")?, &Json::Number(2.0));
                    assert_eq!(args.required("archived")?, &Json::Bool(true));
                    state.requests.push(args);
                    if state.armed && state.fault == ArchiveFault::Before {
                        state.armed = false;
                        return Err(archive_error(state.fault));
                    }
                    assert_eq!(
                        state.commits, 0,
                        "A completed archive must not be sent twice"
                    );
                    state.commits += 1;
                    state.metadata.insert("archivedAt", clock().into());
                    state.metadata.insert("effectivelyArchived", true.into());
                    if state.armed && state.fault == ArchiveFault::After {
                        state.armed = false;
                        return Err(archive_error(state.fault));
                    }
                    Ok(state.metadata.clone())
                }
                "objects.stat" => {
                    assert_eq!(args.required("objectId")?, &Json::String(id(100)));
                    Ok(state.metadata.clone())
                }
                other => panic!("Archive membership must not call {other}: {args:?}"),
            }
        })
    }
}

#[tokio::test(flavor = "current_thread")]
async fn archive_membership_reopens_original_identity_after_failures_before_and_after_commit() {
    for fault in [
        ArchiveFault::None,
        ArchiveFault::Before,
        ArchiveFault::After,
        ArchiveFault::Identity,
    ] {
        struct Temp(std::path::PathBuf);
        impl Drop for Temp {
            fn drop(&mut self) {
                let _ = std::fs::remove_file(&self.0);
            }
        }
        let temp = Temp(std::env::temp_dir().join(format!(
            "task-board-membership-{}.sqlite",
            uuid::Uuid::new_v4()
        )));
        let group = data().as_array().unwrap().last().unwrap();
        let mut task = group
            .required("base")
            .unwrap()
            .required("task")
            .unwrap()
            .clone();
        task.insert("primaryResourceRef", Json::Null);
        let state = Arc::new(Mutex::new(ArchiveState {
            fault,
            armed: fault != ArchiveFault::None,
            metadata: Json::object([
                ("id", Json::String(id(100))),
                ("parentId", "root".into()),
                ("contractKey", "task-board/task".into()),
                ("currentRevision", Json::Number(2.0)),
                ("archivedAt", Json::Null),
                ("effectivelyArchived", false.into()),
            ]),
            trace: vec![],
            requests: vec![],
            commits: 0,
            identities: 0,
        }));
        let catalogs =
            CheckedNativeContracts::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."));
        let mut operation_id = None;
        let mut original_archive_id = None;
        for round in 0..3 {
            let db = Connection::open(&temp.0).unwrap();
            local_journal::initialize(&db).unwrap();
            // A new client/cache represents a replacement generation. Its SDK
            // identity cache cannot stand in for the persisted operation field.
            let client = Arc::new(ArchiveClient {
                task: task.clone(),
                state: state.clone(),
            });
            let cache = ReadCache::default();
            let store = JournalStore {
                journal: LocalJournal {
                    connection: &db,
                    root_object_id: Some("root".into()),
                },
                client: client.clone(),
                contracts: Policy {
                    task: task.clone(),
                    local_task: false,
                },
                limits: HistoryLimits::default(),
                now: clock as fn() -> String,
                new_id: fresh_id as fn() -> JsString,
                cache: Some(&cache),
            };
            let pages = Pages;
            let operations = Operations {
                store: LifecycleJournal {
                    journal: &store,
                    pages: &pages,
                },
                now: clock as fn() -> String,
            };
            let operation = if round == 0 {
                let operation = operations
                    .begin(
                        &Json::object([
                            ("action", "archive".into()),
                            ("operationId", "membership-reopen".into()),
                            ("taskId", Json::String(id(100))),
                            ("expectedRevision", Json::Number(2.0)),
                            ("archived", true.into()),
                        ]),
                        &Json::object([
                            ("principalId", "caller".into()),
                            ("serviceNodeId", "task-board-node".into()),
                            ("generation", Json::Number(1.0)),
                            ("source", "user".into()),
                        ]),
                    )
                    .await
                    .unwrap();
                operation_id = Some(operation.pin.required("objectId").unwrap().clone());
                operation
            } else {
                Store::read(
                    &operations.store,
                    "task-board/operation",
                    operation_id.as_ref().unwrap(),
                    true,
                )
                .await
                .unwrap()
            };
            let engine =
                LifecycleRuntime::new(client, "caller".into(), "task-board-node".into(), Policies);
            let subject = NativeLifecycle {
                store: &operations.store,
                engine: &engine,
                operations: &operations,
                catalogs: &catalogs,
            };
            let mut steps = Publication {
                operations: &operations,
                operation: &operation,
                task_written: None,
                index: 0.0,
            };
            let initial = Json::object([
                ("task", Json::Null),
                ("history", Json::Null),
                ("archive", Json::Null),
            ]);
            let result = subject.publish(&operation, &mut steps, initial).await;
            if round == 0 && fault != ArchiveFault::None {
                assert_eq!(
                    result.unwrap_err().to_wire(),
                    archive_error(fault).to_wire(),
                    "{fault:?}"
                );
            } else {
                let result = result.unwrap();
                assert_eq!(result.required("task").unwrap(), &pin(100, 2.0));
                assert_eq!(
                    result.required("archive").unwrap(),
                    &state.lock().unwrap().metadata
                );
                assert_eq!(result.required("history").unwrap(), &Json::Null);
            }
            assert_eq!(steps.index, 0.0);
            let saved = Store::read(
                &operations.store,
                "task-board/operation",
                operation_id.as_ref().unwrap(),
                true,
            )
            .await
            .unwrap();
            let archive_id = f(&saved.value, "archiveMutationId");
            if round == 0 && fault == ArchiveFault::Identity {
                assert!(archive_id.is_null());
            } else {
                assert!(!archive_id.as_js_string().unwrap().0.is_empty());
                if let Some(original) = &original_archive_id {
                    assert_eq!(archive_id, original);
                } else {
                    original_archive_id = Some(archive_id.clone());
                }
                for request in &state.lock().unwrap().requests {
                    assert_eq!(request.required("mutationId").unwrap(), archive_id);
                }
            }
            let local_tasks: i64 = db
                .query_row(
                    "SELECT count(*) FROM documents WHERE key='task-board/task'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(local_tasks, 0);
            assert!(db.is_autocommit());
            db.execute_batch("BEGIN IMMEDIATE;ROLLBACK;").unwrap();
        }
        let state = state.lock().unwrap();
        assert_eq!(state.commits, 1, "{fault:?}");
        assert_eq!(
            state.requests.len(),
            if fault == ArchiveFault::Before { 2 } else { 1 }
        );
        assert_eq!(
            state.identities,
            if fault == ArchiveFault::Identity {
                2
            } else {
                1
            }
        );
        assert!(
            state
                .trace
                .iter()
                .all(|row| row.as_array().unwrap()[0] != Json::from("tools.call"))
        );
    }
}
