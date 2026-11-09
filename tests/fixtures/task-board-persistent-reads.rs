use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString,
    json::{canonical, parse, stringify},
    limits::MANAGEMENT_FRAME_BYTES,
};
use ivy_sdk::rpc::{RequestFuture, RequestOptions, RpcClient};
use ivy_store::rusqlite::{Connection, params};
use ivy_task_board::{
    engine_read::condition_policy::{ConditionStore, task_condition},
    journal_store::{
        JournalStore,
        persistent_reads::{
            fill_verification_sizes, read_legacy_conversation, read_legacy_conversation_owned,
        },
    },
    local_journal::{LocalJournal, initialize},
    local_retention::HistoryLimits,
};
use std::{
    collections::VecDeque,
    future::Future,
    pin::pin,
    sync::{Arc, Mutex},
    task::{Context, Poll, Wake},
};
fn field<'a>(v: &'a Json, k: &str) -> &'a Json {
    static NULL: Json = Json::Null;
    v.get(k).unwrap_or(&NULL)
}
fn items(v: &Json) -> &[Json] {
    v.as_array().unwrap_or(&[])
}
fn text(v: &Json, k: &str) -> String {
    field(v, k).as_str().unwrap()
}
fn fixture() -> Json {
    parse(
        &std::fs::read_to_string(std::env::var("IVY_TASK_BOARD_PERSISTENT_READS_FIXTURE").unwrap())
            .unwrap(),
        MANAGEMENT_FRAME_BYTES,
    )
    .unwrap()
}
fn same(a: &Json, b: &Json, name: &str) {
    assert_eq!(
        canonical(a, MANAGEMENT_FRAME_BYTES).unwrap(),
        canonical(b, MANAGEMENT_FRAME_BYTES).unwrap(),
        "{name}"
    );
}
fn observed(v: IvyResult<Json>) -> Json {
    match v {
        Ok(value) => Json::object([("value", value)]),
        Err(e) => Json::object([("error", e.to_wire())]),
    }
}
struct NoWake;
impl Wake for NoWake {
    fn wake(self: Arc<Self>) {}
}
fn complete<T>(work: impl Future<Output = T>) -> T {
    let mut work = pin!(work);
    let waker = Arc::new(NoWake).into();
    for _ in 0..128 {
        if let Poll::Ready(v) = work.as_mut().poll(&mut Context::from_waker(&waker)) {
            return v;
        }
    }
    panic!("Finite source receiver blocked")
}
struct Client {
    remaining: Mutex<VecDeque<Json>>,
    trace: Mutex<Vec<Json>>,
}
impl Client {
    fn new(input: &Json) -> Self {
        Self {
            remaining: Mutex::new(items(field(input, "rpc")).iter().cloned().collect()),
            trace: Mutex::default(),
        }
    }
}
impl RpcClient for Client {
    fn request<'a>(
        &'a self,
        method: &'a str,
        params: Json,
        options: RequestOptions,
    ) -> RequestFuture<'a> {
        Box::pin(async move {
            assert_eq!(
                method, "objects.read",
                "No write or native effect is allowed"
            );
            assert!(options.bound_read.is_none());
            assert!(!options.cancellation.is_cancelled());
            self.trace.lock().unwrap().push(Json::object([
                ("method", method.into()),
                ("params", params),
            ]));
            let next = self
                .remaining
                .lock()
                .unwrap()
                .pop_front()
                .expect("Unexpected read");
            if let Some(error) = next.get("error") {
                let mut e = IvyError::new_js(
                    field(error, "code").as_js_string().unwrap().clone(),
                    field(error, "message").as_js_string().unwrap().clone(),
                );
                e = e.with_provider_outcome(field(error, "outcome").clone());
                e.details = error.get("details").cloned();
                return Err(e);
            }
            Ok(field(&next, "value").clone())
        })
    }
}
fn seed(db: &Connection, input: &Json) {
    initialize(db).unwrap();
    for row in items(field(input, "rows")) {
        let (id, at, revision) = (
            text(row, "id"),
            text(row, "updatedAt"),
            field(row, "revision").as_f64().unwrap(),
        );
        db.execute("INSERT INTO documents(id,key,name,parent_id,current_revision,created_at,updated_at) VALUES(?,?,?,NULL,?,?,?)",params![id,text(row,"key"),id,revision,at,at]).unwrap();
        let raw = row
            .get("raw")
            .map(|v| v.as_str().unwrap())
            .unwrap_or_else(|| stringify(field(row, "value"), MANAGEMENT_FRAME_BYTES).unwrap());
        db.execute(
            "INSERT INTO revisions(id,revision,value_json,created_at) VALUES(?,?,?,?)",
            params![id, revision, raw, at],
        )
        .unwrap();
        for older in items(field(row, "older")) {
            db.execute(
                "INSERT INTO revisions(id,revision,value_json,created_at) VALUES(?,?,?,?)",
                params![
                    id,
                    field(older, "revision").as_f64().unwrap(),
                    stringify(field(older, "value"), MANAGEMENT_FRAME_BYTES).unwrap(),
                    at
                ],
            )
            .unwrap();
        }
    }
}
fn journal_state(db: &Connection) -> (i64, i64, i64) {
    (
        db.query_row("SELECT count(*) FROM documents", [], |r| r.get(0))
            .unwrap(),
        db.query_row("SELECT count(*) FROM revisions", [], |r| r.get(0))
            .unwrap(),
        db.query_row("SELECT sum(bytes) FROM storage_usage", [], |r| r.get(0))
            .unwrap(),
    )
}
fn lookup(db: &Connection, input: &Json) -> IvyResult<Option<Json>> {
    let client = Arc::new(Client::new(&Json::Null));
    let store = JournalStore {
        journal: LocalJournal {
            connection: db,
            root_object_id: Some("root".into()),
        },
        client: client.clone(),
        contracts: (),
        limits: HistoryLimits::default(),
        now: || -> String { panic!("A retained read cannot use the clock") },
        new_id: || -> JsString { panic!("A retained read cannot allocate an identity") },
        cache: None,
    };
    let run = input
        .get("runId")
        .and_then(Json::as_js_string)
        .cloned()
        .unwrap_or_else(|| "run".into());
    let result = store.failed_handoff(&run);
    if field(input, "missingTables").as_bool() == Some(true) {
        // The existing condition caller must propagate this actual SQL failure
        // even when publication would otherwise have higher display precedence.
        let task = Json::object([
            ("workflowState", "todo".into()),
            ("fields", Json::object([("control", "agent".into())])),
            ("publication", Json::Object(vec![])),
            (
                "completionIssue",
                Json::object([
                    ("run", Json::object([("objectId", Json::String(run))])),
                    ("detail", "Older detail".into()),
                ]),
            ),
        ]);
        assert_eq!(
            task_condition(&store, &task, None).unwrap_err(),
            result.clone().unwrap_err()
        );
    }
    assert!(client.trace.lock().unwrap().is_empty());
    result
}
#[test]
fn current_failed_handoff_binds_the_retained_journal() {
    let fixture = fixture();
    let mut count = 0;
    let mut persisted = None;
    for case in items(field(&fixture, "cases"))
        .iter()
        .filter(|c| field(c, "kind") == &Json::from("sql"))
    {
        let input = field(case, "input");
        let name = text(case, "name");
        let persist = name == "current revision controls selection";
        let path =
            std::path::PathBuf::from(std::env::var("IVY_TASK_BOARD_PERSISTENT_JOURNAL").unwrap());
        if persist {
            assert!(
                !path.exists(),
                "Use a new output path for each attempted check"
            );
        }
        let db = if persist {
            Connection::open(&path).unwrap()
        } else {
            Connection::open_in_memory().unwrap()
        };
        let missing = field(input, "missingTables").as_bool() == Some(true);
        if !missing {
            seed(&db, input);
        }
        let before = if missing {
            None
        } else {
            Some(journal_state(&db))
        };
        let actual = observed(lookup(&db, input).map(|v| v.unwrap_or(Json::Null)));
        same(&actual, field(case, "output"), &name);
        if let Some(before) = before {
            assert_eq!(journal_state(&db), before, "Lookup wrote to the journal");
        }
        if persist {
            let integrity: String = db
                .query_row("PRAGMA quick_check", [], |r| r.get(0))
                .unwrap();
            assert_eq!(integrity, "ok");
            persisted = Some((path.clone(), input.clone(), actual.clone(), before.unwrap()));
        }
        db.close().unwrap();
        count += 1;
    }
    assert_eq!(count, 19);
    let (path, input, expected, before) = persisted.unwrap();
    let db =
        Connection::open_with_flags(path, ivy_store::rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    same(
        &observed(lookup(&db, &input).map(|v| v.unwrap_or(Json::Null))),
        &expected,
        "Exact current revision after close/reopen",
    );
    assert_eq!(journal_state(&db), before);
}
#[test]
fn current_retained_prefix_reads_match_the_original() {
    let fixture = fixture();
    let mut count = 0;
    for case in items(field(&fixture, "cases"))
        .iter()
        .filter(|c| field(c, "kind") == &Json::from("prefix"))
    {
        let input = field(case, "input");
        let before = input.clone();
        let client = Client::new(input);
        let task_id = field(input, "taskId")
            .as_js_string()
            .cloned()
            .unwrap_or_else(|| "task".into());
        let actual = observed(
            complete(read_legacy_conversation(
                &client,
                field(input, "task"),
                &task_id,
            ))
            .map(|v| v.expect("Tree was not selected")),
        );
        same(&actual, field(case, "output"), &text(case, "name"));
        same(
            &Json::Array(client.trace.lock().unwrap().clone()),
            field(case, "trace"),
            "Original immutable-prefix read trace",
        );
        assert_eq!(
            client.remaining.lock().unwrap().len() as f64,
            field(case, "rpcRemaining").as_f64().unwrap()
        );
        assert_eq!(input, &before, "Unchanged prefix source input");
        count += 1;
    }
    assert_eq!(count, 21);
    let tree = Json::object([
        ("comments", Json::Array(vec![])),
        (
            "commentStore",
            Json::object([
                ("kind", "tree".into()),
                ("root", Json::Null),
                ("length", Json::Number(0.0)),
            ]),
        ),
    ]);
    let client = Client::new(&Json::Null);
    assert!(
        complete(read_legacy_conversation(&client, &tree, &"task".into()))
            .unwrap()
            .is_none()
    );
    assert!(client.trace.lock().unwrap().is_empty());
}
#[test]
fn current_verification_size_backfill_matches_the_owned_boundary() {
    let fixture = fixture();
    let mut count = 0;
    for case in items(field(&fixture, "cases"))
        .iter()
        .filter(|c| field(c, "kind") == &Json::from("sizes"))
    {
        let input = field(case, "input");
        let before = input.clone();
        let client = Client::new(input);
        let key = input
            .get("key")
            .map(|v| v.as_str().unwrap())
            .unwrap_or_else(|| "task-board/task".into());
        let result = complete(fill_verification_sizes(&client, &key, field(input, "task")));
        let boundary = match &result {
            Ok(value) => Json::Array(vec![Json::object([
                ("key", key.clone().into()),
                ("value", value.clone()),
            ])]),
            Err(_) => Json::Array(vec![]),
        };
        same(
            &observed(result),
            field(case, "output"),
            &text(case, "name"),
        );
        same(
            &boundary,
            field(case, "upgradeBoundary"),
            "Exact value at the separate Shared upgrader boundary",
        );
        same(
            &Json::Array(client.trace.lock().unwrap().clone()),
            field(case, "trace"),
            "Original verification read order and pins",
        );
        assert_eq!(
            client.remaining.lock().unwrap().len() as f64,
            field(case, "rpcRemaining").as_f64().unwrap()
        );
        same(input, &before, "Unchanged original verification input");
        count += 1;
    }
    assert_eq!(count, 12);
}

#[test]
fn prefix_boundaries_preserve_payloads_errors_and_original_read_order() {
    // Keep boundary cases runnable without externally captured fixture data.
    let first = Json::object([
        ("commentId", "c1".into()),
        ("body", Json::String(JsString(vec![0xd800, 65, 0, 0xdfff]))),
        ("nativeInput", "stale".into()),
        ("handoff", "stale".into()),
        ("delivery", "stale".into()),
    ]);
    let second = Json::object([
        ("commentId", "c2".into()),
        ("body", "outside the selected prefix".into()),
    ]);
    let states = Json::Array(vec![
        Json::object([("commentId", "c1".into()), ("delivery", "earlier".into())]),
        Json::object([
            ("commentId", "c1".into()),
            ("delivery", "latest".into()),
            ("handoff", Json::object([("workflowState", "todo".into())])),
        ]),
    ]);
    let original = Json::object([
        ("comments", Json::Array(vec![])),
        ("commentDeliveries", Json::Array(vec![])),
        (
            "opaque",
            Json::Object(vec![
                ("same".into(), (-0.0).into()),
                ("same".into(), "retained".into()),
            ]),
        ),
        (
            "commentStore",
            Json::object([
                ("objectId", "log".into()),
                ("length", 1.0.into()),
                ("nativeInputs", states),
            ]),
        ),
    ]);
    let saved = Json::object([
        (
            "object",
            Json::object([
                ("contractKey", "task-board/comments".into()),
                ("ownerObjectId", Json::Null),
                ("parentId", "task".into()),
            ]),
        ),
        (
            "content",
            Json::object([
                ("encoding", "json".into()),
                (
                    "value",
                    Json::object([
                        ("taskId", "task".into()),
                        ("comments", Json::Array(vec![first.clone(), second.clone()])),
                    ]),
                ),
            ]),
        ),
    ]);
    let mut expected_comment = first;
    for key in ["nativeInput", "handoff", "delivery"] {
        expected_comment.remove(key);
    }
    expected_comment.insert("delivery", "latest".into());
    expected_comment.insert("handoff", Json::object([("workflowState", "todo".into())]));
    let missing = |key| {
        IvyError::new(
            "invalid_arguments",
            format!("Missing required property: {key}"),
        )
    };
    let mismatch = || {
        IvyError::new(
            "task_board_contract_mismatch",
            "The ticket conversation is not a retained comment record.",
        )
    };
    let unavailable = || {
        IvyError::new(
            "task_board_evidence_mismatch",
            "The ticket conversation prefix is unavailable.",
        )
    };
    let source_error = IvyError::new_js(JsString(vec![0xd800, 65]), JsString(vec![0xdfff, 66]))
        .with_provider_outcome(Json::object([("opaque", "original".into())]))
        .with_details(Json::object([("cause", "retained".into())]));
    let names = [
        "inline",
        "null-store",
        "tree-selector",
        "retained-prefix",
        "zero-length",
        "full-prefix",
        "reference-before-read",
        "original-source-error",
        "contract-before-length",
        "missing-content-before-contract",
        "wrong-task",
        "wrong-parent",
        "invalid-comments",
        "missing-length",
        "negative-length",
        "fractional-length",
        "nan-length",
        "infinite-length",
        "overflow-length",
        "missing-native-inputs",
        "length-before-native-inputs",
        "invalid-native-inputs",
        "malformed-selected-state",
    ];
    for (index, name) in names.into_iter().enumerate() {
        let mut task = original.clone();
        let mut reply = saved.clone();
        let mut expected_task = original.clone();
        expected_task.remove("commentStore");
        expected_task.insert("comments", Json::Array(vec![expected_comment.clone()]));
        let mut expected = Ok(expected_task);
        let mut read_count = 1;
        let mut source_failure = false;
        match index {
            0 => {
                task.remove("commentStore");
                expected = Ok(task.clone());
                read_count = 0;
            }
            1 => {
                task.insert("commentStore", Json::Null);
                expected = Ok(task.clone());
                read_count = 0;
            }
            2 => {
                task.get_mut("commentStore")
                    .unwrap()
                    .insert("kind", "tree".into());
                expected = Ok(Json::Null);
                read_count = 0;
            }
            3 => {}
            4 => {
                task.get_mut("commentStore")
                    .unwrap()
                    .insert("length", (-0.0).into());
                expected
                    .as_mut()
                    .unwrap()
                    .insert("comments", Json::Array(vec![]));
            }
            5 => {
                task.get_mut("commentStore")
                    .unwrap()
                    .insert("length", 2.0.into());
                expected.as_mut().unwrap().insert(
                    "comments",
                    Json::Array(vec![expected_comment.clone(), second.clone()]),
                );
            }
            6 => {
                let reference = task.get_mut("commentStore").unwrap();
                reference.remove("objectId");
                reference.insert("length", (-1.0).into());
                expected = Err(missing("objectId"));
                read_count = 0;
            }
            7 => {
                source_failure = true;
                expected = Err(source_error.clone());
            }
            8 => {
                reply
                    .get_mut("object")
                    .unwrap()
                    .insert("contractKey", "wrong".into());
                task.get_mut("commentStore").unwrap().remove("length");
                expected = Err(mismatch());
            }
            9 => {
                reply
                    .get_mut("object")
                    .unwrap()
                    .insert("contractKey", "wrong".into());
                reply.remove("content");
                expected = Err(missing("content"));
            }
            10 => {
                reply
                    .get_mut("content")
                    .unwrap()
                    .get_mut("value")
                    .unwrap()
                    .insert("taskId", "other".into());
                expected = Err(unavailable());
            }
            11 => {
                reply
                    .get_mut("object")
                    .unwrap()
                    .insert("parentId", "other".into());
                expected = Err(unavailable());
            }
            12 => {
                reply
                    .get_mut("content")
                    .unwrap()
                    .get_mut("value")
                    .unwrap()
                    .insert("comments", Json::Null);
                expected = Err(unavailable());
            }
            13 => {
                task.get_mut("commentStore").unwrap().remove("length");
                expected = Err(unavailable());
            }
            14..=18 => {
                let lengths = [-1.0, 0.5, f64::NAN, f64::INFINITY, 3.0];
                task.get_mut("commentStore")
                    .unwrap()
                    .insert("length", lengths[index - 14].into());
                expected = Err(unavailable());
            }
            19 => {
                task.get_mut("commentStore").unwrap().remove("nativeInputs");
                expected = Err(missing("nativeInputs"));
            }
            20 => {
                let reference = task.get_mut("commentStore").unwrap();
                reference.remove("nativeInputs");
                reference.insert("length", (-1.0).into());
                expected = Err(unavailable());
            }
            21 => {
                task.get_mut("commentStore")
                    .unwrap()
                    .insert("nativeInputs", Json::Null);
                expected = Err(unavailable());
            }
            22 => {
                task.get_mut("commentStore")
                    .unwrap()
                    .insert("nativeInputs", Json::Array(vec![Json::Null]));
                let log = reply.get_mut("content").unwrap().get_mut("value").unwrap();
                log.insert(
                    "comments",
                    Json::Array(vec![Json::object([
                        ("nativeInput", "stale".into()),
                        ("body", "retained".into()),
                    ])]),
                );
                expected = Err(unavailable());
            }
            _ => unreachable!(),
        }
        let expected = observed(expected);
        let row = if source_failure {
            Json::object([(
                "error",
                Json::object([
                    ("code", Json::String(source_error.code_js())),
                    ("message", Json::String(source_error.message_js())),
                    ("outcome", source_error.outcome.to_json()),
                    ("details", source_error.details.clone().unwrap()),
                ]),
            )])
        } else {
            Json::object([("value", reply)])
        };
        let input = Json::object([("rpc", Json::Array(vec![row]))]);
        let original_input = format!("{task:?}");
        for owned in [false, true] {
            let client = Client::new(&input);
            let result = if owned {
                complete(read_legacy_conversation_owned(
                    &client,
                    task.clone(),
                    &"task".into(),
                ))
            } else {
                complete(read_legacy_conversation(&client, &task, &"task".into()))
            };
            same(
                &observed(result.map(|value| value.unwrap_or(Json::Null))),
                &expected,
                name,
            );
            let trace = client.trace.lock().unwrap();
            assert_eq!(trace.len(), read_count, "{name}, owned={owned}");
            if read_count == 1 {
                assert_eq!(
                    trace[0],
                    Json::object([
                        ("method", "objects.read".into()),
                        ("params", Json::object([("objectId", "log".into())]))
                    ]),
                    "{name}"
                );
            }
            assert_eq!(
                client.remaining.lock().unwrap().len(),
                1 - read_count,
                "{name}"
            );
            assert_eq!(format!("{task:?}"), original_input, "{name}");
        }
    }
}
