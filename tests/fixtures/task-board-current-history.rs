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
    fn local(&self, _: &str) -> IvyResult<bool> {
        Ok(true)
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
}
impl Source {
    fn ready() -> Self {
        Self {
            queries: Cell::new(0),
            hold_first: false,
            release: Notify::new(),
            failure: None,
            recovery: false,
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
    async fn read(&self, _: Option<Json>) -> Result<Json, Failure> {
        panic!("These closed graphs have no remote receipt or live task")
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
async fn current_history_keeps_protected_graphs_and_retires_exact_receipts_atomically() {
    for mode in [
        "closed",
        "completed-object",
        "incoming",
        "unresolved",
        "external-reference",
        "active-gate",
        "rollback",
    ] {
        let db = Connection::open_in_memory().unwrap();
        let operation = seed(&db);
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
        if ["closed", "rollback"].contains(&mode) {
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

#[tokio::test(flavor = "current_thread")]
async fn current_history_rechecks_references_changed_during_its_cooperative_index_scan() {
    let db = Connection::open_in_memory().unwrap();
    seed(&db);
    for index in 0..38 {
        row(
            &db,
            &format!("filler-{index:02}"),
            "task-board/coordination",
            "root",
            &Json::object([("note", "original".into())]),
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
    work.await.unwrap();
    assert!(parses.get() > 40);
    assert_eq!(count(&db, "documents"), 40);
    assert_eq!(count(&db, "retired_operations"), 0);
    assert_released(&db);
}
