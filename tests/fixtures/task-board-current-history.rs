use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString,
    hash::hash_json,
    json::{canonical, parse},
};
use ivy_store::rusqlite::{Connection, params};
use ivy_task_board::{
    engine_read::read_failure::{ReadCause, ReadFailure},
    journal_store::{
        Contracts,
        persistent_reads::{
            local_history::{HistoryCollections, HistoryDocuments, HistoryQueries, LocalHistory},
            local_reads::SqliteReads,
            operation_receipt::operation_receipt,
            store::ObjectReads,
        },
    },
    local_journal::{LocalJournal, initialize},
    local_retention::HistoryLimits,
};
use std::cell::{Cell, RefCell};
use tokio::sync::Notify;
type Failure = ReadFailure<IvyError>;
const AT: &str = "2026-10-22T12:00:00.000Z";
struct Catalog;
impl Contracts for Catalog {
    fn version(&self, _: &str) -> IvyResult<String> {
        Ok("1.0.0".into())
    }
    fn local(&self, key: &str) -> IvyResult<bool> {
        Ok(!matches!(
            key,
            "task-board/task" | "task-board/run" | "task-board/delivery"
        ))
    }
    fn readable_version(&self, _: &str, v: &JsString) -> IvyResult<bool> {
        Ok(v == &"1.0.0".into())
    }
    fn validate_definition(&self, _: &str, _: &Json) -> IvyResult<()> {
        panic!("History cannot admit an action")
    }
    fn validate_record(&self, _: &str, v: &JsString, value: &Json) -> IvyResult<()> {
        assert_eq!(v, &"1.0.0".into());
        assert!(value.as_object().is_some());
        Ok(())
    }
}
struct Source {
    queries: Cell<usize>,
    hold_first: bool,
    release: Notify,
    failure: Option<Failure>,
    recovery: bool,
    receipt: Option<Result<Json, Failure>>,
    reads: Cell<usize>,
}
impl Source {
    fn ready() -> Self {
        Self {
            queries: Cell::new(0),
            hold_first: false,
            release: Notify::new(),
            failure: None,
            recovery: false,
            receipt: None,
            reads: Cell::new(0),
        }
    }
}
impl HistoryQueries<Failure> for Source {
    async fn query(&self, params: Json) -> Result<Json, Failure> {
        assert_eq!(params.get("contractKey"), Some(&"task-board/task".into()));
        assert_eq!(params.get("includeArchived"), Some(&true.into()));
        let offset = self.queries.replace(self.queries.get() + 1);
        if offset == 0 && self.hold_first {
            self.release.notified().await;
            if let Some(error) = &self.failure {
                return Err(error.clone());
            }
        }
        Ok(Json::object([
            (
                "items",
                Json::Array(if self.recovery {
                    vec![Json::object([("objectId", "recovering-task".into())])]
                } else {
                    vec![]
                }),
            ),
            ("nextCursor", Json::Null),
        ]))
    }
}
impl ObjectReads<Failure> for Source {
    async fn read(&self, params: Option<Json>) -> Result<Json, Failure> {
        assert_eq!(params, Some(Json::object([("objectId", "receipt".into())])));
        self.reads.set(self.reads.get() + 1);
        self.receipt.clone().expect("No remote receipt is bound")
    }
}
struct NoDocuments;
impl HistoryDocuments<Failure> for NoDocuments {
    async fn history_document(
        &self,
        _: &str,
        _: Option<&Json>,
        _: bool,
        _: bool,
    ) -> Result<Json, Failure> {
        panic!("These graphs have no live handoff or recovery lineage")
    }
}
fn parser(raw: &str) -> Result<Json, Failure> {
    parse(raw, usize::MAX).map_err(Into::into)
}
fn sql_failure(error: ivy_store::rusqlite::Error) -> Failure {
    Failure::source(ivy_store::sqlite::error(error))
}
fn local(
    db: &Connection,
) -> SqliteReads<
    '_,
    Catalog,
    fn(&str) -> Result<Json, Failure>,
    fn(ivy_store::rusqlite::Error) -> Failure,
> {
    SqliteReads {
        journal: LocalJournal {
            connection: db,
            root_object_id: Some("root".into()),
        },
        contracts: &Catalog,
        parse: parser,
        sqlite_failure: sql_failure,
    }
}
fn row(db: &Connection, id: &str, key: &str, parent: &str, value: &Json) {
    db.execute(
        "INSERT INTO documents VALUES(?,?,?,?,1,?,?)",
        params![id, key, id, parent, AT, AT],
    )
    .unwrap();
    db.execute(
        "INSERT INTO revisions VALUES(?,1,?,?)",
        params![id, canonical(value, usize::MAX).unwrap(), AT],
    )
    .unwrap();
}
fn seed(db: &Connection) -> Json {
    initialize(db).unwrap();
    let request = Json::object([("action", "comment".into()), ("taskId", "task".into())]);
    let operation = Json::object([
        ("schemaVersion", Json::Number(1.0)),
        ("phase", "failed".into()),
        ("operationId", "original-id".into()),
        ("callerPrincipalId", "caller".into()),
        ("requestHash", hash_json(&request).unwrap().into()),
        ("request", request),
        ("actor", Json::object([("source", "worker".into())])),
        ("writes", Json::Array(vec![])),
        (
            "error",
            Json::object([
                ("code", "invalid_arguments".into()),
                ("message", "Conclusive refusal.".into()),
                ("execution", "not_executed".into()),
            ]),
        ),
        ("outcome", Json::Null),
        ("updatedAt", AT.into()),
    ]);
    row(db, "closed", "task-board/operation", "root", &operation);
    row(
        db,
        "child",
        "task-board/native-evidence",
        "closed",
        &Json::object([("payload", "retained bytes".into())]),
    );
    db.execute("INSERT INTO cleanup_roots VALUES('closed',?)", [AT])
        .unwrap();
    operation
}
fn count(db: &Connection, table: &str) -> i64 {
    db.query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
        row.get(0)
    })
    .unwrap()
}
fn assert_released(db: &Connection) {
    assert!(db.is_autocommit());
    db.execute_batch("BEGIN IMMEDIATE; ROLLBACK").unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn current_history_delivery_veto_preserves_errors_and_receipts_under_row_and_byte_pressure() {
    for bytes_only in [false, true] {
        for state in [
            "pending",
            "outcome_unknown",
            "confirmed",
            "not_found",
            "service_unavailable",
        ] {
            let db = Connection::open_in_memory().unwrap();
            let mut operation = seed(&db);
            operation.insert(
                "writes",
                Json::Array(vec![Json::object([
                    ("contractKey", "task-board/delivery".into()),
                    ("outcome", Json::object([("objectId", "receipt".into())])),
                ])]),
            );
            db.execute(
                "UPDATE revisions SET value_json=? WHERE id='closed'",
                [canonical(&operation, usize::MAX).unwrap()],
            )
            .unwrap();
            let reply = if ["not_found", "service_unavailable"].contains(&state) {
                Err(Failure::from(IvyError::new(
                    state,
                    "Original remote receipt failure.",
                )))
            } else {
                Ok(Json::object([
                    (
                        "object",
                        Json::object([("contractKey", "task-board/delivery".into())]),
                    ),
                    (
                        "content",
                        Json::object([
                            ("encoding", "json".into()),
                            ("value", Json::object([("state", state.into())])),
                        ]),
                    ),
                ]))
            };
            let source = Source {
                receipt: Some(reply),
                ..Source::ready()
            };
            let local = local(&db);
            let collections = HistoryCollections::default();
            let changes = db.total_changes();
            LocalHistory::new(
                &local,
                &NoDocuments,
                &source,
                &collections,
                HistoryLimits::default(),
            )
            .collect::<IvyError>(0, 0, None, None)
            .await
            .unwrap();
            assert_eq!(db.total_changes(), changes);
            assert_eq!(source.queries.get(), 0);
            assert_eq!(source.reads.get(), 0);
            let limits = if bytes_only {
                HistoryLimits {
                    count: 10000,
                    bytes: 0,
                }
            } else {
                HistoryLimits {
                    count: 0,
                    ..HistoryLimits::default()
                }
            };
            let history = LocalHistory::new(&local, &NoDocuments, &source, &collections, limits);
            let before = count(&db, "documents");
            let result = history.collect::<IvyError>(0, 0, None, None).await;
            if state == "service_unavailable" {
                match result.unwrap_err().cause() {
                    ReadCause::Ivy(error) => {
                        assert_eq!(error.code, "service_unavailable");
                        assert_eq!(error.message, "Original remote receipt failure.");
                    }
                    _ => panic!("Remote receipt failure was replaced"),
                }
            } else {
                result.unwrap();
            }
            assert_eq!(source.reads.get(), 1);
            let retired = ["confirmed", "not_found"].contains(&state);
            assert_eq!(count(&db, "documents"), if retired { 0 } else { before });
            assert_eq!(count(&db, "retired_operations"), i64::from(retired));
            assert_eq!(count(&db, "retired_operation_receipts"), i64::from(retired));
            if retired {
                let receipt = local
                    .retired_operation_receipt::<Failure>(
                        Some(&"caller".into()),
                        Some(&"original-id".into()),
                    )
                    .unwrap()
                    .unwrap();
                assert_eq!(
                    canonical(&receipt, usize::MAX).unwrap(),
                    canonical(
                        &operation_receipt::<IvyError>(&operation).unwrap(),
                        usize::MAX
                    )
                    .unwrap()
                );
            }
            assert_released(&db);
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn current_history_keeps_protected_graphs_and_retires_exact_receipts_atomically() {
    for mode in [
        "closed",
        "completed-object",
        "self-original",
        "self-original-unknown",
        "incoming",
        "unresolved",
        "external-reference",
        "active-gate",
        "rollback",
    ] {
        let db = Connection::open_in_memory().unwrap();
        let mut operation = seed(&db);
        if mode.starts_with("self-original") {
            let mut original = operation.clone();
            let mut request = Json::object([
                ("action", "uploadAttachment".into()),
                ("operationId", "original-id".into()),
                ("taskId", "task".into()),
                ("expectedRevision", Json::Number(1.0)),
                ("attachmentId", "file".into()),
                ("filename", "file.bin".into()),
                ("mediaType", "application/octet-stream".into()),
                ("bytesBase64", "AAE=".into()),
            ]);
            original.insert("requestHash", hash_json(&request).unwrap().into());
            original.insert("request", request.clone());
            original.insert("phase", "accepted".into());
            original.insert("error", Json::Null);
            operation.insert(
                "requestHash",
                original.required("requestHash").unwrap().clone(),
            );
            request.remove("bytesBase64");
            request.insert(
                "originalRequest",
                Json::object([
                    ("objectId", "closed".into()),
                    ("revision", Json::Number(1.0)),
                ]),
            );
            operation.insert("request", request);
            if mode == "self-original-unknown" {
                operation
                    .get_mut("error")
                    .unwrap()
                    .insert("execution", "unknown".into());
            }
            db.execute(
                "UPDATE revisions SET value_json=? WHERE id='closed' AND revision=1",
                [canonical(&original, usize::MAX).unwrap()],
            )
            .unwrap();
            db.execute(
                "INSERT INTO revisions VALUES('closed',2,?,?)",
                params![canonical(&operation, usize::MAX).unwrap(), AT],
            )
            .unwrap();
            db.execute(
                "UPDATE documents SET current_revision=2 WHERE id='closed'",
                [],
            )
            .unwrap();
        }
        if mode == "completed-object" {
            let mut complete = operation.clone();
            complete.insert("phase", "succeeded".into());
            complete.insert("error", Json::Null);
            complete.insert(
                "outcome",
                Json::object([("operationId", "original-id".into()), ("task", Json::Null)]),
            );
            db.execute(
                "UPDATE revisions SET value_json=? WHERE id='closed'",
                [canonical(&complete, usize::MAX).unwrap()],
            )
            .unwrap();
            operation = complete;
        }
        if mode == "unresolved" {
            row(
                &db,
                "call",
                "task-board/native-call",
                "closed",
                &Json::object([
                    ("state", "observed".into()),
                    ("ownerPhase", "outcome_unknown".into()),
                ]),
            );
        }
        if mode == "external-reference" {
            row(
                &db,
                "outside",
                "task-board/coordination",
                "root",
                &Json::object([("objectId", "child".into())]),
            );
        }
        if mode == "active-gate" {
            db.execute("INSERT INTO active_gates VALUES('gate','closed')", [])
                .unwrap();
        }
        if mode == "rollback" {
            db.execute_batch("CREATE TRIGGER refuse_retirement BEFORE DELETE ON documents WHEN OLD.id='closed' BEGIN SELECT RAISE(ABORT,'retirement refused'); END;").unwrap();
        }
        let before = count(&db, "documents");
        let source = Source::ready();
        let local = local(&db);
        let collections = HistoryCollections::default();
        let history = LocalHistory::new(
            &local,
            &NoDocuments,
            &source,
            &collections,
            HistoryLimits { count: 0, bytes: 0 },
        );
        let incoming = Json::object([("objectId", "child".into())]);
        let result = history
            .collect::<IvyError>(0, 0, None, (mode == "incoming").then_some(&incoming))
            .await;
        if mode == "rollback" {
            match result.unwrap_err().cause() {
                ReadCause::Source(error) => {
                    assert_eq!(error.code, "storage_unavailable");
                    assert!(error.message.contains("retirement refused"));
                }
                _ => panic!("Original SQLite failure was lost"),
            }
            assert_eq!(count(&db, "documents"), before);
            assert_eq!(count(&db, "retired_operations"), 0);
            assert_eq!(count(&db, "retired_operation_receipts"), 0);
            assert_released(&db);
            db.execute_batch("DROP TRIGGER refuse_retirement").unwrap();
            history.collect::<IvyError>(0, 0, None, None).await.unwrap();
        } else {
            result.unwrap();
        }
        if ["closed", "completed-object", "self-original", "rollback"].contains(&mode) {
            assert_eq!(count(&db, "documents"), 0);
            assert_eq!(count(&db, "retired_operations"), 1);
            assert_eq!(count(&db, "retired_operation_receipts"), 1);
            assert_eq!(count(&db, "cleanup_roots"), 0);
            let receipt = local
                .retired_operation_receipt::<Failure>(
                    Some(&"caller".into()),
                    Some(&"original-id".into()),
                )
                .unwrap()
                .unwrap();
            assert_eq!(
                canonical(&receipt, usize::MAX).unwrap(),
                canonical(
                    &operation_receipt::<IvyError>(&operation).unwrap(),
                    usize::MAX
                )
                .unwrap()
            );
            assert!(
                local
                    .retired_operation_receipt::<Failure>(
                        Some(&"other".into()),
                        Some(&"original-id".into())
                    )
                    .unwrap()
                    .is_none()
            );
            assert!(db.execute("INSERT INTO documents VALUES('replay','task-board/operation','closed','root',1,?,?)",params![AT,AT]).is_err());
        } else {
            assert_eq!(count(&db, "documents"), before, "{mode}");
            assert_eq!(count(&db, "retired_operations"), 0);
            if mode == "self-original-unknown" {
                assert_eq!(db.query_row("SELECT json_extract(value_json,'$.request.bytesBase64') FROM revisions WHERE id='closed' AND revision=1", [], |r|r.get::<_,String>(0)).unwrap(), "AAE=");
            }
        }
        assert_released(&db);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn current_history_fifo_rechecks_budget_and_preserves_the_failed_predecessor() {
    for failed in [false, true] {
        let db = Connection::open_in_memory().unwrap();
        seed(&db);
        let error = Failure::source(IvyError::new(
            "original_query_failure",
            "Original query failure.",
        ));
        let source = Source {
            hold_first: true,
            failure: failed.then(|| error.clone()),
            ..Source::ready()
        };
        let local = local(&db);
        let collections = HistoryCollections::default();
        let history = LocalHistory::new(
            &local,
            &NoDocuments,
            &source,
            &collections,
            HistoryLimits { count: 0, bytes: 0 },
        );
        let second_facade = LocalHistory::new(
            &local,
            &NoDocuments,
            &source,
            &collections,
            HistoryLimits { count: 0, bytes: 0 },
        );
        let mut first = Box::pin(history.collect::<IvyError>(0, 0, None, None));
        assert!(futures_util::poll!(first.as_mut()).is_pending());
        assert_eq!(source.queries.get(), 1);
        let mut second = Box::pin(second_facade.collect::<IvyError>(0, 0, None, None));
        assert!(futures_util::poll!(second.as_mut()).is_pending());
        assert_eq!(source.queries.get(), 1);
        if !failed {
            db.execute_batch("DELETE FROM revisions; DELETE FROM documents")
                .unwrap();
        }
        source.release.notify_one();
        let (one, two) = futures_util::join!(first, second);
        if failed {
            assert!(one.unwrap_err().ptr_eq(&error));
        } else {
            one.unwrap();
        }
        two.unwrap();
        assert_eq!(source.queries.get(), if failed { 2 } else { 1 });
        assert_eq!(count(&db, "documents"), 0);
        assert_released(&db);
    }
}

struct RecoveryDocuments {
    mode: &'static str,
    reads: RefCell<Vec<Json>>,
    failure: Failure,
}
impl HistoryDocuments<Failure> for RecoveryDocuments {
    async fn history_document(
        &self,
        key: &str,
        address: Option<&Json>,
        primary: bool,
        archived: bool,
    ) -> Result<Json, Failure> {
        assert!(primary);
        assert!(!archived);
        let address = address.unwrap();
        self.reads
            .borrow_mut()
            .push(Json::Array(vec![key.into(), address.clone()]));
        if key == "task-board/task" {
            assert_eq!(address, &Json::from("recovering-task"));
            return Ok(Json::object([(
                "value",
                Json::object([(
                    "completionIssue",
                    Json::object([(
                        "run",
                        Json::object([("objectId", "run".into()), ("revision", Json::Number(2.0))]),
                    )]),
                )]),
            )]));
        }
        assert_eq!(key, "task-board/run");
        if address.as_object().is_some() {
            assert_eq!(address.get("objectId"), Some(&"run".into()));
            assert_eq!(address.get("revision"), Some(&Json::Number(2.0)));
            if self.mode == "source-refusal" {
                return Err(self.failure.clone());
            }
            return Err(IvyError::new("revision_pruned", "Original revision was pruned.").into());
        }
        assert_eq!(address, &Json::from("run"));
        Ok(Json::object([
            (
                "pin",
                Json::object([
                    ("objectId", "run".into()),
                    (
                        "revision",
                        Json::Number(if self.mode == "older" { 1.0 } else { 3.0 }),
                    ),
                ]),
            ),
            (
                "value",
                Json::object([
                    (
                        "phase",
                        if self.mode == "running" {
                            "running"
                        } else {
                            "completed"
                        }
                        .into(),
                    ),
                    (
                        "externalOutcome",
                        Json::object([(
                            "state",
                            if self.mode == "unknown" {
                                "unknown"
                            } else {
                                "succeeded"
                            }
                            .into(),
                        )]),
                    ),
                    ("evidence", Json::object([("objectId", "child".into())])),
                ]),
            ),
        ]))
    }
}

#[tokio::test(flavor = "current_thread")]
async fn current_history_pruned_recovery_requires_the_same_confirmed_terminal_lineage() {
    for mode in ["confirmed", "older", "running", "unknown", "source-refusal"] {
        let db = Connection::open_in_memory().unwrap();
        seed(&db);
        let source = Source {
            recovery: true,
            ..Source::ready()
        };
        let local = local(&db);
        let collections = HistoryCollections::default();
        let documents = RecoveryDocuments {
            mode,
            reads: RefCell::new(vec![]),
            failure: Failure::source(IvyError::new(
                "source_refusal",
                "Original recovery read failed.",
            )),
        };
        let history = LocalHistory::new(
            &local,
            &documents,
            &source,
            &collections,
            HistoryLimits { count: 0, bytes: 0 },
        );
        let result = history.collect::<IvyError>(0, 0, None, None).await;
        if mode == "confirmed" {
            result.unwrap();
        } else if mode == "source-refusal" {
            assert!(result.unwrap_err().ptr_eq(&documents.failure));
        } else {
            assert!(
                matches!(result.unwrap_err().cause(), ReadCause::Ivy(error) if error.code=="task_board_recovery_evidence_missing")
            );
        }
        assert_eq!(
            documents.reads.borrow().len(),
            if mode == "source-refusal" { 2 } else { 3 }
        );
        assert_eq!(count(&db, "documents"), 2);
        assert_eq!(count(&db, "retired_operations"), 0);
        assert_released(&db);
    }
}

async fn recheck_cooperative_references(new_target: bool) {
    let db = Connection::open_in_memory().unwrap();
    seed(&db);
    for index in 0..38 {
        row(
            &db,
            &format!("filler-{index:02}"),
            "task-board/coordination",
            "root",
            &Json::object([(
                "note",
                if new_target { "late-child" } else { "original" }.into(),
            )]),
        );
    }
    let parses = Cell::new(0);
    let local = SqliteReads {
        journal: LocalJournal {
            connection: &db,
            root_object_id: Some("root".into()),
        },
        contracts: &Catalog,
        parse: |raw: &str| {
            parses.set(parses.get() + 1);
            parser(raw)
        },
        sqlite_failure: sql_failure,
    };
    let source = Source::ready();
    let collections = HistoryCollections::default();
    let history = LocalHistory::new(
        &local,
        &NoDocuments,
        &source,
        &collections,
        HistoryLimits { count: 0, bytes: 0 },
    );
    let mut work = Box::pin(history.collect::<IvyError>(0, 0, None, None));
    assert!(futures_util::poll!(work.as_mut()).is_pending());
    assert_eq!(parses.get(), 32);
    if new_target {
        row(
            &db,
            "late-child",
            "task-board/coordination",
            "closed",
            &Json::object([("note", "created during collection".into())]),
        );
    } else {
        let changed = canonical(&Json::object([("objectId", "child".into())]), usize::MAX).unwrap();
        db.execute(
            "INSERT INTO revisions VALUES('filler-00',2,?,?)",
            params![changed, AT],
        )
        .unwrap();
        db.execute(
            "UPDATE documents SET current_revision=2 WHERE id='filler-00'",
            [],
        )
        .unwrap();
    }
    work.await.unwrap();
    assert!(parses.get() > 40);
    assert_eq!(count(&db, "documents"), 40 + i64::from(new_target));
    assert_eq!(count(&db, "retired_operations"), 0);
    assert_released(&db);
}

#[tokio::test(flavor = "current_thread")]
async fn current_history_rechecks_references_changed_during_its_cooperative_index_scan() {
    recheck_cooperative_references(false).await;
}

#[tokio::test(flavor = "current_thread")]
async fn current_history_rechecks_unchanged_references_when_new_targets_appear() {
    recheck_cooperative_references(true).await;
}

#[cfg(target_os = "linux")]
#[tokio::test(flavor = "current_thread")]
async fn current_history_does_not_retain_comment_bodies_as_reference_ids() {
    const NAME: &str =
        "current_history::current_history_does_not_retain_comment_bodies_as_reference_ids";
    const CHILD: &str = "IVY_HISTORY_MEMORY_TEST_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", NAME, "--nocapture"])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    fn peak_bytes() -> usize {
        std::fs::read_to_string("/proc/self/status")
            .unwrap()
            .lines()
            .find(|line| line.starts_with("VmHWM:"))
            .unwrap()
            .split_whitespace()
            .nth(1)
            .unwrap()
            .parse::<usize>()
            .unwrap()
            * 1024
    }
    let db = Connection::open_in_memory().unwrap();
    initialize(&db).unwrap();
    let body = Json::object([("body", "x".repeat(1024 * 1024).into())]);
    for index in 0..32 {
        row(
            &db,
            &format!("record-{index:02}"),
            "task-board/task",
            "root",
            &body,
        );
    }
    row(
        &db,
        "unused",
        "task-board/coordination",
        "root",
        &Json::object([("activeOperation", Json::Null)]),
    );
    db.execute("INSERT INTO cleanup_roots VALUES('unused',?)", [AT])
        .unwrap();
    let local = local(&db);
    let source = Source::ready();
    let collections = HistoryCollections::default();
    let history = LocalHistory::new(
        &local,
        &NoDocuments,
        &source,
        &collections,
        HistoryLimits { count: 0, bytes: 0 },
    );
    let before = peak_bytes();
    history.collect::<IvyError>(0, 0, None, None).await.unwrap();
    let after = peak_bytes();
    assert!(
        after < before + 16 * 1024 * 1024,
        "before={before}, after={after}"
    );
    assert_eq!(count(&db, "documents"), 32);
    assert_released(&db);
}

struct CollectionClient {
    queries: std::sync::atomic::AtomicUsize,
    reads: std::sync::Mutex<Vec<Json>>,
    hold_first: bool,
    release: Notify,
    task: Json,
    failure: Option<IvyError>,
}
impl CollectionClient {
    fn new(task: Json) -> Self {
        Self {
            queries: std::sync::atomic::AtomicUsize::new(0),
            reads: std::sync::Mutex::new(vec![]),
            hold_first: false,
            release: Notify::new(),
            task,
            failure: None,
        }
    }
}
impl ivy_sdk::rpc::RpcClient for CollectionClient {
    fn request<'a>(
        &'a self,
        method: &'a str,
        params: Json,
        _: ivy_sdk::rpc::RequestOptions,
    ) -> ivy_sdk::rpc::RequestFuture<'a> {
        Box::pin(async move {
            match method {
                "objects.query" => {
                    assert_eq!(params.get("contractKey"), Some(&"task-board/task".into()));
                    assert_eq!(params.get("includeArchived"), Some(&true.into()));
                    let previous = self
                        .queries
                        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    if previous == 0 && self.hold_first {
                        self.release.notified().await;
                    }
                    Ok(Json::object([
                        ("items", Json::Array(vec![])),
                        ("nextCursor", Json::Null),
                    ]))
                }
                "objects.read" => {
                    assert_eq!(
                        params,
                        Json::object([("objectId", "task".into())]),
                        "Collection must never hydrate conversation nodes"
                    );
                    self.reads.lock().unwrap().push(params);
                    if let Some(error) = &self.failure {
                        return Err(error.clone());
                    }
                    Ok(Json::object([
                        (
                            "object",
                            Json::object([
                                ("id", "task".into()),
                                ("contractKey", "task-board/task".into()),
                                ("parentId", "root".into()),
                                ("effectivelyArchived", false.into()),
                            ]),
                        ),
                        (
                            "revision",
                            Json::object([
                                ("revision", Json::Number(1.0)),
                                ("contractVersion", "1.0.0".into()),
                                ("mediaType", "application/json".into()),
                                ("contentHash", hash_json(&self.task)?.into()),
                                ("byteLength", Json::Number(1000.0)),
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
                _ => panic!("Collection cannot perform {method}"),
            }
        })
    }
}

#[tokio::test(flavor = "current_thread")]
async fn journal_collection_preserves_live_handoffs_unknown_reads_and_original_receipts_without_hydration()
 {
    use ivy_task_board::journal_store::{JournalStore, ReadCache};
    for state in ["claim", "completionIssue", "closed", "unavailable"] {
        let db = Connection::open_in_memory().unwrap();
        let mut operation = seed(&db);
        operation.insert("phase", "succeeded".into());
        operation.insert("error", Json::Null);
        let run = Json::object([("objectId", "run".into()), ("revision", Json::Number(1.0))]);
        operation.insert("handoffRun", run.clone());
        db.execute(
            "UPDATE revisions SET value_json=? WHERE id='closed'",
            [canonical(&operation, usize::MAX).unwrap()],
        )
        .unwrap();
        let mut task = Json::object([
            ("claim", Json::Null),
            ("completionIssue", Json::Null),
            ("comments", Json::Array(vec![])),
            ("commentDeliveries", Json::Array(vec![])),
            (
                "commentStore",
                Json::object([
                    ("kind", "tree".into()),
                    ("length", Json::Number(1_000_000.0)),
                    (
                        "root",
                        Json::object([
                            ("length", Json::Number(1_000_000.0)),
                            (
                                "object",
                                Json::object([
                                    ("objectId", "unread-conversation".into()),
                                    ("revision", Json::Number(1.0)),
                                ]),
                            ),
                        ]),
                    ),
                ]),
            ),
        ]);
        if state == "claim" {
            task.insert("claim", Json::object([("run", run.clone())]));
        }
        if state == "completionIssue" {
            task.insert("completionIssue", Json::object([("run", run)]));
        }
        let mut client = CollectionClient::new(task);
        if state == "unavailable" {
            client.failure = Some(
                IvyError::new("service_unavailable", "Original remote read.")
                    .with_outcome(ivy_contracts::Outcome::Unknown),
            );
        }
        let client = std::sync::Arc::new(client);
        let cache = ReadCache::default();
        let store = JournalStore {
            journal: LocalJournal {
                connection: &db,
                root_object_id: Some("root".into()),
            },
            client: client.clone(),
            contracts: Catalog,
            limits: HistoryLimits { count: 0, bytes: 0 },
            now: || AT.to_owned(),
            new_id: || panic!("Collection never allocates a new identity"),
            cache: Some(&cache),
        };
        let result = store.collect_local().await;
        if state == "unavailable" {
            let failure = result.unwrap_err();
            assert_eq!(failure.code, "service_unavailable");
            assert_eq!(failure.message, "Original remote read.");
            assert_eq!(failure.outcome, ivy_contracts::Outcome::Unknown);
        } else {
            result.unwrap();
        }
        assert_eq!(client.queries.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(client.reads.lock().unwrap().len(), 1);
        assert_eq!(
            count(&db, "documents"),
            if state == "closed" { 0 } else { 2 }
        );
        assert_eq!(
            count(&db, "retired_operation_receipts"),
            if state == "closed" { 1 } else { 0 }
        );
        if state == "closed" {
            let encoded: String = db.query_row(
                "SELECT receipt_json FROM retired_operation_receipts WHERE caller_id='caller' AND operation_id='original-id'",
                [], |row| row.get(0)).unwrap();
            assert_eq!(
                encoded,
                canonical(
                    &operation_receipt::<IvyError>(&operation).unwrap(),
                    usize::MAX
                )
                .unwrap()
            );
        }
        assert_released(&db);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn journal_collection_serializes_on_the_shared_owner_while_local_writes_remain_independent() {
    use futures_util::FutureExt;
    use ivy_task_board::{
        journal_store::{JournalStore, ReadCache},
        operations::Store,
    };
    let db = Connection::open_in_memory().unwrap();
    seed(&db);
    let mut client = CollectionClient::new(Json::Null);
    client.hold_first = true;
    let client = std::sync::Arc::new(client);
    let cache = ReadCache::default();
    let store = JournalStore {
        journal: LocalJournal {
            connection: &db,
            root_object_id: Some("root".into()),
        },
        client: client.clone(),
        contracts: Catalog,
        limits: HistoryLimits { count: 0, bytes: 0 },
        now: || AT.to_owned(),
        new_id: || "created".into(),
        cache: Some(&cache),
    };
    let first = store.collect_local();
    let second = store.collect_local();
    tokio::pin!(first, second);
    assert!(first.as_mut().now_or_never().is_none());
    assert!(second.as_mut().now_or_never().is_none());
    assert_eq!(client.queries.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert!(db.is_autocommit());
    let created = store
        .write(
            "task-board/coordination",
            &Json::object([("activeOperation", Json::Null)]),
            "independent-write",
            &Json::object([(
                "create",
                Json::object([("name", "new".into()), ("parentId", "root".into())]),
            )]),
        )
        .await
        .unwrap();
    assert_eq!(created.get("objectId"), Some(&"created".into()));
    assert_eq!(client.queries.load(std::sync::atomic::Ordering::SeqCst), 1);
    client.release.notify_one();
    let (first, second) = tokio::join!(first, second);
    first.unwrap();
    second.unwrap();
    assert_eq!(
        client.queries.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "The queued collection has no eligible graph left and performs no second recovery query"
    );
    assert_eq!(count(&db, "documents"), 1);
    assert_eq!(count(&db, "retired_operation_receipts"), 1);
    assert!(store.journal.key(&"closed".into()).unwrap().is_none());
    assert!(store.journal.key(&"created".into()).unwrap().is_some());
    assert_released(&db);
}

impl ivy_sdk::rpc::OptionalRpcClient for CollectionClient {
    fn request_optional<'a>(
        &'a self,
        method: &'a str,
        params: Option<Json>,
        options: ivy_sdk::rpc::RequestOptions,
    ) -> ivy_sdk::rpc::RequestFuture<'a> {
        match params {
            Some(params) => ivy_sdk::rpc::RpcClient::request(self, method, params, options),
            None => Box::pin(async { Err(ivy_contracts::IvyError::internal()) }),
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn journal_collection_retains_uncertain_prepared_public_records_until_their_original_outcome_settles()
 {
    use ivy_task_board::journal_store::{JournalStore, ReadCache};
    for key in ["task-board/task", "task-board/run", "task-board/delivery"] {
        let db = Connection::open_in_memory().unwrap();
        seed(&db);
        let uncertain = Json::object([(
            "externalOutcome",
            Json::object([("state", "unknown".into())]),
        )]);
        row(&db, "prepared-public", key, "closed", &uncertain);
        let original = canonical(&uncertain, usize::MAX).unwrap();
        let client = std::sync::Arc::new(CollectionClient::new(Json::Null));
        let cache = ReadCache::default();
        let store = JournalStore {
            journal: LocalJournal {
                connection: &db,
                root_object_id: Some("root".into()),
            },
            client: client.clone(),
            contracts: Catalog,
            limits: HistoryLimits { count: 0, bytes: 0 },
            now: || AT.to_owned(),
            new_id: || panic!("Collection does not allocate an identity"),
            cache: Some(&cache),
        };
        store.collect_local().await.unwrap();
        assert_eq!(count(&db, "documents"), 3, "{key}");
        assert_eq!(count(&db, "retired_operation_receipts"), 0);
        assert_eq!(
            db.query_row(
                "SELECT value_json FROM revisions WHERE id='prepared-public'",
                [],
                |row| row.get::<_, String>(0)
            )
            .unwrap(),
            original
        );
        assert!(client.reads.lock().unwrap().is_empty());
        let settled = Json::object([(
            "externalOutcome",
            Json::object([("state", "succeeded".into())]),
        )]);
        store
            .journal
            .update(key, &"prepared-public".into(), 1.0, &settled, &AT.into())
            .unwrap();
        store.collect_local().await.unwrap();
        assert_eq!(count(&db, "documents"), 0);
        assert_eq!(count(&db, "retired_operations"), 1);
        assert_eq!(count(&db, "retired_operation_receipts"), 1);
        assert_eq!(client.queries.load(std::sync::atomic::Ordering::SeqCst), 2);
        assert_released(&db);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn current_history_without_eligible_roots_never_reads_bodies_or_remote_owners() {
    let db = Connection::open_in_memory().unwrap();
    seed(&db);
    db.execute("DELETE FROM cleanup_roots", []).unwrap();
    let local = SqliteReads {
        journal: LocalJournal {
            connection: &db,
            root_object_id: Some("root".into()),
        },
        contracts: &Catalog,
        parse: |_: &str| -> Result<Json, Failure> {
            panic!("There is no eligible body to inspect")
        },
        sqlite_failure: sql_failure,
    };
    let source = Source::ready();
    let collections = HistoryCollections::default();
    let changes = db.total_changes();
    LocalHistory::new(
        &local,
        &NoDocuments,
        &source,
        &collections,
        HistoryLimits { count: 0, bytes: 0 },
    )
    .collect::<IvyError>(0, 0, None, None)
    .await
    .unwrap();
    assert_eq!(source.queries.get(), 0);
    assert_eq!(source.reads.get(), 0);
    assert_eq!(count(&db, "documents"), 2);
    assert_eq!(db.total_changes(), changes);
    assert_released(&db);
}
