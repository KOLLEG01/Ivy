use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString,
    json::{canonical, parse},
    limits::MANAGEMENT_FRAME_BYTES,
};
use ivy_sdk::task_comments::{CancellationToken, CommentReadFuture, CommentSource};
use ivy_store::rusqlite::Connection;
use ivy_task_board::{
    engine_read::read_failure::{ReadCause, ReadFailure},
    engine_read::read_policy::{QuestionPolicies, TaskReadStore},
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
        Ok(matches!(
            key,
            "task-board/operation" | "task-board/prepared-payload"
        ))
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
    fn open_questions(&self, _: &Json) -> Result<Vec<Json>, Failure> {
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
struct MovingObjects(RefCell<Option<Json>>);
impl ObjectReads<Failure> for MovingObjects {
    async fn read(&self, _: Option<Json>) -> Result<Json, Failure> {
        tokio::task::yield_now().await;
        Ok(self
            .0
            .borrow_mut()
            .take()
            .expect("one original source read"))
    }
}
#[tokio::test(flavor = "current_thread")]
async fn remote_owned_payloads_reach_readers_without_an_identity_copy() {
    let db = Connection::open_in_memory().unwrap();
    initialize(&db).unwrap();
    let local = SqliteReads {
        journal: LocalJournal {
            connection: &db,
            root_object_id: Some("root".into()),
        },
        contracts: &Catalog,
        parse: |raw: &str| parse(raw, MANAGEMENT_FRAME_BYTES).map_err(Failure::from),
        sqlite_failure: |_: ivy_store::rusqlite::Error| Failure::from(IvyError::internal()),
    };
    let comments = TaskComments::new(Arc::new(NoComments), tokio::runtime::Handle::current());
    let source_failure = Failure::from;
    // Ordinary and retained documents, then the domain value reader. The source
    // relinquishes ownership once; neither history mode needs an upgrade or tree.
    for (mode, cached) in (0..3).flat_map(|mode| [false, true].map(|cached| (mode, cached))) {
        let mut original = snapshot();
        let payload = Json::Object(vec![
            (
                "opaque".into(),
                Json::String(JsString(vec![0xd800, 0, 0xdfff, 65].repeat(65536))),
            ),
            ("duplicate".into(), "first".into()),
            ("duplicate".into(), "last".into()),
        ]);
        let units = payload.required("opaque").unwrap().as_js_string().unwrap();
        let original_pointer = units.0.as_ptr();
        let original_length = units.0.len();
        original
            .get_mut("content")
            .unwrap()
            .insert("value", payload);
        let metadata = Json::String(JsString(vec![0xd800, 66].repeat(32768)));
        let metadata_pointer = metadata.as_js_string().unwrap().0.as_ptr();
        original
            .get_mut("object")
            .unwrap()
            .insert("opaqueMetadata", metadata);
        let objects = MovingObjects(RefCell::new(Some(original)));
        let scopes = ReadScopes::default();
        let store: PersistentReadStore<'_, _, _, _, _, _, _, Failure> = PersistentReadStore {
            local: &local,
            objects: &objects,
            policies: &Policies,
            comments: &comments,
            source_failure: &source_failure,
            cache: cached.then_some(scopes.cache()),
        };
        // Oversized records must keep their original buffers when an active
        // action cache declines admission, as well as when no cache is present.
        let output = scopes
            .with_read_cache(async {
                if mode == 2 {
                    TaskReadStore::read(&store, "task-board/history", &pin(), false)
                        .await
                        .unwrap()
                } else {
                    let mut document = store
                        .read_document("task-board/history", &pin(), true, false, mode == 1)
                        .await
                        .unwrap();
                    assert_eq!(document.get("pin"), Some(&pin()));
                    assert_eq!(
                        document
                            .required("metadata")
                            .unwrap()
                            .required("opaqueMetadata")
                            .unwrap()
                            .as_js_string()
                            .unwrap()
                            .0
                            .as_ptr(),
                        metadata_pointer
                    );
                    document.remove("value").unwrap()
                }
            })
            .await;
        assert!(objects.0.borrow().is_none());
        let units = output.required("opaque").unwrap().as_js_string().unwrap();
        assert_eq!(
            units.0.as_ptr(),
            original_pointer,
            "mode {mode} copied the original payload"
        );
        assert_eq!(units.0.len(), original_length);
        assert!(
            units
                .0
                .chunks_exact(4)
                .all(|part| part == [0xd800, 0, 0xdfff, 65])
        );
        let fields = output.as_object().unwrap();
        assert_eq!(fields.len(), 3);
        assert_eq!(fields[1], ("duplicate".into(), "first".into()));
        assert_eq!(fields[2], ("duplicate".into(), "last".into()));
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
                .read_document("task-board/history", &pin(), true, true, false)
                .await
                .unwrap();
            first.insert("value", "caller mutation".into());
            let second = store
                .read_document("task-board/history", &pin(), true, true, false)
                .await
                .unwrap();
            assert_eq!(
                second.required("value").unwrap().get("event"),
                Some(&"saved".into())
            );
            assert_eq!(objects.calls.borrow().len(), 1);
            let mut selected = TaskReadStore::read(&store, "task-board/history", &pin(), false)
                .await
                .unwrap();
            selected.insert("event", "caller mutation".into());
            let selected = TaskReadStore::read(&store, "task-board/history", &pin(), false)
                .await
                .unwrap();
            assert_eq!(selected.get("event"), Some(&"saved".into()));
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
    let mut expanded = snapshot();
    let mut content = expanded.remove("content").unwrap();
    content.insert("value", Json::Array(vec![Json::Null; 4096]));
    expanded.insert("content", content);
    assert!(canonical(&expanded, MANAGEMENT_FRAME_BYTES).unwrap().len() < 65536);
    *objects.value.borrow_mut() = expanded;
    scopes
        .with_read_cache(async {
            for _ in 0..2 {
                store
                    .read_document("task-board/history", &pin(), true, false, false)
                    .await
                    .unwrap();
            }
        })
        .await;
    assert_eq!(
        objects.calls.borrow().len(),
        5,
        "expanded documents are read without retaining an oversized cache entry"
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

#[tokio::test(flavor = "current_thread")]
async fn compact_task_snapshot_resumes_through_the_current_reader_with_original_hash_and_scope() {
    let db = Connection::open_in_memory().unwrap();
    initialize(&db).unwrap();
    let journal = LocalJournal {
        connection: &db,
        root_object_id: Some("root".into()),
    };
    let task = Json::object([
        (
            "publication",
            Json::object([
                ("objectId", "operation".into()),
                ("revision", Json::Number(1.0)),
            ]),
        ),
        ("comments", Json::Array(vec![])),
        ("commentDeliveries", Json::Array(vec![])),
    ]);
    let mut header = task.clone();
    header.insert(
        "commentStore",
        Json::object([
            ("kind", "tree".into()),
            ("root", Json::Null),
            ("length", Json::Number(0.0)),
            (
                "summary",
                Json::object([
                    ("directCount", Json::Number(0.0)),
                    ("questions", Json::Array(vec![])),
                ]),
            ),
        ]),
    );
    let payload = Json::object([
        ("schemaVersion", Json::Number(1.0)),
        ("contractKey", "task-board/task".into()),
        ("contractVersion", "1.0.0".into()),
        ("content", header.clone()),
        ("references", Json::Object(vec![])),
    ]);
    let address = journal
        .create(
            "task-board/prepared-payload",
            &"Prepared".into(),
            Some(&"operation".into()),
            &payload,
            &"2026-01-01T00:00:00.000Z".into(),
            &"payload".into(),
        )
        .unwrap();
    let hash = ivy_contracts::hash::hash_json(&payload).unwrap();
    let operation = Json::object([
        // A create operation does not know its Task ID at initial admission.
        ("request", Json::object([("action", "create".into())])),
        (
            "writes",
            Json::Array(vec![Json::object([
                ("contractKey", "task-board/task".into()),
                ("contractVersion", "1.0.0".into()),
                ("payloadContract", "task-board/prepared-payload".into()),
                (
                    "destination",
                    Json::object([
                        ("objectId", "task".into()),
                        ("expectedRevision", Json::Number(1.0)),
                    ]),
                ),
                (
                    "payload",
                    Json::object([
                        ("object", address.clone()),
                        ("mediaType", "application/json".into()),
                        ("contentHash", hash.clone().into()),
                    ]),
                ),
            ])]),
        ),
    ]);
    journal
        .create(
            "task-board/operation",
            &"Operation".into(),
            Some(&"root".into()),
            &operation,
            &"2026-01-01T00:00:00.000Z".into(),
            &"operation".into(),
        )
        .unwrap();
    let local = SqliteReads {
        journal,
        contracts: &Catalog,
        parse: |raw: &str| parse(raw, MANAGEMENT_FRAME_BYTES).map_err(Failure::from),
        sqlite_failure: |_: ivy_store::rusqlite::Error| {
            Failure::from(IvyError::new("database_error", "sqlite"))
        },
    };
    let objects = Objects {
        value: RefCell::new(Json::Null),
        calls: RefCell::default(),
    };
    let comments = TaskComments::new(Arc::new(NoComments), tokio::runtime::Handle::current());
    let source_failure = |e: IvyError| Failure::from(e);
    let store: PersistentReadStore<'_, _, _, _, _, _, _, Failure> = PersistentReadStore {
        local: &local,
        objects: &objects,
        policies: &Policies,
        comments: &comments,
        source_failure: &source_failure,
        cache: None,
    };
    for retained in [false, true] {
        let read = store
            .read_document("task-board/task", &address, false, false, retained)
            .await
            .unwrap();
        assert_eq!(read.get("key"), Some(&"task-board/prepared-payload".into()));
        assert_eq!(
            canonical(read.required("value").unwrap(), MANAGEMENT_FRAME_BYTES).unwrap(),
            canonical(
                if retained { &header } else { &task },
                MANAGEMENT_FRAME_BYTES
            )
            .unwrap(),
        );
        assert_eq!(
            read.required("revision").unwrap().get("contentHash"),
            Some(&Json::from(hash.clone()))
        );
    }
    assert_eq!(
        code(
            &store
                .read_document("task-board/task", &address, true, false, true)
                .await
                .unwrap_err()
        ),
        "task_board_scope_mismatch"
    );
    assert!(
        objects.calls.borrow().is_empty(),
        "A local prepared Task never falls through to a remote read."
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
