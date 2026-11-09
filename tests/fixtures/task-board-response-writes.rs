//! Structured response deltas retain every historical question's own coverage.
use super::*;
use ivy_task_board::lifecycle_journal::LifecycleJournal;

fn requests(prefix: &str) -> Json {
    Json::Array(
        ["a", "b"]
            .iter()
            .map(|id| {
                Json::object([
                    ("requestId", format!("{prefix}{id}").into()),
                    ("type", "approval".into()),
                    ("title", "Original decision".into()),
                    ("detail", "".into()),
                ])
            })
            .collect(),
    )
}
fn response(id: &str, revision: f64, key: &str) -> Json {
    let mut request = append_request(id, revision);
    request.insert(
        "responses",
        Json::Array(vec![Json::object([
            ("requestId", key.into()),
            ("type", "approval".into()),
            ("decision", "approved".into()),
            ("detail", "".into()),
        ])]),
    );
    request
}
fn fixture(length: usize, repeated_ids: bool, native: bool, oversized: bool) -> Json {
    let mut fixture = conversation_fixture(length, 1);
    let contracts = runtime_inputs::contracts().unwrap();
    let Json::Array(nodes) = fixture.get_mut("nodes").unwrap() else {
        unreachable!()
    };
    let mut ordered = Vec::new();
    for row in nodes {
        let value = row.get_mut("content").unwrap().get_mut("value").unwrap();
        if let Some(entry) = value.get_mut("entry") {
            let c = entry.get_mut("comment").unwrap();
            let at = c.required("sequence").unwrap().as_f64().unwrap() as usize;
            if [3, 7, 12, 18, 23, 50].contains(&at) {
                c.insert(
                    "requests",
                    requests(if [7, 50].contains(&at) { "other-" } else { "" }),
                );
                if at != 23 {
                    c.insert("purpose", "question".into());
                    c.insert("authorKind", "agent".into());
                    c.get_mut("author")
                        .unwrap()
                        .insert("source", "worker".into());
                    c.insert("recipient", json(r#"{"kind":"user"}"#));
                    if repeated_ids && [3, 12].contains(&at) {
                        c.insert("commentId", "q".into());
                    }
                    if native && at == 18 {
                        c.insert("nativeInput", json(r#"{"identity":{"serviceNodeId":"native-agent","epoch":"epoch","requestId":"native"},"method":"item/tool/requestUserInput","params":{},"observedAt":"2026-01-02T03:04:05.006Z","updatedAt":"2026-01-02T03:04:05.006Z","threadId":"original-thread","turnId":"original-turn","state":"pending","answerOperationId":null,"answerCallerPrincipalId":null,"reply":null,"code":null}"#));
                        c.get_mut("author")
                            .unwrap()
                            .insert("source", "native".into());
                    }
                    let receipt = Json::object([
                        ("commentId", c.required("commentId").unwrap().clone()),
                        ("operationId", format!("original-{at}").into()),
                        ("state", "queued".into()),
                        ("updatedAt", TIME.into()),
                        ("code", Json::Null),
                    ]);
                    entry.insert("receipt", receipt);
                }
            } else if oversized {
                let mut reqs = Vec::new();
                let mut replies = Vec::new();
                for number in 0..32 {
                    let prefix = format!("noise-{at}-{number}-");
                    let id = prefix.clone() + &"x".repeat(256 - prefix.len());
                    reqs.push(Json::object([
                        ("requestId", id.clone().into()),
                        ("type", "approval".into()),
                        ("title", "Retained".into()),
                        ("detail", "".into()),
                    ]));
                    replies.push(Json::object([
                        ("requestId", id.into()),
                        ("type", "approval".into()),
                        ("decision", "approved".into()),
                        ("detail", "".into()),
                    ]));
                }
                c.insert("requests", Json::Array(reqs));
                c.insert("responses", Json::Array(replies));
            }
            ordered.push(entry.required("comment").unwrap().clone());
        }
        consumer_reads::refresh(row, &contracts);
    }
    let open =
        ivy_sdk::open_task_questions(&Json::object([("comments", Json::Array(ordered))]), None)
            .unwrap();
    assert_eq!(open.len(), 5);
    let row = fixture.get_mut("task").unwrap();
    let task = row.get_mut("content").unwrap().get_mut("value").unwrap();
    task.insert("openQuestionCount", 5.0.into());
    task.get_mut("commentStore")
        .unwrap()
        .get_mut("summary")
        .unwrap()
        .insert(
            "questions",
            Json::Array(
                open.iter()
                    .map(|q| {
                        Json::object([
                            ("commentId", q.required("commentId").unwrap().clone()),
                            ("sequence", q.required("sequence").unwrap().clone()),
                            ("recipient", q.required("recipient").unwrap().clone()),
                            ("open", true.into()),
                        ])
                    })
                    .collect(),
            ),
        );
    consumer_reads::refresh(row, &contracts);
    fixture
}
fn saved(hive: &Hive) -> Json {
    hive.state.lock().unwrap().records["task"]
        .required("content")
        .unwrap()
        .required("value")
        .unwrap()
        .clone()
}
fn revision(hive: &Hive) -> f64 {
    hive.state.lock().unwrap().records["task"]
        .required("revision")
        .unwrap()
        .required("revision")
        .unwrap()
        .as_f64()
        .unwrap()
}
async fn check(hive: Arc<Hive>, before: &Json, expected: usize) {
    let header = saved(&hive);
    let full = hydrated(hive, &header, &"task".into()).await;
    let original = before.required("comments").unwrap().as_array().unwrap();
    assert_eq!(
        &full.required("comments").unwrap().as_array().unwrap()[..original.len()],
        original
    );
    let receipts = before
        .required("commentDeliveries")
        .unwrap()
        .as_array()
        .unwrap();
    assert_eq!(
        &full
            .required("commentDeliveries")
            .unwrap()
            .as_array()
            .unwrap()[..receipts.len()],
        receipts
    );
    let open = ivy_sdk::open_task_questions(&full, None).unwrap();
    assert_eq!(open.len(), expected);
    let summary = header
        .required("commentStore")
        .unwrap()
        .required("summary")
        .unwrap();
    let projected = summary.required("questions").unwrap().as_array().unwrap();
    assert_eq!(projected.len(), expected);
    for (actual, original) in projected.iter().zip(&open) {
        for key in ["commentId", "sequence", "recipient"] {
            assert_eq!(actual.get(key), original.get(key));
        }
        assert_eq!(actual.get("open"), Some(&true.into()));
    }
    assert_eq!(
        header.get("openQuestionCount"),
        Some(&Json::Number(expected as f64))
    );
    assert_eq!(
        summary.get("directCount"),
        Some(&Json::Number(
            full.required("comments").unwrap().as_array().unwrap().len() as f64
        ))
    );
}

#[tokio::test(flavor = "current_thread")]
async fn reused_request_ids_keep_all_question_sequences_after_loss_and_reopen() {
    for length in [65, 300, 4096] {
        for (repeated, native) in [(false, false), (true, false), (false, true)] {
            let fixture = fixture(length, repeated, native, false);
            let hive = Arc::new(Hive::new(&fixture));
            let header = saved(&hive);
            let before = hydrated(hive.clone(), &header, &"task".into()).await;
            let path =
                std::env::temp_dir().join(format!("coverage-{}.sqlite", uuid::Uuid::new_v4()));
            let first = response("answer-a", 1.0, "a");
            let removed;
            let second;
            {
                let db = Connection::open(&path).unwrap();
                initialize(&db).unwrap();
                let journal = snapshot_store(&db, hive.clone());
                let store = LifecycleJournal {
                    journal: &journal,
                    pages: &NoPages,
                };
                store
                    .queued_comment_ids(&header, &"task".into())
                    .await
                    .unwrap();
                let mut state = hive.state.lock().unwrap();
                let tail = format!("node{length}");
                let ids = state
                    .records
                    .iter()
                    .filter(|(id, row)| {
                        row.required("content")
                            .unwrap()
                            .required("value")
                            .unwrap()
                            .get("kind")
                            == Some(&"comment".into())
                            && id.as_str() != tail
                            && !["node3", "node7", "node12", "node18", "node23"]
                                .contains(&id.as_str())
                    })
                    .map(|(id, _)| id.clone())
                    .collect::<Vec<_>>();
                removed = ids
                    .iter()
                    .map(|id| (id.clone(), state.records.remove(id).unwrap()))
                    .collect::<Vec<_>>();
                state.comment_reads = 0;
                drop(state);
                let error = publish_incremental_comment(&journal, &first, true)
                    .await
                    .unwrap_err();
                assert_eq!(
                    error.code, "transport_lost",
                    "{length}/{repeated}/{native}: {error:?}"
                );
                assert!(hive.state.lock().unwrap().comment_reads <= 64);
            }
            let writes = hive.state.lock().unwrap().writes;
            {
                let db = Connection::open(&path).unwrap();
                initialize(&db).unwrap();
                let journal = snapshot_store(&db, hive.clone());
                let outcome = publish_incremental_comment(&journal, &first, false)
                    .await
                    .unwrap();
                assert_eq!(
                    outcome,
                    publish_incremental_comment(&journal, &first, false)
                        .await
                        .unwrap()
                );
                assert_eq!(hive.state.lock().unwrap().writes, writes);
                hive.state.lock().unwrap().comment_reads = 0;
                second = response("answer-b", revision(&hive), "b");
                assert_eq!(
                    publish_incremental_comment(&journal, &second, true)
                        .await
                        .unwrap_err()
                        .code,
                    "transport_lost"
                );
                assert!(hive.state.lock().unwrap().comment_reads <= 64);
            }
            let writes = hive.state.lock().unwrap().writes;
            {
                let db = Connection::open(&path).unwrap();
                initialize(&db).unwrap();
                let journal = snapshot_store(&db, hive.clone());
                let outcome = publish_incremental_comment(&journal, &second, false)
                    .await
                    .unwrap();
                assert_eq!(
                    outcome,
                    publish_incremental_comment(&journal, &second, false)
                        .await
                        .unwrap()
                );
                assert_eq!(hive.state.lock().unwrap().writes, writes);
                let invalid = response("another-a", revision(&hive), "a");
                assert_eq!(
                    publish_incremental_comment(&journal, &invalid, false)
                        .await
                        .unwrap_err()
                        .code,
                    "task_board_request_already_answered"
                );
                assert_eq!(hive.state.lock().unwrap().writes, writes);
            }
            for (id, row) in removed {
                hive.state.lock().unwrap().records.insert(id, row);
            }
            check(hive.clone(), &before, if native { 3 } else { 2 }).await;
            std::fs::remove_file(path).unwrap();
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn oversized_response_projection_retains_first_occurrences_and_original_guards() {
    let fixture = fixture(512, false, false, true);
    let hive = Arc::new(Hive::new(&fixture));
    let before = hydrated(hive.clone(), &saved(&hive), &"task".into()).await;
    let db = Connection::open_in_memory().unwrap();
    initialize(&db).unwrap();
    let journal = snapshot_store(&db, hive.clone());
    for (id, key) in [("answer-a", "a"), ("answer-b", "b")] {
        publish_incremental_comment(&journal, &response(id, revision(&hive), key), false)
            .await
            .unwrap();
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM comment_index_roots", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM comment_index_ids", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
    check(hive, &before, 2).await;
}

#[tokio::test(flavor = "current_thread")]
async fn missing_response_history_is_an_error_before_publication() {
    let hive = Arc::new(Hive::new(&fixture(65, false, false, false)));
    hive.state.lock().unwrap().records.remove("node7");
    let db = Connection::open_in_memory().unwrap();
    initialize(&db).unwrap();
    let journal = snapshot_store(&db, hive.clone());
    assert_eq!(
        publish_incremental_comment(&journal, &response("answer-a", 1.0, "a"), false)
            .await
            .unwrap_err()
            .code,
        "not_found"
    );
    assert_eq!(hive.state.lock().unwrap().task_writes, 0);
}
