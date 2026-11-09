//! Closed native inputs retain their Run identity without hydrating ticket history.
use super::consumer_reads::refresh;
use super::*;
use ivy_contracts::checked_native_contract::CheckedNativeContracts;
use ivy_task_board::{
    inspection_journal::InspectionJournal,
    native_comments::{NativeCommentJournal, sync_native_comments},
    native_inspector::InspectionStore,
};
use std::cell::RefCell;

fn input() -> Json {
    json(
        r#"{"identity":{"serviceNodeId":"native-agent","epoch":"previous-epoch","requestId":1},"method":"item/tool/requestUserInput","params":{"questions":[{"question":"Choose a value."}]},"observedAt":"2026-01-02T03:04:05.006Z","updatedAt":"2026-01-02T03:04:05.006Z","threadId":"original-thread","turnId":"original-turn","state":"answered","answerOperationId":"answer","answerCallerPrincipalId":"user","reply":{"result":{"answers":{"question":{"answers":["First","Second"]}}}},"code":null}"#,
    )
}
fn fixture(length: usize, repeated: bool) -> Json {
    let mut fixture = claimed_conversation_fixture(length);
    let input = input();
    let mut pending = input.clone();
    for (key, value) in [
        ("state", "pending".into()),
        ("answerOperationId", Json::Null),
        ("answerCallerPrincipalId", Json::Null),
        ("reply", Json::Null),
    ] {
        pending.insert(key, value);
    }
    let id = format!(
        "native-question-{}",
        &hash_json(input.required("identity").unwrap()).unwrap()[7..]
    );
    let contracts = runtime_inputs::contracts().unwrap();
    let Json::Array(nodes) = fixture.get_mut("nodes").unwrap() else {
        unreachable!()
    };
    for row in nodes {
        let value = row.get_mut("content").unwrap().get_mut("value").unwrap();
        if let Some(comment) = value.get_mut("entry").and_then(|e| e.get_mut("comment")) {
            if comment.get("body").is_some_and(|body| body.is_str("")) {
                comment.insert("body", "Retained source comment".into());
            }
            let sequence = comment.required("sequence").unwrap().as_f64().unwrap() as usize;
            if sequence == 12 || sequence == 20 || repeated && sequence == 14 {
                comment.insert(
                    "commentId",
                    if sequence == 20 {
                        "foreign-question".into()
                    } else {
                        id.clone().into()
                    },
                );
                comment.insert("purpose", "question".into());
                comment.insert("authorKind", "agent".into());
                comment
                    .get_mut("author")
                    .unwrap()
                    .insert("source", "native".into());
                comment.insert("nativeInput", pending.clone());
                comment.insert(
                    "run",
                    json(if sequence == 20 {
                        r#"{"objectId":"foreign-run","revision":1}"#
                    } else {
                        r#"{"objectId":"run","revision":1}"#
                    }),
                );
            }
            if sequence == length - 1 || sequence == length {
                comment.insert(
                    "replyTo",
                    if sequence == length {
                        id.clone().into()
                    } else {
                        "foreign-question".into()
                    },
                );
                comment.insert("nativeInput", input.clone());
                comment
                    .get_mut("author")
                    .unwrap()
                    .insert("source", "native".into());
            }
        }
        refresh(row, &contracts);
    }
    fixture
}

#[tokio::test(flavor = "current_thread")]
async fn native_question_index_keeps_closed_records_run_scope_and_duplicates_after_reopen() {
    for length in [65, 300, 4096] {
        let hive = Arc::new(Hive::new(&fixture(length, true)));
        let path =
            std::env::temp_dir().join(format!("native-questions-{}.sqlite", uuid::Uuid::new_v4()));
        let original;
        {
            let db = Connection::open(&path).unwrap();
            initialize(&db).unwrap();
            let journal = snapshot_store(&db, hive.clone());
            let inspection =
                InspectionJournal::new(&journal, "caller".into(), "fixture.task-board".into());
            let header = inspection.task_header(&"task".into()).await.unwrap();
            assert_eq!(hive.state.lock().unwrap().comment_reads, 0);
            assert!(
                header
                    .value
                    .required("commentStore")
                    .unwrap()
                    .required("summary")
                    .unwrap()
                    .required("questions")
                    .unwrap()
                    .as_array()
                    .unwrap()
                    .is_empty()
            );
            original = inspection
                .task_native_questions(&header.value, &"task".into(), &"run".into(), None)
                .await
                .unwrap();
            assert_eq!(original.len(), 2);
            assert_eq!(original[0].get("commentId"), original[1].get("commentId"));
            assert_eq!(
                original
                    .iter()
                    .map(|c| c.required("sequence").unwrap().as_f64().unwrap() as usize)
                    .collect::<Vec<_>>(),
                vec![12, 14]
            );
            let full = journal
                .read("task-board/task", &"task".into(), true)
                .await
                .unwrap();
            assert!(
                ivy_sdk::open_task_questions(&full.value, None)
                    .unwrap()
                    .is_empty(),
                "The native questions are actually closed"
            );
        }
        {
            let db = Connection::open(&path).unwrap();
            initialize(&db).unwrap();
            let journal = snapshot_store(&db, hive.clone());
            let inspection =
                InspectionJournal::new(&journal, "caller".into(), "fixture.task-board".into());
            let header = inspection.task_header(&"task".into()).await.unwrap();
            let before = hive.state.lock().unwrap().comment_reads;
            assert_eq!(
                inspection
                    .task_native_questions(&header.value, &"task".into(), &"run".into(), None)
                    .await
                    .unwrap(),
                original
            );
            assert!(hive.state.lock().unwrap().comment_reads - before <= 6);
            let before = hive.state.lock().unwrap().comment_reads;
            assert!(
                inspection
                    .task_native_questions(
                        &header.value,
                        &"task".into(),
                        &"absent-run".into(),
                        None
                    )
                    .await
                    .unwrap()
                    .is_empty()
            );
            assert_eq!(hive.state.lock().unwrap().comment_reads, before);
            assert_eq!(
                inspection
                    .task_native_questions(
                        &header.value,
                        &"task".into(),
                        &"foreign-run".into(),
                        None
                    )
                    .await
                    .unwrap()
                    .len(),
                1
            );
            assert_eq!(
                db.query_row(
                    "SELECT COUNT(*) FROM comment_index_native_questions",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
                3
            );
            db.execute("DELETE FROM comment_index_roots", []).unwrap();
            assert_eq!(
                db.query_row(
                    "SELECT COUNT(*) FROM comment_index_native_questions",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
                0
            );
        }
        assert_eq!(hive.state.lock().unwrap().writes, 0);
    }
}

struct InputHive {
    hive: Arc<Hive>,
    page: Mutex<Json>,
}
impl RpcClient for InputHive {
    fn request<'a>(
        &'a self,
        method: &'a str,
        params: Json,
        options: RequestOptions,
    ) -> RequestFuture<'a> {
        Box::pin(async move {
            if method == "tools.list" {
                let name = params.required("namePrefix")?.clone();
                assert!(name.is_str("inputs"));
                return Ok(Json::object([
                    (
                        "provider",
                        Json::object([(
                            "node",
                            Json::object([("serviceNodeId", "native-agent".into())]),
                        )]),
                    ),
                    (
                        "items",
                        Json::Array(vec![Json::object([
                            ("qualifiedName", "agent.inputs".into()),
                            (
                                "definition",
                                Json::object([
                                    ("namespace", "agent".into()),
                                    ("name", name),
                                    ("interfaceVersion", "1.0.0".into()),
                                    ("annotations", Json::object([("readOnlyHint", true.into())])),
                                    ("inputSchema", Json::Object(vec![])),
                                    ("outputSchema", Json::Object(vec![])),
                                ]),
                            ),
                            (
                                "definitionHash",
                                format!("sha256:{}", "a".repeat(64)).into(),
                            ),
                            ("catalogHash", format!("sha256:{}", "b".repeat(64)).into()),
                        ])]),
                    ),
                    ("nextCursor", Json::Null),
                ]));
            }
            if method == "tools.call" {
                assert_eq!(
                    params.required("qualifiedName")?,
                    &Json::from("agent.inputs")
                );
                assert_eq!(
                    params.required("serviceNodeId")?,
                    &Json::from("native-agent")
                );
                return Ok(self.page.lock().unwrap().clone());
            }
            self.hive.request(method, params, options).await
        })
    }
}
#[tokio::test(flavor = "current_thread")]
async fn mirror_reads_closed_historical_question_and_keeps_exact_response_request_after_reopen() {
    for (length, repeated) in [(65, false), (300, true), (4096, true)] {
        let hive = Arc::new(Hive::new(&fixture(length, repeated)));
        let client = Arc::new(InputHive {
            hive: hive.clone(),
            page: Mutex::new(Json::object([
                ("epoch", "epoch".into()),
                ("items", Json::Array(vec![input()])),
                ("truncated", false.into()),
            ])),
        });
        let path =
            std::env::temp_dir().join(format!("native-mirror-{}.sqlite", uuid::Uuid::new_v4()));
        let captured = RefCell::new(Vec::new());
        for pass in 0..2 {
            let db = Connection::open(&path).unwrap();
            initialize(&db).unwrap();
            let journal = JournalStore {
                journal: LocalJournal {
                    connection: &db,
                    root_object_id: None,
                },
                client: client.clone(),
                contracts: runtime_inputs::contracts().unwrap(),
                limits: HistoryLimits::default(),
                now: edit_clock as fn() -> String,
                new_id: (|| uuid::Uuid::new_v4().to_string().into()) as fn() -> JsString,
                cache: None,
            };
            let inspection =
                InspectionJournal::new(&journal, "caller".into(), "fixture.task-board".into());
            let header = inspection.task_header(&"task".into()).await.unwrap();
            inspection
                .task_native_questions(&header.value, &"task".into(), &"run".into(), None)
                .await
                .unwrap();
            let before = hive.state.lock().unwrap().comment_reads;
            let comments = NativeCommentJournal {
                inspection: &inspection,
                native_comment: |request| {
                    captured.borrow_mut().push(request);
                    std::future::ready(Err::<Json, _>(IvyError::new(
                        "fixture_stop",
                        "Capture before publication; no native answer is dispatched.",
                    )))
                },
            };
            let catalogs = CheckedNativeContracts::new(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."),
            );
            let saved_page = client.page.lock().unwrap().clone();
            client
                .page
                .lock()
                .unwrap()
                .insert("items", Json::Array(vec![]));
            sync_native_comments(&comments, &catalogs, &"run".into())
                .await
                .unwrap();
            assert_eq!(
                hive.state.lock().unwrap().comment_reads,
                before,
                "An empty input page reads no old question bodies"
            );
            *client.page.lock().unwrap() = saved_page;
            assert_eq!(
                sync_native_comments(&comments, &catalogs, &"run".into())
                    .await
                    .unwrap_err()
                    .code,
                "fixture_stop"
            );
            assert!(
                hive.state.lock().unwrap().comment_reads - before <= if repeated { 18 } else { 9 }
            );
            assert_eq!(captured.borrow().len(), pass + 1);
            assert_eq!(
                header
                    .value
                    .required("comments")
                    .unwrap()
                    .as_array()
                    .unwrap()
                    .len(),
                0
            );
        }
        let requests = captured.into_inner();
        assert_eq!(requests[0], requests[1]);
        let hash = hash_json(input().required("identity").unwrap()).unwrap();
        assert_eq!(
            requests[0].required("commentId").unwrap(),
            &Json::from(format!("native-response-{}", &hash[7..]))
        );
        assert_eq!(
            requests[0].required("operationId").unwrap(),
            &Json::from(format!("native-response-{}-1", &hash[7..]))
        );
        assert_eq!(
            requests[0].required("body").unwrap(),
            &Json::from("First\n\nSecond")
        );
        assert_eq!(requests[0].required("nativeInput").unwrap(), &input());
        assert_eq!(hive.state.lock().unwrap().writes, 0);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn incomplete_native_question_index_cannot_prove_a_missing_run() {
    let mut fixture = fixture(300, false);
    let Json::Array(nodes) = fixture.get_mut("nodes").unwrap() else {
        unreachable!()
    };
    let index = nodes
        .iter()
        .position(|row| row.required("object").unwrap().get("id") == Some(&Json::from("node300")))
        .unwrap();
    nodes.remove(index);
    let hive = Arc::new(Hive::new(&fixture));
    let db = Connection::open_in_memory().unwrap();
    initialize(&db).unwrap();
    let journal = snapshot_store(&db, hive.clone());
    let inspection = InspectionJournal::new(&journal, "caller".into(), "fixture.task-board".into());
    let header = inspection.task_header(&"task".into()).await.unwrap();
    for run in ["run", "absent-run"] {
        assert!(
            inspection
                .task_native_questions(&header.value, &"task".into(), &run.into(), None)
                .await
                .is_err()
        );
        assert_eq!(
            db.query_row("SELECT complete FROM comment_index_roots", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
    assert_eq!(hive.state.lock().unwrap().writes, 0);
}
