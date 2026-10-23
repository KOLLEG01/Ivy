use ivy_contracts::{IvyError, IvyJson as Json, IvyResult, JsString, Outcome};
use ivy_sdk::rpc::{RequestFuture, RequestOptions, RpcClient};
use ivy_store::rusqlite::{Connection, params};
use ivy_task_board::{
    engine_execution::{CachedExecution, ExecutionServices},
    journal_store::{Contracts, JournalStore},
    local_gates::LocalGates,
    local_journal::{LocalJournal, initialize},
    local_retention::HistoryLimits,
    operations::{Document, Operations, Store},
    publication_coordination::{CoordinationStore, PublicationCoordination},
};
use std::{future::Future, sync::Arc};

#[path = "task-board-frozen-contracts.rs"]
mod contracts;
const ROOT: &str = "TaskBoard publication gate";
const TASK: &str = "TaskBoard publication gate task";
const AT: &str = "2026-10-22T09:30:00.000Z";

struct NoRpc;
impl RpcClient for NoRpc {
    fn request<'a>(&'a self, method: &'a str, _: Json, _: RequestOptions) -> RequestFuture<'a> {
        Box::pin(async move { panic!("Local gate work cannot issue {method}") })
    }
}
struct GateContracts;
impl Contracts for GateContracts {
    fn version(&self, _: &str) -> IvyResult<String> {
        Ok("1.0.0".into())
    }
    fn local(&self, _: &str) -> IvyResult<bool> {
        Ok(true)
    }
    fn readable_version(&self, _: &str, version: &JsString) -> IvyResult<bool> {
        Ok(version == &"1.0.0".into())
    }
    fn validate_definition(&self, name: &str, value: &Json) -> IvyResult<()> {
        contracts::admission(name, value)
    }
    fn validate_record(&self, key: &str, _: &JsString, value: &Json) -> IvyResult<()> {
        match key {
            "task-board/operation" => contracts::admission("Operation", value),
            "task-board/coordination" => contracts::admission("Coordination", value),
            "task-board/task" => {
                value.required("fields")?.required("workspaceRequirement")?;
                Ok(())
            }
            _ => panic!("Unexpected gate record {key}"),
        }
    }
}
fn clock() -> String {
    AT.into()
}
fn new_id() -> JsString {
    uuid::Uuid::new_v4().to_string().into()
}
fn store(
    db: &Connection,
) -> JournalStore<'_, NoRpc, GateContracts, fn() -> String, fn() -> JsString> {
    JournalStore {
        journal: LocalJournal {
            connection: db,
            root_object_id: Some("root".into()),
        },
        client: Arc::new(NoRpc),
        contracts: GateContracts,
        limits: HistoryLimits::default(),
        now: clock,
        new_id,
        cache: None,
    }
}
fn actor() -> Json {
    Json::object([
        ("principalId", "caller".into()),
        ("serviceNodeId", "fixture".into()),
        ("generation", Json::Number(1.0)),
        ("source", "worker".into()),
    ])
}
fn request(id: &str, action: &str, task: &str, directory: &str) -> Json {
    if action == "comment" {
        Json::object([
            ("action", action.into()),
            ("operationId", id.into()),
            ("taskId", task.into()),
            ("expectedRevision", Json::Number(1.0)),
            ("commentId", id.into()),
            ("body", "Gate fixture.".into()),
            ("requests", Json::Array(vec![])),
            ("responses", Json::Array(vec![])),
            ("attachments", Json::Array(vec![])),
            ("replyTo", Json::Null),
            ("authorKind", "agent".into()),
        ])
    } else {
        // Public start admission is covered by the actual factory/scheduler.
        Json::object([
            ("action", action.into()),
            ("operationId", id.into()),
            ("taskId", task.into()),
            ("expectedRevision", Json::Number(1.0)),
            ("target", Json::object([("hostId", "host".into())])),
            (
                "workspace",
                Json::object([("canonicalCwd", directory.into())]),
            ),
        ])
    }
}
fn operation(value: Json, id: &str) -> Document {
    Document {
        pin: Json::object([("objectId", id.into()), ("revision", Json::Number(1.0))]),
        value: Json::object([("request", value), ("createdAt", AT.into())]),
        parent_id: Some("root".into()),
        content_hash: String::new(),
    }
}
fn task(db: &Connection, id: &str) {
    let value = Json::object([(
        "fields",
        Json::object([
            ("category", Json::Null),
            (
                "workspaceRequirement",
                Json::object([("kind", "directory_path".into())]),
            ),
        ]),
    )]);
    db.execute(
        "INSERT INTO documents VALUES(?,'task-board/task',?,'root',1,?,?)",
        params![id, id, AT, AT],
    )
    .unwrap();
    db.execute(
        "INSERT INTO revisions VALUES(?,1,?,?)",
        params![
            id,
            ivy_contracts::json::canonical(&value, usize::MAX).unwrap(),
            AT
        ],
    )
    .unwrap();
}
fn count(db: &Connection, table: &str) -> i64 {
    db.query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
        row.get(0)
    })
    .unwrap()
}

#[tokio::test(flavor = "current_thread")]
async fn workspace_gates_serialize_only_the_same_host_and_shared_workspace() {
    let db = Connection::open_in_memory().unwrap();
    initialize(&db).unwrap();
    for id in ["first", "second", "independent"] {
        task(&db, id);
    }
    let store = store(&db);
    let principal = "board".into();
    let gates = PublicationCoordination {
        store: &store,
        principal_id: &principal,
        now: &clock,
    };
    let first = operation(request("one", "start", "first", "C:\\shared"), "one");
    let second = operation(request("two", "continue", "second", "C:\\shared"), "two");
    let independent = operation(
        request("three", "start", "independent", "C:\\separate"),
        "three",
    );
    gates.acquire(&first).await.unwrap();
    let names = store.operation_gates(&"one".into()).unwrap();
    assert_eq!(names.len(), 2);
    assert!(!names.contains(&ROOT.into()));
    assert!(names.iter().any(|name| {
        name.to_utf8()
            .unwrap()
            .starts_with("TaskBoard workspace gate sha256:")
    }));
    let before = count(&db, "active_gates");
    assert_eq!(
        gates.acquire(&second).await.unwrap_err().code,
        "task_board_publication_busy"
    );
    assert_eq!(
        count(&db, "active_gates"),
        before,
        "The workspace gate precedes the ticket gate"
    );
    gates.acquire(&independent).await.unwrap();
    assert_eq!(count(&db, "active_gates"), 4);
    // Old retained selections survive current policy instead of being recomputed.
    LocalGates::new(&db)
        .retain_operation_gates(&"retained".into(), &[ROOT.into(), TASK.into()])
        .unwrap();
    let retained = operation(request("retained", "nativeUpdate", "task", ""), "retained");
    gates.acquire(&retained).await.unwrap();
    assert_eq!(
        store.operation_gates(&"retained".into()).unwrap(),
        [JsString::from(ROOT), JsString::from(TASK)]
    );
    assert!(db.is_autocommit());
}

struct Unused;
impl ExecutionServices for Unused {
    type Transcript = Json;
    async fn with_read_cache<F: Future<Output = IvyResult<Json>>>(&self, _: F) -> IvyResult<Json> {
        panic!("No execution")
    }
    async fn owner_stopped(&self, _: &Json) -> IvyResult<bool> {
        panic!("No native observation")
    }
    async fn verify_transcript(&self, _: &Json, _: &Json) -> IvyResult<Json> {
        panic!("No evidence")
    }
    async fn normalize(&self, _: &JsString) -> IvyResult<JsString> {
        panic!("No category")
    }
    async fn revalidate(&self, _: &JsString) -> IvyResult<JsString> {
        panic!("No category")
    }
    async fn publish(
        &self,
        _: &Document,
        _: Option<&Json>,
        _: Option<&JsString>,
    ) -> IvyResult<Json> {
        panic!("No publication")
    }
}

#[tokio::test(flavor = "current_thread")]
async fn gate_clear_failure_preserves_partial_progress_and_original_refusal_after_reopen() {
    for mode in ["release", "unstarted", "failed-publication"] {
        let temp = super::Temporary::new();
        let path = temp.0.join("gates.sqlite");
        let principal = JsString::from("board");
        let id;
        {
            let db = Connection::open(&path).unwrap();
            initialize(&db).unwrap();
            let operations = Operations {
                store: store(&db),
                now: clock,
            };
            let begun = operations
                .begin(&request("intent", "comment", "task", ""), &actor())
                .await
                .unwrap();
            id = begun
                .pin
                .required("objectId")
                .unwrap()
                .as_js_string()
                .unwrap()
                .clone();
            LocalGates::new(&db)
                .retain_operation_gates(&id, &[ROOT.into(), TASK.into()])
                .unwrap();
            let gates = PublicationCoordination {
                store: &operations.store,
                principal_id: &principal,
                now: &clock,
            };
            gates.acquire(&begun).await.unwrap();
            db.execute_batch("CREATE TRIGGER refuse_clear BEFORE INSERT ON revisions WHEN NEW.id=(SELECT id FROM documents WHERE name='TaskBoard publication gate task') AND json_type(NEW.value_json,'$.activeOperation')='null' BEGIN SELECT RAISE(ABORT,'gate clear refused'); END;").unwrap();
        }
        {
            let db = Connection::open(&path).unwrap();
            initialize(&db).unwrap();
            let operations = Operations {
                store: store(&db),
                now: clock,
            };
            let gates = PublicationCoordination {
                store: &operations.store,
                principal_id: &principal,
                now: &clock,
            };
            let original = IvyError::new("invalid_arguments", "Refused before publication.")
                .with_outcome(Outcome::NotExecuted);
            let error = if mode == "unstarted" {
                gates.release_unstarted(&id).await.unwrap_err()
            } else if mode == "release" {
                operations.failure(&id, &original, false).await.unwrap();
                gates.release(&id, false).await.unwrap_err()
            } else {
                CachedExecution {
                    operations: &operations,
                    principal_id: &principal,
                    services: &Unused,
                }
                .failed_publication(&id, original)
                .await
                .unwrap_err()
            };
            assert_eq!(error.code, "storage_unavailable");
            assert_eq!(error.outcome, Outcome::Unknown);
            assert!(error.message.contains("gate clear refused"));
            assert_eq!(count(&db, "active_gates"), 1);
            let root = gates.current(&ROOT.into()).await.unwrap();
            let ticket = gates.current(&TASK.into()).await.unwrap();
            assert_eq!(root.pin.get("revision"), Some(&Json::Number(3.0)));
            assert_eq!(root.value.get("activeOperation"), Some(&Json::Null));
            assert_eq!(ticket.pin.get("revision"), Some(&Json::Number(2.0)));
            assert_eq!(
                ticket
                    .value
                    .required("activeOperation")
                    .unwrap()
                    .get("objectId"),
                Some(&Json::String(id.clone()))
            );
            let saved = operations
                .store
                .read("task-board/operation", &Json::String(id.clone()), true)
                .await
                .unwrap();
            if mode != "unstarted" {
                assert_eq!(saved.value.get("phase"), Some(&"failed".into()));
                assert_eq!(
                    saved.value.required("error").unwrap().get("code"),
                    Some(&"invalid_arguments".into())
                );
            }
            assert!(db.is_autocommit());
            db.execute_batch("BEGIN IMMEDIATE; ROLLBACK; DROP TRIGGER refuse_clear")
                .unwrap();
        }
        {
            let db = Connection::open(&path).unwrap();
            initialize(&db).unwrap();
            let journal = store(&db);
            let gates = PublicationCoordination {
                store: &journal,
                principal_id: &principal,
                now: &clock,
            };
            if mode == "unstarted" {
                assert!(gates.release_unstarted(&id).await.unwrap());
            } else {
                gates.release(&id, false).await.unwrap();
            }
            let before: i64 = db
                .query_row("SELECT total_changes()", [], |row| row.get(0))
                .unwrap();
            if mode == "unstarted" {
                assert!(!gates.release_unstarted(&id).await.unwrap());
            } else {
                gates.release(&id, false).await.unwrap();
            }
            assert_eq!(
                db.query_row::<i64, _, _>("SELECT total_changes()", [], |row| row.get(0))
                    .unwrap(),
                before
            );
            assert_eq!(count(&db, "active_gates"), 0);
            assert!(db.is_autocommit());
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn gate_name_retention_failure_rolls_back_before_any_gate_is_acquired() {
    let db = Connection::open_in_memory().unwrap();
    initialize(&db).unwrap();
    task(&db, "task");
    db.execute_batch("CREATE TRIGGER refuse_gate BEFORE INSERT ON operation_gates WHEN NEW.gate_name='TaskBoard publication gate task' BEGIN SELECT RAISE(ABORT,'gate retention refused'); END;").unwrap();
    let journal = store(&db);
    let principal = "board".into();
    let gates = PublicationCoordination {
        store: &journal,
        principal_id: &principal,
        now: &clock,
    };
    let mut request = request("one", "edit", "task", "");
    request.insert("fields", Json::object([("category", "Changed".into())]));
    let error = gates.acquire(&operation(request, "one")).await.unwrap_err();
    assert_eq!(error.code, "storage_unavailable");
    assert!(error.message.contains("gate retention refused"));
    assert_eq!(count(&db, "operation_gates"), 0);
    assert_eq!(count(&db, "active_gates"), 0);
    assert_eq!(count(&db, "documents"), 1);
    assert!(db.is_autocommit());
    db.execute_batch("BEGIN IMMEDIATE; ROLLBACK").unwrap();
}
