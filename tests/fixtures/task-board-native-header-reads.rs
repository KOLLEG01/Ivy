//! Native input preparation keeps immutable source pins and captured requests.
use super::*;
use ivy_contracts::checked_native_contract::CheckedNativeContracts;
use ivy_task_board::{
    lifecycle_journal::LifecycleJournal,
    native_driver::{NativeDriver, NativeDriverRuntime},
    native_intent::{NativeIntent, NativeSlot, TaskPrompts},
    prompts::CurrentTaskPrompts,
};
use std::{
    cell::{Cell, RefCell},
    path::Path,
};

struct Runtime {
    lose_reply: Cell<bool>,
    updates: RefCell<Vec<Json>>,
}
impl NativeDriverRuntime for Runtime {
    async fn verify_owner(&self) -> IvyResult<()> {
        Ok(())
    }
    async fn scoped_operation_id(&self, scope: &Json) -> IvyResult<JsString> {
        Ok(hash_json(scope)?.into())
    }
    async fn native_update(&self, request: &Json) -> IvyResult<Json> {
        self.updates.borrow_mut().push(request.clone());
        if self.lose_reply.replace(false) {
            Err(IvyError::new(
                "transport_lost",
                "The original attachment reply was lost.",
            ))
        } else {
            Ok(Json::Null)
        }
    }
    fn now(&self) -> JsString {
        TIME.into()
    }
}

fn refresh(row: &mut Json, contracts: &RuntimeContracts) {
    let value = row.required("content").unwrap().required("value").unwrap();
    let key = row
        .required("object")
        .unwrap()
        .required("contractKey")
        .unwrap()
        .as_str()
        .unwrap();
    let version = row
        .required("revision")
        .unwrap()
        .required("contractVersion")
        .unwrap()
        .as_js_string()
        .unwrap();
    contracts.validate_record(&key, version, value).unwrap();
    let hash = hash_json(value).unwrap();
    let bytes = canonical(value, JSON_OBJECT_CONTENT_BYTES).unwrap().len();
    row.get_mut("revision")
        .unwrap()
        .insert("contentHash", hash.into());
    row.get_mut("revision")
        .unwrap()
        .insert("byteLength", Json::Number(bytes as f64));
}
fn fixture(length: usize, recipient: Option<&str>) -> Json {
    let mut fixture = claimed_conversation_fixture(length);
    let contracts = runtime_inputs::contracts().unwrap();
    let Json::Array(nodes) = fixture.get_mut("nodes").unwrap() else {
        unreachable!()
    };
    for row in nodes {
        let value = row.get_mut("content").unwrap().get_mut("value").unwrap();
        if let Some(comment) = value
            .get_mut("entry")
            .and_then(|entry| entry.get_mut("comment"))
        {
            if comment.get("body").is_some_and(|body| body.is_str("")) {
                comment.insert("body", "Retained source comment".into());
            }
        }
        refresh(row, &contracts);
    }
    if let Some(recipient) = recipient {
        let Json::Array(nodes) = fixture.get_mut("nodes").unwrap() else {
            unreachable!()
        };
        let row = nodes
            .iter_mut()
            .find(|row| row.required("object").unwrap().get("id") == Some(&Json::from("node2")))
            .unwrap();
        let comment = row
            .get_mut("content")
            .unwrap()
            .get_mut("value")
            .unwrap()
            .get_mut("entry")
            .unwrap()
            .get_mut("comment")
            .unwrap();
        comment.insert("purpose", "question".into());
        let mut target = Json::object([("kind", recipient.into())]);
        if recipient == "task" {
            target.insert("taskId", "recipient-task".into());
        }
        comment.insert("recipient", target.clone());
        refresh(row, &contracts);
        let row = fixture.get_mut("task").unwrap();
        let task = row.get_mut("content").unwrap().get_mut("value").unwrap();
        task.insert(
            "openQuestionCount",
            Json::Number(if recipient == "task" { 0.0 } else { 1.0 }),
        );
        task.get_mut("commentStore")
            .unwrap()
            .get_mut("summary")
            .unwrap()
            .insert(
                "questions",
                Json::Array(vec![Json::object([
                    ("commentId", "c2".into()),
                    ("sequence", 2.0.into()),
                    ("open", true.into()),
                    ("recipient", target),
                ])]),
            );
        refresh(row, &contracts);
    }
    fixture
}

#[tokio::test(flavor = "current_thread")]
async fn native_source_headers_keep_prompt_bytes_and_original_calls_after_reopen() {
    let catalogs =
        CheckedNativeContracts::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."));
    for (length, recipient) in [(65, Some("user")), (300, Some("task")), (4096, None)] {
        let hive = Arc::new(Hive::new(&fixture(length, recipient)));
        let path =
            std::env::temp_dir().join(format!("native-headers-{}.sqlite", uuid::Uuid::new_v4()));
        let runtime = Runtime {
            lose_reply: Cell::new(true),
            updates: RefCell::default(),
        };
        let pin;
        let captured;
        {
            let db = Connection::open(&path).unwrap();
            initialize(&db).unwrap();
            let journal = snapshot_store(&db, hive.clone());
            let driver = NativeDriver {
                store: &journal,
                runtime: &runtime,
                prompts: &CurrentTaskPrompts,
                catalogs: &catalogs,
            };
            assert_eq!(
                driver
                    .prepare(&"run".into(), NativeSlot::Turn, None)
                    .await
                    .unwrap_err()
                    .code,
                "transport_lost"
            );
            assert_eq!(
                hive.state.lock().unwrap().comment_reads,
                0,
                "Native guards and initial input must not read comment nodes"
            );
            pin = runtime.updates.borrow()[0]
                .required("change")
                .unwrap()
                .required("call")
                .unwrap()
                .clone();
            captured = Store::read(&journal, "task-board/native-call", &pin, true)
                .await
                .unwrap();
            let run = Store::read(&journal, "task-board/run", &"run".into(), true)
                .await
                .unwrap();
            let header = journal
                .read_task_header(&"task".into(), true)
                .await
                .unwrap();
            let lifecycle = LifecycleJournal {
                journal: &journal,
                pages: &NoPages,
            };
            assert_eq!(
                hash_json(&header.value).unwrap(),
                hash_json(
                    &lifecycle
                        .task_header(&"task".into(), true, false)
                        .await
                        .unwrap()
                        .document
                        .value
                )
                .unwrap()
            );
            assert_eq!(hive.state.lock().unwrap().comment_reads, 0);
            let full = hydrated(hive.clone(), &header.value, &"task".into()).await;
            for changed in [false, true] {
                assert_eq!(
                    CurrentTaskPrompts
                        .task_update_prompt(&header.value, &"fixture.task-board".into(), changed)
                        .unwrap(),
                    CurrentTaskPrompts
                        .task_update_prompt(&full, &"fixture.task-board".into(), changed)
                        .unwrap()
                );
            }
            let mut waiting_header = header.value.clone();
            let mut waiting_full = full.clone();
            for task in [&mut waiting_header, &mut waiting_full] {
                task.insert("workflowState", "waiting".into());
                task.insert("claim", Json::Null);
            }
            let prompt = CurrentTaskPrompts
                .task_prompt(&waiting_header, &"fixture.task-board".into(), None)
                .unwrap();
            assert_eq!(
                prompt,
                CurrentTaskPrompts
                    .task_prompt(&waiting_full, &"fixture.task-board".into(), None)
                    .unwrap()
            );
            assert_eq!(
                prompt
                    .to_utf8()
                    .unwrap()
                    .contains("waiting without a wake-up trigger"),
                recipient.is_none()
            );
            let intent = NativeIntent {
                store: &journal,
                catalogs: &catalogs,
            };
            let expected = intent
                .intended_call(&run.value, NativeSlot::Turn, None, &CurrentTaskPrompts)
                .await
                .unwrap();
            assert_eq!(
                hash_json(
                    &intent
                        .resolve_native_request(captured.value.required("request").unwrap())
                        .unwrap()
                )
                .unwrap(),
                hash_json(expected.required("request").unwrap()).unwrap()
            );
        }
        let reads = hive.state.lock().unwrap().comment_reads;
        // Missing conversation nodes cannot justify rebuilding a committed NativeCall.
        hive.state.lock().unwrap().records.retain(|_, row| {
            row.required("object").unwrap().get("contractKey")
                != Some(&Json::from("task-board/comment-node"))
        });
        {
            let db = Connection::open(&path).unwrap();
            initialize(&db).unwrap();
            let journal = snapshot_store(&db, hive.clone());
            let driver = NativeDriver {
                store: &journal,
                runtime: &runtime,
                prompts: &CurrentTaskPrompts,
                catalogs: &catalogs,
            };
            assert_eq!(
                driver
                    .prepare(&"run".into(), NativeSlot::Turn, None)
                    .await
                    .unwrap(),
                pin
            );
            let saved = Store::read(&journal, "task-board/native-call", &pin, true)
                .await
                .unwrap();
            assert_eq!(
                hash_json(&saved.value).unwrap(),
                hash_json(&captured.value).unwrap()
            );
            assert_eq!(runtime.updates.borrow()[0], runtime.updates.borrow()[1]);
            assert_eq!(hive.state.lock().unwrap().comment_reads, reads);
        }
        std::fs::remove_file(path).unwrap();
        println!(
            "native header: {length} comments, recipient={recipient:?}; zero body reads, same prompt/request, lost attachment reply and reopen retain original call"
        );
    }
}

#[test]
fn compact_prompt_questions_match_the_existing_question_corpus() {
    let fixture = json(include_str!("task-board-questions.json"));
    let cases = fixture.required("cases").unwrap().as_array().unwrap();
    for case in cases {
        let expected = case
            .required("expected")
            .unwrap()
            .required("all")
            .unwrap()
            .as_array()
            .unwrap();
        let mut full = json(
            r#"{"taskKey":"TASK-0001","fields":{"control":"agent"},"workflowState":"waiting","claim":null,"publication":null,"runtimeWait":null,"completionIssue":null,"wakeAt":null,"dependencies":[],"comments":[],"commentDeliveries":[]}"#,
        );
        full.insert(
            "comments",
            case.required("task")
                .unwrap()
                .required("comments")
                .unwrap()
                .clone(),
        );
        let mut header = full.clone();
        header.insert("comments", Json::Array(vec![]));
        header.insert(
            "commentStore",
            Json::object([
                ("kind", "tree".into()),
                (
                    "summary",
                    Json::object([(
                        "questions",
                        Json::Array(
                            expected
                                .iter()
                                .map(|question| {
                                    Json::object([
                                        (
                                            "commentId",
                                            question.required("commentId").unwrap().clone(),
                                        ),
                                        ("open", true.into()),
                                    ])
                                })
                                .collect(),
                        ),
                    )]),
                ),
            ]),
        );
        let prompt = CurrentTaskPrompts
            .task_prompt(&header, &"owner".into(), None)
            .unwrap();
        assert_eq!(
            prompt,
            CurrentTaskPrompts
                .task_prompt(&full, &"owner".into(), None)
                .unwrap(),
            "{}",
            case.required("name").unwrap().as_str().unwrap()
        );
        assert_eq!(
            prompt
                .to_utf8()
                .unwrap()
                .contains("waiting without a wake-up trigger"),
            expected.is_empty()
        );
    }
    assert_eq!(cases.len(), 23);
}

#[tokio::test(flavor = "current_thread")]
async fn native_header_retains_legacy_materialization_and_archive_policy() {
    let mut fixture = conversation_fixture(65, 1);
    let contracts = runtime_inputs::contracts().unwrap();
    let Json::Array(nodes) = fixture.get_mut("nodes").unwrap() else {
        unreachable!()
    };
    for row in nodes {
        row.get_mut("revision")
            .unwrap()
            .insert("contractVersion", "1.0.0".into());
        refresh(row, &contracts);
    }
    let row = fixture.get_mut("task").unwrap();
    let task = row.get_mut("content").unwrap().get_mut("value").unwrap();
    task.remove("wakeAt");
    task.remove("dependencies");
    task.insert("nextSteps", Json::Array(vec![]));
    task.get_mut("commentStore").unwrap().remove("summary");
    row.get_mut("revision")
        .unwrap()
        .insert("contractVersion", "2.2.0".into());
    refresh(row, &contracts);
    let hive = Arc::new(Hive::new(&fixture));
    let db = Connection::open_in_memory().unwrap();
    initialize(&db).unwrap();
    let journal = snapshot_store(&db, hive.clone());
    let header = journal
        .read_task_header(&"task".into(), true)
        .await
        .unwrap();
    assert_eq!(
        header
            .value
            .required("comments")
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        65
    );
    assert!(hive.state.lock().unwrap().comment_reads > 0);
    hive.state
        .lock()
        .unwrap()
        .records
        .get_mut("task")
        .unwrap()
        .get_mut("object")
        .unwrap()
        .insert("effectivelyArchived", true.into());
    let reads = hive.state.lock().unwrap().comment_reads;
    assert_eq!(
        journal
            .read_task_header(&"task".into(), true)
            .await
            .unwrap_err()
            .code,
        "task_board_record_archived"
    );
    assert_eq!(hive.state.lock().unwrap().comment_reads, reads);
}
