use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString,
    json::{canonical, parse},
    limits::MANAGEMENT_FRAME_BYTES,
};
use ivy_sdk::task_comments::{CancellationToken, CommentReadFuture, CommentSource};
use ivy_store::rusqlite::Connection;
use ivy_task_board::{
    engine_read::read_failure::{ReadCause, ReadFailure},
    engine_read::read_policy::QuestionPolicies,
    journal_store::{
        Contracts, ReadScopes,
        persistent_reads::{
            local_reads::SqliteReads,
            store::{ObjectReads, PersistentReadStore},
            task_comments::TaskComments,
        },
    },
    local_journal::{LocalJournal, initialize},
};
use std::{cell::RefCell, sync::Arc};
struct Catalog;
impl Contracts for Catalog {
    fn version(&self, _: &str) -> IvyResult<String> {
        Ok("1.0.0".into())
    }
    fn local(&self, key: &str) -> IvyResult<bool> {
        Ok(key == "task-board/operation")
    }
    fn readable_version(&self, _: &str, v: &JsString) -> IvyResult<bool> {
        Ok(v == &JsString::from("1.0.0"))
    }
    fn validate_record(&self, _: &str, _: &JsString, _: &Json) -> IvyResult<()> {
        Ok(())
    }
    fn validate_definition(&self, _: &str, _: &Json) -> IvyResult<()> {
        Ok(())
    }
}
type Failure = ReadFailure<()>;
struct Policies;
impl QuestionPolicies<Failure> for Policies {
    fn open_questions(&self, _: &Json) -> Result<Vec<JsString>, Failure> {
        Ok(vec![])
    }
}
struct NoComments;
impl CommentSource for NoComments {
    type Error = IvyError;
    fn read<'a>(
        &'a self,
        _: Json,
        _: Option<CancellationToken>,
    ) -> CommentReadFuture<'a, Self::Error> {
        Box::pin(async { panic!("history reads do not load conversations") })
    }
}
struct Objects {
    value: RefCell<Json>,
    calls: RefCell<Vec<Json>>,
}
impl ObjectReads<Failure> for Objects {
    async fn read(&self, params: Option<Json>) -> Result<Json, Failure> {
        self.calls.borrow_mut().push(params.unwrap());
        tokio::task::yield_now().await;
        Ok(self.value.borrow().clone())
    }
}
fn snapshot() -> Json {
    Json::object([
        (
            "object",
            Json::object([
                ("id", "remote".into()),
                ("contractKey", "task-board/history".into()),
                ("parentId", "root".into()),
                ("effectivelyArchived", false.into()),
            ]),
        ),
        (
            "revision",
            Json::object([
                ("revision", Json::Number(7.0)),
                ("contractVersion", "1.0.0".into()),
                ("mediaType", "application/json".into()),
            ]),
        ),
        (
            "content",
            Json::object([
                ("encoding", "json".into()),
                ("value", Json::object([("event", "saved".into())])),
            ]),
        ),
    ])
}
fn pin() -> Json {
    Json::object([
        ("objectId", "remote".into()),
        ("revision", Json::Number(7.0)),
    ])
}
fn code(e: &Failure) -> String {
    match e.cause() {
        ReadCause::Ivy(e) => e.code.to_string(),
        _ => panic!("{e:?}"),
    }
}
#[tokio::test(flavor = "current_thread")]
async fn remote_documents_enforce_scope_version_and_pins_and_isolate_action_caches() {
    let db = Connection::open_in_memory().unwrap();
    initialize(&db).unwrap();
    let local = SqliteReads {
        journal: LocalJournal {
            connection: &db,
            root_object_id: Some("root".into()),
        },
        contracts: &Catalog,
        parse: |raw: &str| parse(raw, MANAGEMENT_FRAME_BYTES).map_err(Failure::from),
        sqlite_failure: |_: ivy_store::rusqlite::Error| {
            Failure::from(IvyError::new("database_error", "sqlite"))
        },
    };
    let objects = Objects {
        value: RefCell::new(snapshot()),
        calls: RefCell::default(),
    };
    let comments = TaskComments::new(Arc::new(NoComments), tokio::runtime::Handle::current());
    let scopes = ReadScopes::default();
    let source_failure = |e: IvyError| Failure::from(e);
    let store: PersistentReadStore<'_, _, _, _, _, _, _, Failure> = PersistentReadStore {
        local: &local,
        objects: &objects,
        policies: &Policies,
        comments: &comments,
        source_failure: &source_failure,
        cache: Some(scopes.cache()),
    };
    scopes
        .with_read_cache(async {
            let mut first = store
                .read_document("task-board/history", &pin(), true, false, false)
                .await
                .unwrap();
            first.insert("value", "caller mutation".into());
            let second = store
                .read_document("task-board/history", &pin(), true, false, false)
                .await
                .unwrap();
            assert_eq!(
                second.required("value").unwrap().get("event"),
                Some(&"saved".into())
            );
            assert_eq!(objects.calls.borrow().len(), 1);
            store
                .read_document("task-board/history", &pin(), true, false, true)
                .await
                .unwrap();
            assert_eq!(
                objects.calls.borrow().len(),
                2,
                "header reads have separate cache entries"
            );
        })
        .await;
    scopes
        .with_read_cache(async {
            store
                .read_document("task-board/history", &pin(), true, false, false)
                .await
                .unwrap();
        })
        .await;
    assert_eq!(
        objects.calls.borrow().len(),
        3,
        "independent actions read again"
    );
    for (field, value, expected) in [
        ("parentId", "foreign".into(), "task_board_scope_mismatch"),
        (
            "effectivelyArchived",
            true.into(),
            "task_board_record_archived",
        ),
        (
            "contractKey",
            "foreign".into(),
            "task_board_contract_mismatch",
        ),
    ] {
        let mut response = snapshot();
        let mut object = response.remove("object").unwrap();
        object.insert(field, value);
        response.insert("object", object);
        *objects.value.borrow_mut() = response;
        assert_eq!(
            code(
                &store
                    .read_document("task-board/history", &pin(), true, false, false)
                    .await
                    .unwrap_err()
            ),
            expected
        );
    }
    for (field, value, expected) in [
        (
            "revision",
            Json::Number(8.0),
            "task_board_evidence_mismatch",
        ),
        (
            "contractVersion",
            "unsupported".into(),
            "task_board_contract_mismatch",
        ),
    ] {
        let mut response = snapshot();
        let mut revision = response.remove("revision").unwrap();
        revision.insert(field, value);
        response.insert("revision", revision);
        *objects.value.borrow_mut() = response;
        assert_eq!(
            code(
                &store
                    .read_document("task-board/history", &pin(), true, false, false)
                    .await
                    .unwrap_err()
            ),
            expected
        );
    }
}
#[test]
fn local_documents_select_exact_revisions_and_preserve_native_database_errors() {
    let db = Connection::open_in_memory().unwrap();
    initialize(&db).unwrap();
    let value = Json::object([("phase", "prepared".into())]);
    db.execute(
        "INSERT INTO documents VALUES(?,'task-board/operation',?,'root',2,?,?)",
        [
            "operation",
            "operation-name",
            "2026-01-01T00:00:00.000Z",
            "2026-01-01T00:00:00.000Z",
        ],
    )
    .unwrap();
    for revision in [1, 2] {
        db.execute(
            "INSERT INTO revisions VALUES(?,?,?,?)",
            ivy_store::rusqlite::params![
                "operation",
                revision,
                canonical(&value, MANAGEMENT_FRAME_BYTES).unwrap(),
                "2026-01-01T00:00:00.000Z"
            ],
        )
        .unwrap();
    }
    let local = SqliteReads {
        journal: LocalJournal {
            connection: &db,
            root_object_id: Some("root".into()),
        },
        contracts: &Catalog,
        parse: |raw: &str| parse(raw, MANAGEMENT_FRAME_BYTES),
        sqlite_failure: |_: ivy_store::rusqlite::Error| {
            IvyError::new("native_sqlite", "original classifier")
        },
    };
    let requested = Json::object([
        ("objectId", "operation".into()),
        ("revision", Json::Number(1.0)),
    ]);
    let stored = local
        .document::<IvyError>("task-board/operation", &requested, true)
        .unwrap();
    assert_eq!(stored.get("pin"), Some(&requested));
    assert_eq!(stored.get("value"), Some(&value));
    assert_eq!(
        stored.required("metadata").unwrap().get("currentRevision"),
        Some(&Json::Number(2.0))
    );
    let foreign = SqliteReads {
        journal: LocalJournal {
            connection: &db,
            root_object_id: None,
        },
        ..local
    };
    assert_eq!(
        foreign
            .document::<IvyError>("task-board/operation", &requested, true)
            .unwrap_err()
            .code,
        "task_board_scope_mismatch"
    );
    let unavailable = Json::object([
        ("objectId", "operation".into()),
        ("revision", Json::Number(3.0)),
    ]);
    assert_eq!(
        foreign
            .document::<IvyError>("task-board/operation", &unavailable, false)
            .unwrap_err()
            .code,
        "task_board_evidence_mismatch"
    );
    db.execute_batch("DROP TABLE revisions").unwrap();
    assert_eq!(
        foreign
            .document::<IvyError>("task-board/operation", &requested, false)
            .unwrap_err()
            .code,
        "native_sqlite"
    );
}

struct NoDocuments;
impl ivy_task_board::journal_store::persistent_reads::local_history::HistoryDocuments<Failure>
    for NoDocuments
{
    async fn history_document(
        &self,
        _: &str,
        _: Option<&Json>,
        _: bool,
        _: bool,
    ) -> Result<Json, Failure> {
        panic!("scheduler evidence uses its local document")
    }
}
struct RetentionSource {
    archived: bool,
    reads: std::cell::Cell<usize>,
}
impl ObjectReads<Failure> for RetentionSource {
    async fn read(&self, params: Option<Json>) -> Result<Json, Failure> {
        assert_eq!(params.unwrap().get("objectId"), Some(&"task".into()));
        self.reads.set(self.reads.get() + 1);
        Ok(Json::object([
            (
                "object",
                Json::object([
                    ("contractKey", "task-board/task".into()),
                    ("effectivelyArchived", self.archived.into()),
                    ("currentRevision", Json::Number(7.0)),
                ]),
            ),
            (
                "content",
                Json::object([
                    ("encoding", "json".into()),
                    ("value", Json::object([("workflowState", "todo".into())])),
                ]),
            ),
        ]))
    }
}
impl ivy_task_board::journal_store::persistent_reads::local_history::HistoryQueries<Failure>
    for RetentionSource
{
    async fn query(&self, _: Json) -> Result<Json, Failure> {
        Ok(Json::object([
            ("items", Json::Array(vec![])),
            ("nextCursor", Json::Null),
        ]))
    }
}
#[tokio::test(flavor = "current_thread")]
async fn history_retains_active_unarchived_task_evidence_and_collects_archived_evidence() {
    use ivy_task_board::journal_store::persistent_reads::local_history::{
        HistoryCollections, LocalHistory,
    };
    use ivy_task_board::local_retention::HistoryLimits;
    for archived in [false, true] {
        let db = Connection::open_in_memory().unwrap();
        initialize(&db).unwrap();
        let value = Json::object([(
            "task",
            Json::object([("objectId", "task".into()), ("revision", Json::Number(7.0))]),
        )]);
        db.execute("INSERT INTO documents VALUES('attempt','task-board/scheduler-attempt','attempt','root',1,?,?)",
            ["2026-01-01T00:00:00.000Z","2026-01-01T00:00:00.000Z"]).unwrap();
        db.execute(
            "INSERT INTO revisions VALUES('attempt',1,?,?)",
            [
                canonical(&value, MANAGEMENT_FRAME_BYTES).unwrap(),
                "2026-01-01T00:00:00.000Z".into(),
            ],
        )
        .unwrap();
        db.execute(
            "INSERT INTO cleanup_roots VALUES('attempt',?)",
            ["2026-01-01T00:00:00.000Z"],
        )
        .unwrap();
        let local = SqliteReads {
            journal: LocalJournal {
                connection: &db,
                root_object_id: Some("root".into()),
            },
            contracts: &Catalog,
            parse: |raw: &str| parse(raw, MANAGEMENT_FRAME_BYTES).map_err(Failure::from),
            sqlite_failure: |_: ivy_store::rusqlite::Error| {
                Failure::from(IvyError::new("database_error", "sqlite"))
            },
        };
        let source = RetentionSource {
            archived,
            reads: std::cell::Cell::new(0),
        };
        let collections = HistoryCollections::default();
        let history = LocalHistory::new(
            &local,
            &NoDocuments,
            &source,
            &collections,
            HistoryLimits { count: 0, bytes: 0 },
        );
        history.collect::<()>(0, 0, None, None).await.unwrap();
        let remaining: i64 = db
            .query_row("SELECT COUNT(*) FROM documents", [], |row| row.get(0))
            .unwrap();
        assert_eq!(remaining, if archived { 0 } else { 1 });
        assert!(source.reads.get() > 0);
    }
}
