//! Existing admission publishes compact headers and retains exact input receipts.
use super::*;
use ivy_contracts::checked_native_contract::CheckedNativeContracts;
use ivy_sdk::{
    native_plan::{NativePlanKind, save_native_plan},
    tools::ToolClients,
};
use ivy_task_board::{
    checks::{CheckStore, TaskChecks},
    engine_actions::{ActionRestore, TaskActions},
    engine_publication::{ExistingTaskPublication, empty_outcome},
    engine_validation::{ActionValidation, ActionValidationChecks},
    lifecycle_journal::LifecycleJournal,
    native_lifecycle::LifecycleDocument,
    operations::Document,
    publication::Publication,
};
use std::cell::Cell;

fn admission_store<'a>(
    db: &'a Connection,
    hive: Arc<Hive>,
) -> JournalStore<'a, Hive, RuntimeContracts, fn() -> String, fn() -> JsString> {
    let journal = snapshot_store(db, hive.clone());
    // Native plans belong to the local workflow journal, never to Hive Objects.
    if journal.journal.key(&"plan".into()).unwrap().is_none() {
        let state = hive.state.lock().unwrap();
        let value = state.records["plan"]
            .required("content")
            .unwrap()
            .required("value")
            .unwrap();
        journal
            .journal
            .create(
                "task-board/native-plan",
                &"plan".into(),
                None,
                value,
                &TIME.into(),
                &"plan".into(),
            )
            .unwrap();
    }
    journal
}

fn fixture(length: usize, action: &str, control: &str) -> Json {
    let mut fixture = claimed_conversation_fixture(length);
    let contracts = runtime_inputs::contracts().unwrap();
    let Json::Array(nodes) = fixture.get_mut("nodes").unwrap() else {
        unreachable!()
    };
    let draft = nodes
        .iter()
        .find(|row| row.required("object").unwrap().get("id") == Some(&"run".into()))
        .unwrap()
        .required("content")
        .unwrap()
        .required("value")
        .unwrap()
        .required("plan")
        .unwrap()
        .clone();
    let catalogs = CheckedNativeContracts::new(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."),
    );
    let catalog = catalogs
        .checked_native_contract(
            draft
                .required("nativeVersion")
                .unwrap()
                .as_js_string()
                .unwrap(),
            Some(&Json::object([(
                "sourceHash",
                draft.required("catalogSourceHash").unwrap().clone(),
            )])),
        )
        .unwrap();
    let plan = save_native_plan(
        &catalog,
        &draft,
        NativePlanKind::TaskBoard,
        None,
        &"plan".into(),
    )
    .unwrap();
    let mut row = record(
        "plan",
        "task-board/native-plan",
        plan,
        1.0,
        Json::Object(vec![]),
    );
    row.get_mut("object")
        .unwrap()
        .insert("parentId", Json::Null);
    row.get_mut("object")
        .unwrap()
        .insert("ownerObjectId", Json::Null);
    row.get_mut("revision").unwrap().insert(
        "contractVersion",
        contracts.version("task-board/native-plan").unwrap().into(),
    );
    consumer_reads::refresh(&mut row, &contracts);
    nodes.push(row);
    for row in nodes {
        let value = row.get_mut("content").unwrap().get_mut("value").unwrap();
        if let Some(entry) = value.get_mut("entry") {
            let comment = entry.get_mut("comment").unwrap();
            comment.insert("body", "Retained input".into());
            if comment.get("sequence") == Some(&12.0.into()) {
                comment.insert("purpose", "question".into());
                comment.insert("recipient", json(r#"{"kind":"user"}"#));
            }
            if comment.get("sequence") == Some(&1.0.into()) {
                entry.insert("receipt", json(r#"{"commentId":"c1","state":"queued","operationId":"original-input","updatedAt":"2026-01-02T03:04:05.006Z","code":null}"#));
            }
        }
        consumer_reads::refresh(row, &contracts);
    }
    let row = fixture.get_mut("task").unwrap();
    let task = row.get_mut("content").unwrap().get_mut("value").unwrap();
    task.get_mut("fields")
        .unwrap()
        .insert("control", control.into());
    task.insert("openQuestionCount", 1.0.into());
    task.get_mut("commentStore")
        .unwrap()
        .get_mut("summary")
        .unwrap()
        .insert(
            "questions",
            Json::Array(vec![json(
                r#"{"commentId":"c12","sequence":12,"open":true,"recipient":{"kind":"user"}}"#,
            )]),
        );
    if action != "worker-defer" {
        task.insert("claim", Json::Null);
        task.insert("workflowState", "waiting".into());
    }
    if action == "start" {
        task.insert("primaryResourceRef", Json::Null);
        task.insert("lastRun", Json::Null);
        task.insert("attemptCount", 0.0.into());
    }
    consumer_reads::refresh(row, &contracts);
    fixture
}
fn request(action: &str, operation: &str, comments: &[&str]) -> Json {
    let mut value = Json::object([
        ("action", action.into()),
        ("operationId", operation.into()),
        ("taskId", "task".into()),
        ("expectedRevision", 1.0.into()),
    ]);
    if action == "defer" {
        value.insert("wakeAt", "2026-01-03T03:04:05.006Z".into());
    } else {
        value.insert(
            "commentIds",
            Json::Array(comments.iter().map(|id| (*id).into()).collect()),
        );
        value.insert("intent", json(r#"{"objectId":"plan","revision":1}"#));
        value.insert(
            "target",
            json(r#"{"hostId":"fixture","serviceNodeId":"native-agent"}"#),
        );
        value.insert("workspace", json(r#"{"hostId":"fixture","serviceNodeId":"native-agent","canonicalCwd":"/fixture","taskRoot":"/fixture","bootstrapPath":"/fixture","intendedPath":"/fixture","sourcePath":null,"project":null,"useWorktree":false,"repository":null}"#));
        if action == "continue" {
            value.insert("feedback", "Original feedback".into());
        }
    }
    value
}
struct Checks<'a, S>(&'a S, &'a RuntimeContracts, &'a CheckedNativeContracts);
impl<S: CheckStore> ActionValidationChecks for Checks<'_, S> {
    async fn aggregate(&self, task: &Json, id: &JsString) -> IvyResult<()> {
        EditChecks(self.0, self.1).aggregate(task, id).await
    }
    async fn fields(&self, fields: &Json, id: &JsString) -> IvyResult<()> {
        EditChecks(self.0, self.1).fields(fields, id).await
    }
    async fn execution_options(&self, _: &Json, _: Option<&Json>) -> IvyResult<()> {
        panic!("Options unchanged")
    }
    async fn execution(
        &self,
        _: &Json,
        intent: &Json,
        _: &JsString,
        _: &JsString,
        _: &Json,
        _: &Json,
    ) -> IvyResult<Json> {
        // Owner/provider admission is exercised by the existing checks and Factory suites.
        ivy_task_board::native_plan::read_plan(self.0, self.2, intent).await
    }
    async fn deferral(&self, request: &Json, id: &JsString, at: &JsString) -> IvyResult<()> {
        let tools = ToolClients::new(self.0.client());
        TaskChecks {
            store: self.0,
            catalogs: self.2,
            tools: &tools,
        }
        .deferral(request, Some(id), at)
        .await
    }
    async fn content(&self, _: &Json) -> IvyResult<()> {
        panic!("No result")
    }
    async fn artifact(&self, _: &Json) -> IvyResult<()> {
        panic!("No artifact")
    }
}
#[derive(Default)]
struct Restore(Cell<usize>);
impl ActionRestore for Restore {
    async fn restore_for_execution(&self, operation: &Document, task: &Document) -> IvyResult<()> {
        assert_eq!(
            operation.value.required("request")?.get("action"),
            Some(&"continue".into())
        );
        assert_eq!(
            task.value.required("primaryResourceRef")?.get("nativeId"),
            Some(&"original-thread".into())
        );
        assert!(
            task.value
                .required("comments")?
                .as_array()
                .unwrap()
                .is_empty()
        );
        self.0.set(self.0.get() + 1);
        Ok(())
    }
}
async fn publish(
    journal: &JournalStore<'_, Hive, RuntimeContracts, fn() -> String, fn() -> JsString>,
    request: &Json,
    source: &str,
    restore: &Restore,
    lose: bool,
) -> IvyResult<Json> {
    let store = LifecycleJournal {
        journal,
        pages: &NoPages,
    };
    let ops = Operations {
        store,
        now: edit_clock as fn() -> String,
    };
    let mut caller = actor();
    caller.insert("source", source.into());
    let operation = ops.begin(request, &caller).await?;
    let catalogs = CheckedNativeContracts::new(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."),
    );
    let checks = Checks(&ops.store, &journal.contracts, &catalogs);
    let validation = ActionValidation {
        store: &ops.store,
        checks: &checks,
    };
    let read = |_: Json,
                _: bool|
     -> futures_util::future::LocalBoxFuture<'_, IvyResult<LifecycleDocument>> {
        Box::pin(async { panic!("Admission must not hydrate history") })
    };
    let actions = TaskActions {
        store: &ops.store,
        settings: &Json::object([("phoneTarget", Json::Null)]),
        catalogs: &catalogs,
        policies: &validation,
        lifecycle: restore,
        task_read: &read,
    };
    let callback =
        |_: Json, value: Json| -> futures_util::future::LocalBoxFuture<'_, IvyResult<()>> {
            Box::pin(async move {
                if lose && value.get("publication").is_some_and(|v| !v.is_null()) {
                    journal.client.state.lock().unwrap().lost_task_reply = true;
                }
                Ok(())
            })
        };
    let mut steps = Publication {
        operations: &ops,
        operation: &operation,
        task_written: Some(&callback),
        index: 0.0,
    };
    let outcome = actions
        .publish(
            &operation,
            &mut steps,
            empty_outcome(request.required("operationId")?.as_js_string().unwrap()),
            None,
        )
        .await?;
    ops.finish(
        operation
            .pin
            .to_json()
            .required("objectId")?
            .as_js_string()
            .unwrap(),
        &outcome,
    )
    .await?;
    Ok(outcome)
}
#[tokio::test(flavor = "current_thread")]
async fn start_continue_and_worker_defer_preserve_history_after_lost_reply_and_reopen() {
    for length in [65, 300, 4096] {
        for action in ["start", "continue", "worker-defer", "scheduler-defer"] {
            let fixture = fixture(
                length,
                action,
                if action == "worker-defer" {
                    "agent"
                } else {
                    "user"
                },
            );
            let hive = Arc::new(Hive::new(&fixture));
            let path =
                std::env::temp_dir().join(format!("ivy-admission-{}.sqlite", uuid::Uuid::new_v4()));
            let request = request(
                if action.ends_with("-defer") {
                    "defer"
                } else {
                    action
                },
                "retained-admission",
                &["c1"],
            );
            let source = if action == "worker-defer" {
                "worker"
            } else {
                "scheduler"
            };
            let original = fixture
                .required("task")
                .unwrap()
                .required("content")
                .unwrap()
                .required("value")
                .unwrap();
            let restore = Restore::default();
            let outcome;
            {
                let db = Connection::open(&path).unwrap();
                initialize(&db).unwrap();
                let journal = admission_store(&db, hive.clone());
                let store = LifecycleJournal {
                    journal: &journal,
                    pages: &NoPages,
                };
                let header = store.read_task_header(&"task".into(), true).await.unwrap();
                store
                    .comment_entries(&header.value, &"task".into(), &["c1".into()])
                    .await
                    .unwrap();
                let removed;
                let before;
                {
                    let mut state = hive.state.lock().unwrap();
                    let ids = state
                        .records
                        .iter()
                        .filter(|(id, row)| {
                            id.as_str() != "node1"
                                && id.as_str() != format!("node{length}")
                                && row.required("object").unwrap().get("contractKey")
                                    == Some(&"task-board/comment-node".into())
                                && row
                                    .required("content")
                                    .unwrap()
                                    .required("value")
                                    .unwrap()
                                    .get("kind")
                                    == Some(&"comment".into())
                        })
                        .map(|(id, _)| id.clone())
                        .collect::<Vec<_>>();
                    removed = ids
                        .iter()
                        .map(|id| (id.clone(), state.records.remove(id).unwrap()))
                        .collect::<Vec<_>>();
                    before = state.comment_reads;
                }
                assert_eq!(
                    publish(&journal, &request, source, &restore, true)
                        .await
                        .unwrap_err()
                        .code,
                    "transport_lost"
                );
                assert_eq!(hive.state.lock().unwrap().task_writes, 2);
                {
                    let mut state = hive.state.lock().unwrap();
                    assert!(
                        state.comment_reads - before <= 32,
                        "Warm admission reads only selected inputs and the append path"
                    );
                    for (id, row) in removed {
                        state.records.insert(id, row);
                    }
                }
            }
            {
                let db = Connection::open(&path).unwrap();
                initialize(&db).unwrap();
                let journal = admission_store(&db, hive.clone());
                outcome = publish(&journal, &request, source, &restore, false)
                    .await
                    .unwrap();
                let reads = hive.state.lock().unwrap().comment_reads;
                let replay = publish(&journal, &request, source, &restore, false)
                    .await
                    .unwrap();
                assert_eq!(hash_json(&outcome).unwrap(), hash_json(&replay).unwrap());
                assert_eq!(hive.state.lock().unwrap().task_writes, 2);
                let header = journal
                    .read_task_header(&"task".into(), true)
                    .await
                    .unwrap();
                if action != "worker-defer" {
                    assert_eq!(
                        hash_json(header.value.required("commentStore").unwrap()).unwrap(),
                        hash_json(original.required("commentStore").unwrap()).unwrap()
                    );
                    assert_eq!(hive.state.lock().unwrap().comment_reads, reads);
                    if action == "scheduler-defer" {
                        assert!(header.value.required("claim").unwrap().is_null());
                        assert_eq!(header.value.get("workflowState"), Some(&"waiting".into()));
                        assert_eq!(header.value.get("wakeAt"), request.get("wakeAt"));
                    } else {
                        let run = journal
                            .read(
                                "task-board/run",
                                header.value.required("lastRun").unwrap(),
                                true,
                            )
                            .await
                            .unwrap();
                        assert_eq!(
                            hash_json(run.value.required("originalRequest").unwrap()).unwrap(),
                            hash_json(&request).unwrap()
                        );
                        assert_eq!(
                            run.value
                                .required("executionContext")
                                .unwrap()
                                .get("workflowState"),
                            Some(&"waiting".into())
                        );
                        assert_eq!(
                            header.value.required("claim").unwrap().get("operationId"),
                            request.get("operationId")
                        );
                    }
                } else {
                    assert_eq!(
                        hash_json(header.value.required("claim").unwrap()).unwrap(),
                        hash_json(original.required("claim").unwrap()).unwrap()
                    );
                    assert_eq!(
                        header.value.get("workflowState"),
                        Some(&"in_progress".into())
                    );
                    let added = journal
                        .read_task_comments(
                            &header.value,
                            &"task".into(),
                            &["retained-admission".into()],
                        )
                        .await
                        .unwrap();
                    assert_eq!(
                        added[0].get("sequence"),
                        Some(&Json::Number((length + 1) as f64))
                    );
                    assert_eq!(added[0].get("purpose"), Some(&"handoff".into()));
                    let completion = journal
                        .read_completion_comments(
                            &header.value,
                            &"task".into(),
                            original
                                .required("lastRun")
                                .unwrap()
                                .required("objectId")
                                .unwrap()
                                .as_js_string()
                                .unwrap(),
                            &[],
                        )
                        .await
                        .unwrap();
                    assert_eq!(
                        completion.handoff.unwrap().get("commentId"),
                        Some(&"retained-admission".into())
                    );
                    assert!(!completion.answered);
                    assert_eq!(
                        added[0].required("handoff").unwrap().get("workflowState"),
                        Some(&"waiting".into())
                    );
                }
                let full = journal
                    .read("task-board/task", &"task".into(), true)
                    .await
                    .unwrap();
                assert_eq!(
                    ivy_sdk::open_task_questions(&full.value, None)
                        .unwrap()
                        .len(),
                    1
                );
                let comments = full.value.required("comments").unwrap().as_array().unwrap();
                let original_hive = Hive::new(&fixture);
                for comment in comments.iter().take(length) {
                    let index = comment.required("sequence").unwrap().as_f64().unwrap() as usize;
                    let state = original_hive.state.lock().unwrap();
                    let old = state.records[&format!("node{index}")]
                        .required("content")
                        .unwrap()
                        .required("value")
                        .unwrap()
                        .required("entry")
                        .unwrap()
                        .required("comment")
                        .unwrap();
                    assert_eq!(hash_json(comment).unwrap(), hash_json(old).unwrap());
                }
                let receipts = full
                    .value
                    .required("commentDeliveries")
                    .unwrap()
                    .as_array()
                    .unwrap();
                assert_eq!(receipts.len(), 1);
                assert_eq!(
                    receipts[0].get("operationId"),
                    Some(&"original-input".into())
                );
                assert_eq!(receipts[0].get("state"), Some(&"queued".into()));
            }
            assert_eq!(restore.0.get(), usize::from(action == "continue"));
            std::fs::remove_file(path).unwrap();
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn blocked_execution_does_not_read_a_missing_conversation_or_infer_no_inputs() {
    for blocked in [true, false] {
        let hive = Arc::new(Hive::new(&fixture(65, "start", "user")));
        {
            let mut state = hive.state.lock().unwrap();
            state.records.retain(|_, row| {
                row.required("object").unwrap().get("contractKey")
                    != Some(&"task-board/comment-node".into())
            });
        }
        let db = Connection::open_in_memory().unwrap();
        initialize(&db).unwrap();
        let journal = admission_store(&db, hive.clone());
        let store = LifecycleJournal {
            journal: &journal,
            pages: &NoPages,
        };
        let mut header = store.read_task_header(&"task".into(), true).await.unwrap();
        if blocked {
            header.value.insert("runtimeWait",json(r#"{"reason":"host","detail":"Unavailable","retryAt":"2026-01-03T03:04:05.006Z","pending":null}"#));
        }
        let catalogs = CheckedNativeContracts::new(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."),
        );
        let tools = ToolClients::new(hive.clone());
        let checks = TaskChecks {
            store: &store,
            catalogs: &catalogs,
            tools: &tools,
        };
        let req = request("start", "wake-probe", &["c1"]);
        let error = checks
            .execution(
                &header.value,
                req.required("intent").unwrap(),
                &"task".into(),
                &TIME.into(),
                req.get("target"),
                req.get("workspace"),
            )
            .await
            .unwrap_err();
        assert_eq!(
            error.code,
            if blocked {
                "task_board_input_required"
            } else {
                "not_found"
            }
        );
        assert_eq!(hive.state.lock().unwrap().comment_reads, 0);
    }
}
#[tokio::test(flavor = "current_thread")]
async fn compact_admission_rejects_unqueued_or_own_inputs_and_keeps_real_wake_checks() {
    for (selected, own, repeated) in [
        ("c1", false, false),
        ("c2", false, false),
        ("absent", false, false),
        ("c1", true, false),
        ("c1", true, true),
    ] {
        let mut fixture = fixture(300, "start", "user");
        if own {
            let Json::Array(nodes) = fixture.get_mut("nodes").unwrap() else {
                unreachable!()
            };
            let row = nodes
                .iter_mut()
                .find(|row| row.required("object").unwrap().get("id") == Some(&"node1".into()))
                .unwrap();
            row.get_mut("content")
                .unwrap()
                .get_mut("value")
                .unwrap()
                .get_mut("entry")
                .unwrap()
                .get_mut("comment")
                .unwrap()
                .insert("authorKind", "agent".into());
            consumer_reads::refresh(row, &runtime_inputs::contracts().unwrap());
            if repeated {
                let row = nodes
                    .iter_mut()
                    .find(|row| row.required("object").unwrap().get("id") == Some(&"node2".into()))
                    .unwrap();
                row.get_mut("content")
                    .unwrap()
                    .get_mut("value")
                    .unwrap()
                    .get_mut("entry")
                    .unwrap()
                    .get_mut("comment")
                    .unwrap()
                    .insert("commentId", "c1".into());
                consumer_reads::refresh(row, &runtime_inputs::contracts().unwrap());
            }
        }
        let hive = Arc::new(Hive::new(&fixture));
        let db = Connection::open_in_memory().unwrap();
        initialize(&db).unwrap();
        let journal = admission_store(&db, hive.clone());
        let req = request("start", "admission-probe", &[selected]);
        if selected != "c1" || own && !repeated {
            assert_eq!(
                publish(&journal, &req, "scheduler", &Restore::default(), false)
                    .await
                    .unwrap_err()
                    .code,
                "task_board_invalid_transition"
            );
            assert_eq!(hive.state.lock().unwrap().task_writes, 0);
        }
        let store = LifecycleJournal {
            journal: &journal,
            pages: &NoPages,
        };
        let header = store.read_task_header(&"task".into(), true).await.unwrap();
        let catalogs = CheckedNativeContracts::new(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."),
        );
        let tools = ToolClients::new(hive.clone());
        let checks = TaskChecks {
            store: &store,
            catalogs: &catalogs,
            tools: &tools,
        };
        let admission_checks = Checks(&store, &journal.contracts, &catalogs);
        let validation = ActionValidation {
            store: &store,
            checks: &admission_checks,
        };
        let mut scheduler = actor();
        scheduler.insert("source", "scheduler".into());
        let deferred = validation
            .validate(
                &request("defer", "defer-probe", &[]),
                &header.value,
                &scheduler,
                &TIME.into(),
                &"defer-probe".into(),
            )
            .await;
        if own && !repeated {
            assert_eq!(deferred.unwrap_err().code, "task_board_user_controlled");
        } else {
            deferred.unwrap();
        }
        // The real wake/plan admission must reach the provider-read boundary for queued user input.
        assert_eq!(
            checks
                .execution(
                    &header.value,
                    req.required("intent").unwrap(),
                    &"task".into(),
                    &TIME.into(),
                    req.get("target"),
                    req.get("workspace")
                )
                .await
                .unwrap_err()
                .code,
            if own && !repeated {
                "task_board_input_required"
            } else {
                "preflight_reached"
            }
        );
        if repeated {
            validation
                .validate(
                    &req,
                    &header.value,
                    &scheduler,
                    &TIME.into(),
                    &"historical-id".into(),
                )
                .await
                .unwrap();
        }
        let mut blocked = header.value.clone();
        blocked.insert("runtimeWait",json(r#"{"reason":"host","detail":"Unavailable","retryAt":"2026-01-03T03:04:05.006Z","pending":null}"#));
        assert_eq!(
            checks
                .execution(
                    &blocked,
                    req.required("intent").unwrap(),
                    &"task".into(),
                    &TIME.into(),
                    req.get("target"),
                    req.get("workspace")
                )
                .await
                .unwrap_err()
                .code,
            "task_board_input_required"
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn compact_admission_rebases_unrelated_edits_but_rejects_changed_assignment_and_schedule() {
    for change in ["priority", "title", "nativeOptions", "wakeAt", "primary"] {
        let fixture = fixture(65, "continue", "user");
        let hive = Arc::new(Hive::new(&fixture));
        let mut current = fixture.required("task").unwrap().clone();
        let value = current
            .get_mut("content")
            .unwrap()
            .get_mut("value")
            .unwrap();
        match change {
            "priority" => {
                value
                    .get_mut("fields")
                    .unwrap()
                    .insert("priority", 4.0.into());
            }
            "title" => {
                value
                    .get_mut("fields")
                    .unwrap()
                    .insert("title", "Changed assignment".into());
            }
            "nativeOptions" => {
                value.get_mut("fields").unwrap().insert(
                    "nativeOptions",
                    json(r#"{"model":"other","reasoningEffort":null,"serviceTier":null}"#),
                );
            }
            "wakeAt" => {
                value.insert("wakeAt", "2026-01-03T03:04:05.006Z".into());
            }
            _ => {
                value
                    .get_mut("primaryResourceRef")
                    .unwrap()
                    .insert("nativeId", "another-thread".into());
            }
        }
        current
            .get_mut("object")
            .unwrap()
            .insert("currentRevision", 2.0.into());
        current
            .get_mut("revision")
            .unwrap()
            .insert("revision", 2.0.into());
        consumer_reads::refresh(&mut current, &runtime_inputs::contracts().unwrap());
        {
            let mut state = hive.state.lock().unwrap();
            state.records.insert("task".into(), current.clone());
            state.revisions.insert(("task".into(), 2), current);
        }
        let db = Connection::open_in_memory().unwrap();
        initialize(&db).unwrap();
        let journal = admission_store(&db, hive.clone());
        let result = publish(
            &journal,
            &request("continue", "rebased-original", &["c1"]),
            "scheduler",
            &Restore::default(),
            false,
        )
        .await;
        if change == "priority" {
            result.unwrap();
            let header = journal
                .read_task_header(&"task".into(), true)
                .await
                .unwrap();
            assert_eq!(
                header.value.required("fields").unwrap().get("priority"),
                Some(&4.0.into())
            );
            assert_eq!(
                header
                    .value
                    .required("claim")
                    .unwrap()
                    .required("request")
                    .unwrap()
                    .get("expectedRevision"),
                Some(&1.0.into())
            );
            assert_eq!(header.pin.to_json().get("revision"), Some(&4.0.into()));
        } else {
            assert_eq!(result.unwrap_err().code, "revision_conflict");
            assert_eq!(hive.state.lock().unwrap().task_writes, 0);
        }
    }
}
