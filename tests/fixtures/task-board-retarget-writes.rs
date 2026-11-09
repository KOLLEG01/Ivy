//! Recipient corrections retain original entries and re-evaluate actual replies.
use super::*;

fn recipient(task: bool) -> Json {
    if task {
        json(r#"{"kind":"task","taskId":"peer"}"#)
    } else {
        json(r#"{"kind":"user"}"#)
    }
}
fn request(task: bool) -> Json {
    Json::object([
        ("action", "retargetQuestion".into()),
        ("operationId", "correct-recipient".into()),
        ("taskId", "task".into()),
        ("expectedRevision", 1.0.into()),
        ("questionId", "q".into()),
        ("recipient", recipient(task)),
    ])
}
fn native_input(answered: bool) -> Json {
    let mut input = json(
        r#"{"identity":{"serviceNodeId":"native-agent","epoch":"original-epoch","requestId":1},"method":"item/tool/requestUserInput","params":{"questions":[{"question":"Choose a value."}]},"observedAt":"2026-01-02T03:04:05.006Z","updatedAt":"2026-01-02T03:04:05.006Z","threadId":"original-thread","turnId":"original-turn","state":"pending","answerOperationId":null,"answerCallerPrincipalId":null,"reply":null,"code":null}"#,
    );
    if answered {
        input.insert("state", "answered".into());
        input.insert("answerOperationId", "original-answer".into());
        input.insert("answerCallerPrincipalId", "user".into());
        input.insert("reply", json(r#"{"result":{}}"#));
    }
    input
}
fn fixture(length: usize, case: &str) -> Json {
    let mut fixture = conversation_fixture(length, if length == 300 { 600 } else { 0 });
    let contracts = runtime_inputs::contracts().unwrap();
    let Json::Array(nodes) = fixture.get_mut("nodes").unwrap() else {
        unreachable!()
    };
    let duplicate = case.starts_with("duplicate-");
    for row in nodes.iter_mut() {
        let value = row.get_mut("content").unwrap().get_mut("value").unwrap();
        if let Some(entry) = value.get_mut("entry") {
            let comment = entry.get_mut("comment").unwrap();
            if comment.get("body").is_some_and(|v| v.is_str("")) {
                comment.insert("body", "Retained history".into());
            }
            let at = comment.required("sequence").unwrap().as_f64().unwrap() as usize;
            if at == 12 || duplicate && at == 3 {
                comment.insert("commentId", "q".into());
                comment.insert("purpose", "question".into());
                comment.insert("authorKind", "agent".into());
                comment
                    .get_mut("author")
                    .unwrap()
                    .insert("source", "worker".into());
                comment.insert(
                    "recipient",
                    recipient(at == 12 && ["user-answered", "before-question"].contains(&case)),
                );
                if case == "duplicate-covered" && at == 3 {
                    comment.insert("requests", json(r#"[{"requestId":"a","type":"approval","title":"A","detail":""},{"requestId":"b","type":"approval","title":"B","detail":""}]"#));
                }
                if case == "duplicate-native" && at == 3 {
                    comment.insert("nativeInput", native_input(false));
                    comment
                        .get_mut("author")
                        .unwrap()
                        .insert("source", "native".into());
                }
                entry.insert("receipt", json(r#"{"commentId":"q","operationId":"original-input","state":"queued","updatedAt":"2026-01-02T03:04:05.006Z","code":null}"#));
            } else if at == 13 && ["user-answered", "task-answered", "wrong-task"].contains(&case)
                || at == 1 && case == "before-question"
                || at == 4 && duplicate
            {
                comment.insert("replyTo", "q".into());
                if ["task-answered", "wrong-task"].contains(&case) {
                    comment.insert("authorKind", "agent".into());
                    comment.insert(
                        "sourceTaskId",
                        if case == "wrong-task" {
                            "other"
                        } else {
                            "peer"
                        }
                        .into(),
                    );
                }
                if case == "duplicate-resolved" {
                    comment.insert("resolvesQuestions", json(r#"["q"]"#));
                }
                if case == "duplicate-native" {
                    comment.insert("nativeInput", native_input(true));
                    comment
                        .get_mut("author")
                        .unwrap()
                        .insert("source", "native".into());
                }
                if case == "duplicate-covered" {
                    comment.insert("authorKind", "agent".into());
                    comment.insert("responses", json(r#"[{"requestId":"a","type":"approval","decision":"approved","detail":""},{"requestId":"b","type":"approval","decision":"approved","detail":""}]"#));
                }
            }
        }
        consumer_reads::refresh(row, &contracts);
    }
    let mut peer = conversation_fixture(0, 0).required("task").unwrap().clone();
    peer.get_mut("object").unwrap().insert("id", "peer".into());
    nodes.push(peer);
    let ordered = nodes
        .iter()
        .filter_map(|row| {
            row.get("content")?
                .get("value")?
                .get("entry")?
                .get("comment")
                .cloned()
        })
        .collect::<Vec<_>>();
    let full = Json::object([("comments", Json::Array(ordered.clone()))]);
    let open = ivy_sdk::open_task_questions(&full, None).unwrap();
    assert_eq!(open.len(), 1, "{case} starts with one unresolved question");
    let task = fixture.get_mut("task").unwrap();
    let value = task.get_mut("content").unwrap().get_mut("value").unwrap();
    let tree = value.get_mut("commentStore").unwrap();
    let direct = ordered
        .iter()
        .filter(|c| {
            !ivy_task_board::engine_read::conversation::cross_agent_comment(c, &"task".into())
        })
        .count();
    tree.insert(
        "summary",
        Json::object([
            ("directCount", Json::Number(direct as f64)),
            (
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
            ),
        ]),
    );
    value.insert(
        "openQuestionCount",
        Json::Number(
            open.iter()
                .filter(|q| q.required("recipient").unwrap().get("kind") != Some(&"task".into()))
                .count() as f64,
        ),
    );
    consumer_reads::refresh(task, &contracts);
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
async fn assert_projection(hive: Arc<Hive>, before: &Json, task_recipient: bool) {
    let header = saved(&hive);
    let full = hydrated(hive.clone(), &header, &"task".into()).await;
    let mut expected = before.required("comments").unwrap().clone();
    let Json::Array(comments) = &mut expected else {
        unreachable!()
    };
    for comment in comments {
        if comment.get("commentId") == Some(&"q".into()) {
            comment.insert("recipient", recipient(task_recipient));
        }
    }
    assert_eq!(
        hash_json(&expected).unwrap(),
        hash_json(full.required("comments").unwrap()).unwrap()
    );
    assert_eq!(
        before.required("commentDeliveries").unwrap(),
        full.required("commentDeliveries").unwrap()
    );
    let questions = ivy_sdk::open_task_questions(&full, None).unwrap();
    let summary = header
        .required("commentStore")
        .unwrap()
        .required("summary")
        .unwrap();
    let projected = summary.required("questions").unwrap().as_array().unwrap();
    assert_eq!(projected.len(), questions.len());
    for (actual, expected) in projected.iter().zip(&questions) {
        for field in ["commentId", "sequence", "recipient"] {
            assert_eq!(actual.get(field), expected.get(field));
        }
        assert_eq!(actual.get("open"), Some(&true.into()));
    }
    assert_eq!(
        header.get("openQuestionCount"),
        Some(&Json::Number(
            questions
                .iter()
                .filter(|q| q.required("recipient").unwrap().get("kind") != Some(&"task".into()))
                .count() as f64
        ))
    );
    let direct = full
        .required("comments")
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| {
            !ivy_task_board::engine_read::conversation::cross_agent_comment(c, &"task".into())
        })
        .count();
    assert_eq!(
        summary.get("directCount"),
        Some(&Json::Number(direct as f64))
    );
    assert_eq!(
        header.required("commentStore").unwrap().get("length"),
        Some(&Json::Number(expected.as_array().unwrap().len() as f64))
    );
}

#[tokio::test(flavor = "current_thread")]
async fn full_read_cache_and_prepared_writes_keep_only_actual_open_duplicate_questions() {
    use ivy_task_board::engine_read::conversation::ConversationPolicies;
    for case in [
        "duplicate-reopen",
        "duplicate-resolved",
        "duplicate-covered",
        "duplicate-native",
    ] {
        let fixture = fixture(65, case);
        let hive = Arc::new(Hive::new(&fixture));
        let mut header = saved(&hive);
        // Exercise the lower-level reader's optional summary without changing
        // the valid current-version Task record used by actual publication.
        header.get_mut("commentStore").unwrap().remove("summary");
        let tree = header.required("commentStore").unwrap();
        let reader = TaskComments::new(
            Arc::new(RpcCommentSource::new(hive.clone())),
            tokio::runtime::Handle::current(),
        );
        let full = reader
            .hydrate(&header, &"task".into())
            .await
            .unwrap()
            .unwrap();
        let expected = ivy_sdk::open_task_questions(&full, None).unwrap();
        assert_eq!(expected.len(), 1);
        assert_eq!(expected[0].get("sequence"), Some(&12.0.into()));
        let check = |summary: &Json| {
            let questions = summary.required("questions").unwrap().as_array().unwrap();
            assert_eq!(questions.len(), expected.len(), "{case}");
            for (q, original) in questions.iter().zip(&expected) {
                for field in ["commentId", "sequence", "recipient"] {
                    assert_eq!(q.get(field), original.get(field), "{case}");
                }
            }
        };
        check(
            &reader
                .summary(&header, tree, &"task".into())
                .unwrap()
                .unwrap(),
        );
        let condition = |_: &Json, _: Option<&Json>| -> IvyResult<Json> { unreachable!() };
        let policies = ConversationPolicies::with_current_questions(&condition);
        let other = TaskComments::new(
            Arc::new(RpcCommentSource::new(hive.clone())),
            tokio::runtime::Handle::current(),
        );
        assert_eq!(
            other
                .read_tree(&header, &"task".into(), &policies)
                .await
                .unwrap()
                .unwrap(),
            full
        );
        check(
            &other
                .summary(&header, tree, &"task".into())
                .unwrap()
                .unwrap(),
        );
        let mut proposed = full.clone();
        let Json::Array(comments) = proposed.get_mut("comments").unwrap() else {
            unreachable!()
        };
        comments.push(comment(66, "An unrelated new comment".into()));
        let db = Connection::open_in_memory().unwrap();
        initialize(&db).unwrap();
        let store = snapshot_store(&db, hive.clone());
        let destination = json(r#"{"objectId":"task","expectedRevision":1}"#);
        hive.state.lock().unwrap().lost_task_reply = true;
        assert_eq!(
            store
                .write_prepared(
                    "task-board/task",
                    &proposed,
                    &"duplicate-summary".into(),
                    &destination,
                    None,
                    None
                )
                .await
                .unwrap_err()
                .code,
            "transport_lost"
        );
        let pin = store
            .write_prepared(
                "task-board/task",
                &proposed,
                &"duplicate-summary".into(),
                &destination,
                None,
                None,
            )
            .await
            .unwrap();
        let writes = hive.state.lock().unwrap().writes;
        assert_eq!(
            store
                .write_prepared(
                    "task-board/task",
                    &proposed,
                    &"duplicate-summary".into(),
                    &destination,
                    None,
                    None
                )
                .await
                .unwrap(),
            pin
        );
        assert_eq!(hive.state.lock().unwrap().writes, writes);
        let saved = saved(&hive);
        check(
            saved
                .required("commentStore")
                .unwrap()
                .required("summary")
                .unwrap(),
        );
        let actual = hydrated(hive.clone(), &saved, &"task".into()).await;
        assert_eq!(actual.get("comments"), proposed.get("comments"));
        assert_eq!(
            actual.get("commentDeliveries"),
            full.get("commentDeliveries")
        );
        reader.settled().await;
        other.settled().await;
    }
}

#[tokio::test(flavor = "current_thread")]
async fn recipient_correction_preserves_receipts_and_replays_after_lost_reply_and_reopen() {
    for length in [65, 300, 4096] {
        for case in [
            "user-answered",
            "task-answered",
            "wrong-task",
            "before-question",
        ] {
            let fixture = fixture(length, case);
            let hive = Arc::new(Hive::new(&fixture));
            let original = fixture
                .required("task")
                .unwrap()
                .required("content")
                .unwrap()
                .required("value")
                .unwrap();
            let before = hydrated(hive.clone(), original, &"task".into()).await;
            let path =
                std::env::temp_dir().join(format!("retarget-{}.sqlite", uuid::Uuid::new_v4()));
            let req = request(!["user-answered", "before-question"].contains(&case));
            let removed;
            {
                let db = Connection::open(&path).unwrap();
                initialize(&db).unwrap();
                let journal = snapshot_store(&db, hive.clone());
                Store::read_task_comments(&journal, original, &"task".into(), &["q".into()])
                    .await
                    .unwrap();
                let mut state = hive.state.lock().unwrap();
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
                            && id.as_str() != "node12"
                    })
                    .map(|(id, _)| id.clone())
                    .collect::<Vec<_>>();
                removed = ids
                    .iter()
                    .map(|id| (id.clone(), state.records.remove(id).unwrap()))
                    .collect::<Vec<_>>();
                state.comment_reads = 0;
                drop(state);
                assert_eq!(
                    publish_incremental_comment(&journal, &req, true)
                        .await
                        .unwrap_err()
                        .code,
                    "transport_lost",
                    "{case}/{length}"
                );
                assert!(
                    hive.state.lock().unwrap().comment_reads <= 24,
                    "Only selected originals and changed tree paths are needed"
                );
            }
            let writes = hive.state.lock().unwrap().writes;
            {
                let db = Connection::open(&path).unwrap();
                initialize(&db).unwrap();
                let journal = snapshot_store(&db, hive.clone());
                let outcome = publish_incremental_comment(&journal, &req, false)
                    .await
                    .unwrap();
                assert_eq!(
                    outcome,
                    publish_incremental_comment(&journal, &req, false)
                        .await
                        .unwrap()
                );
                assert_eq!(hive.state.lock().unwrap().writes, writes);
                assert_eq!(hive.state.lock().unwrap().task_writes, 2);
                let header = saved(&hive);
                let selected =
                    Store::read_task_comments(&journal, &header, &"task".into(), &["q".into()])
                        .await
                        .unwrap();
                assert_eq!(selected[0].get("recipient"), req.get("recipient"));
                let store = ivy_task_board::lifecycle_journal::LifecycleJournal {
                    journal: &journal,
                    pages: &NoPages,
                };
                let queued = store
                    .queued_comment_ids(&header, &"task".into())
                    .await
                    .unwrap();
                assert_eq!(
                    queued,
                    vec![Json::from("q")],
                    "Original receipt remains in the updated root index"
                );
            }
            {
                let mut state = hive.state.lock().unwrap();
                for (id, row) in removed {
                    state.records.insert(id, row);
                }
            }
            assert_projection(
                hive,
                &before,
                !["user-answered", "before-question"].contains(&case),
            )
            .await;
            std::fs::remove_file(path).unwrap();
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn repeated_ids_reopen_only_questions_without_original_resolution_or_response_coverage() {
    for case in [
        "duplicate-reopen",
        "duplicate-resolved",
        "duplicate-covered",
        "duplicate-native",
    ] {
        let fixture = fixture(300, case);
        let hive = Arc::new(Hive::new(&fixture));
        let original = fixture
            .required("task")
            .unwrap()
            .required("content")
            .unwrap()
            .required("value")
            .unwrap();
        let before = hydrated(hive.clone(), original, &"task".into()).await;
        let db = Connection::open_in_memory().unwrap();
        initialize(&db).unwrap();
        let journal = snapshot_store(&db, hive.clone());
        publish_incremental_comment(&journal, &request(true), false)
            .await
            .unwrap();
        assert_projection(hive.clone(), &before, true).await;
        let value = saved(&hive);
        let full = hydrated(hive, &value, &"task".into()).await;
        assert_eq!(
            ivy_sdk::open_task_questions(&full, None).unwrap().len(),
            if case == "duplicate-reopen" { 2 } else { 1 }
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn a_closed_duplicate_summary_cannot_authorize_another_questions_agent() {
    let mut fixture = fixture(65, "duplicate-reopen");
    let contracts = runtime_inputs::contracts().unwrap();
    let Json::Array(nodes) = fixture.get_mut("nodes").unwrap() else {
        unreachable!()
    };
    let old = nodes
        .iter_mut()
        .find(|r| r.required("object").unwrap().get("id") == Some(&"node3".into()))
        .unwrap();
    old.get_mut("content")
        .unwrap()
        .get_mut("value")
        .unwrap()
        .get_mut("entry")
        .unwrap()
        .get_mut("comment")
        .unwrap()
        .get_mut("author")
        .unwrap()
        .insert("principalId", "closed-agent".into());
    consumer_reads::refresh(old, &contracts);
    let task = fixture.get_mut("task").unwrap();
    let value = task.get_mut("content").unwrap().get_mut("value").unwrap();
    // Historical ID-based summaries can list the closed earlier occurrence too.
    value.get_mut("commentStore").unwrap().get_mut("summary").unwrap().insert("questions", json(r#"[{"commentId":"q","sequence":3,"recipient":{"kind":"user"},"open":true},{"commentId":"q","sequence":12,"recipient":{"kind":"user"},"open":true}]"#));
    value.insert("openQuestionCount", 2.0.into());
    consumer_reads::refresh(task, &contracts);
    let hive = Arc::new(Hive::new(&fixture));
    let db = Connection::open_in_memory().unwrap();
    initialize(&db).unwrap();
    let journal = snapshot_store(&db, hive.clone());
    let store = ivy_task_board::lifecycle_journal::LifecycleJournal {
        journal: &journal,
        pages: &NoPages,
    };
    let checks = EditChecks(&store, &journal.contracts);
    let validation = ivy_task_board::engine_validation::ActionValidation {
        store: &store,
        checks: &checks,
    };
    let original = saved(&hive);
    let mut caller = actor();
    caller.insert("source", "worker".into());
    validation
        .validate(
            &request(true),
            &original,
            &caller,
            &TIME.into(),
            &"policy".into(),
        )
        .await
        .unwrap();
    caller.insert("principalId", "closed-agent".into());
    assert_eq!(
        validation
            .validate(
                &request(true),
                &original,
                &caller,
                &TIME.into(),
                &"policy".into()
            )
            .await
            .unwrap_err()
            .code,
        "task_board_forbidden"
    );
    let before = hydrated(hive.clone(), &original, &"task".into()).await;
    publish_incremental_comment(&journal, &request(true), false)
        .await
        .unwrap();
    assert_projection(hive, &before, true).await;
}

#[tokio::test(flavor = "current_thread")]
async fn retarget_rejects_forged_body_missing_history_and_revision_conflicts() {
    let fixture = fixture(65, "task-answered");
    let hive = Arc::new(Hive::new(&fixture));
    let db = Connection::open_in_memory().unwrap();
    initialize(&db).unwrap();
    let journal = snapshot_store(&db, hive.clone());
    let original = saved(&hive);
    let store = ivy_task_board::lifecycle_journal::LifecycleJournal {
        journal: &journal,
        pages: &NoPages,
    };
    let checks = EditChecks(&store, &journal.contracts);
    let validation = ivy_task_board::engine_validation::ActionValidation {
        store: &store,
        checks: &checks,
    };
    let mut caller = actor();
    caller.insert("source", "worker".into());
    validation
        .validate(
            &request(true),
            &original,
            &caller,
            &TIME.into(),
            &"policy".into(),
        )
        .await
        .unwrap();
    caller.insert("principalId", "other-agent".into());
    assert_eq!(
        validation
            .validate(
                &request(true),
                &original,
                &caller,
                &TIME.into(),
                &"policy".into()
            )
            .await
            .unwrap_err()
            .code,
        "task_board_forbidden"
    );
    let mut own = request(true);
    own.get_mut("recipient")
        .unwrap()
        .insert("taskId", "task".into());
    assert_eq!(
        validation
            .validate(&own, &original, &actor(), &TIME.into(), &"policy".into())
            .await
            .unwrap_err()
            .code,
        "invalid_arguments"
    );
    hive.state
        .lock()
        .unwrap()
        .records
        .get_mut("peer")
        .unwrap()
        .get_mut("object")
        .unwrap()
        .insert("effectivelyArchived", true.into());
    assert_eq!(
        validation
            .validate(
                &request(true),
                &original,
                &actor(),
                &TIME.into(),
                &"policy".into()
            )
            .await
            .unwrap_err()
            .code,
        "task_board_record_archived"
    );
    hive.state
        .lock()
        .unwrap()
        .records
        .get_mut("peer")
        .unwrap()
        .get_mut("object")
        .unwrap()
        .insert("effectivelyArchived", false.into());
    let mut changes = Store::read_task_comments(&journal, &original, &"task".into(), &["q".into()])
        .await
        .unwrap();
    changes[0].insert("recipient", recipient(true));
    changes[0].insert("body", "Forged body".into());
    let mut delta = original.clone();
    delta.insert("comments", Json::Array(changes));
    assert_eq!(
        journal
            .prepare_task_change(&delta, &destination(1.0), TaskChange::QuestionRecipient)
            .await
            .err()
            .unwrap()
            .code,
        "task_board_comment_mismatch"
    );
    assert_eq!(hive.state.lock().unwrap().writes, 0);
    let missing = hive.state.lock().unwrap().records.remove("node25").unwrap();
    db.execute("DELETE FROM comment_index_roots", []).unwrap();
    assert_eq!(
        publish_incremental_comment(&journal, &request(true), false)
            .await
            .unwrap_err()
            .code,
        "not_found"
    );
    assert_eq!(hive.state.lock().unwrap().task_writes, 0);
    hive.state
        .lock()
        .unwrap()
        .records
        .insert("node25".into(), missing);
    let mut wrong = request(true);
    wrong.insert("operationId", "conflict".into());
    {
        let mut state = hive.state.lock().unwrap();
        let original = state.records["task"].clone();
        let mut current = original.clone();
        current
            .get_mut("object")
            .unwrap()
            .insert("currentRevision", 2.0.into());
        current
            .get_mut("revision")
            .unwrap()
            .insert("revision", 2.0.into());
        current
            .get_mut("content")
            .unwrap()
            .get_mut("value")
            .unwrap()
            .get_mut("fields")
            .unwrap()
            .insert("priority", 4.0.into());
        consumer_reads::refresh(&mut current, &journal.contracts);
        state.revisions.insert(("task".into(), 1), original);
        state.revisions.insert(("task".into(), 2), current.clone());
        state.records.insert("task".into(), current);
    }
    assert_eq!(
        publish_incremental_comment(&journal, &wrong, false)
            .await
            .unwrap_err()
            .code,
        "revision_conflict"
    );
    assert_eq!(hive.state.lock().unwrap().task_writes, 0);
}
