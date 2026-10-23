#[path = "task-board-action-support.rs"]
mod support;
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString, Outcome,
    checked_native_contract::CheckedNativeContracts, hash::hash_json,
};
use ivy_task_board::{
    engine_actions::{ActionRestore, TaskActions, handoff_work, result_comment_fits},
    engine_publication::{ExistingTaskPublication, empty_outcome},
    engine_validation::ActionValidation,
    journal_store::persistent_reads::task_comments::validate_task_content,
    native_lifecycle::{LifecycleDocument, LifecycleStore},
    operations::{Document, Operations, Store},
    publication::Publication,
};
use std::{
    path::Path,
    sync::{Arc, Mutex, OnceLock},
};
use support::{AT, JsonEdit, World, actor, claim, comment_request, pin, request, task};
fn f<'a>(v: &'a Json, key: &str) -> &'a Json {
    support::f(v, key)
}
fn s<'a>(v: &'a Json, key: &str) -> &'a JsString {
    support::s(v, key)
}
async fn publish(
    world: &World,
    request: &Json,
    source: &str,
    run: Option<Json>,
    work: Option<Json>,
) -> IvyResult<Json> {
    let store = world;
    let ops = Operations {
        store,
        now: || AT.to_string(),
    };
    let mut op = ops.begin(request, &actor(source)).await?;
    if let Some(run) = run {
        op.value.insert("handoffRun", run);
    }
    if let Some(work) = work {
        op.value.insert("handoffWork", work);
    }
    publish_operation(world, &ops, &op).await
}
async fn publish_operation<C: Fn() -> String>(
    world: &World,
    ops: &Operations<&World, C>,
    op: &Document,
) -> IvyResult<Json> {
    let store = world;
    let read = |address: Json,
                primary: bool|
     -> futures_util::future::LocalBoxFuture<'_, IvyResult<LifecycleDocument>> {
        Box::pin(async move { world.read_task(&address, primary) })
    };
    let validation = ActionValidation {
        store: &store,
        checks: &store,
        task_read: &read,
    };
    let catalogs =
        CheckedNativeContracts::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."));
    let settings = Json::object([("phoneTarget", Json::Null)]);
    let actions = TaskActions {
        store: &store,
        settings: &settings,
        catalogs: &catalogs,
        policies: &validation,
        lifecycle: &store,
        task_read: &read,
    };
    let mut steps = Publication {
        operations: ops,
        operation: op,
        task_written: None,
        index: 0.0,
    };
    actions
        .publish(
            op,
            &mut steps,
            empty_outcome(s(&op.value, "operationId")),
            None,
        )
        .await
}
#[tokio::test(flavor = "current_thread")]
async fn comments_queue_manual_terminal_inputs_without_changing_work_or_result_state() {
    for state in ["todo", "backlog", "waiting", "review", "done", "cancelled"] {
        let mut task = task();
        task.required_mut("fields")
            .unwrap()
            .insert("control", "user".into());
        task.insert("workflowState", state.into());
        task.insert(
            "runtimeWait",
            Json::object([
                ("reason", "host".into()),
                ("since", AT.into()),
                ("retryAt", "2026-10-23T00:01:00.000Z".into()),
            ]),
        );
        let world = World::new(task);
        publish(&world, &comment_request(), "user", None, None)
            .await
            .unwrap();
        let saved = world.saved().value;
        assert_eq!(f(&saved, "workflowState"), &Json::from(state));
        assert_eq!(f(&saved, "workRevision"), &Json::Number(0.0));
        assert_eq!(f(&saved, "runtimeWait").get("retryAt"), Some(&Json::Null));
        assert_eq!(f(&saved, "commentDeliveries").as_array().unwrap().len(), 1);
    }
}
#[tokio::test(flavor = "current_thread")]
async fn own_agent_comment_does_not_queue_itself_and_opt_out_preserves_runtime_retry() {
    for source in ["worker", "user"] {
        let mut task = task();
        task.insert(
            "runtimeWait",
            Json::object([
                ("reason", "host".into()),
                ("retryAt", "2026-10-23T00:01:00.000Z".into()),
            ]),
        );
        let world = World::new(task.clone());
        let mut req = comment_request();
        if source == "user" {
            req.insert("agentDelivery", "none".into());
        }
        publish(&world, &req, source, None, None).await.unwrap();
        let saved = world.saved().value;
        assert!(
            f(&saved, "commentDeliveries")
                .as_array()
                .unwrap()
                .is_empty()
        );
        support::same(f(&saved, "runtimeWait"), f(&task, "runtimeWait"));
    }
}
#[tokio::test(flavor = "current_thread")]
async fn worker_handoff_and_schedule_are_saved_without_applying_workflow_intent_early() {
    let mut task = task();
    claim(&mut task);
    let world = World::new(task);
    world.create("task-board/task", "peer", &support::task());
    let mut req = comment_request();
    req.insert("purpose", "handoff".into());
    req.insert(
        "handoff",
        Json::object([
            ("workflowState", "review".into()),
            ("wakeAt", "2026-10-24T00:00:00.000Z".into()),
            ("dependencies", Json::Array(vec!["peer".into()])),
        ]),
    );
    publish(&world, &req, "worker", None, None).await.unwrap();
    let saved = world.saved().value;
    assert_eq!(f(&saved, "workflowState"), &Json::from("in_progress"));
    assert!(!f(&saved, "claim").is_null());
    assert_eq!(f(&saved, "wakeAt"), &Json::from("2026-10-24T00:00:00.000Z"));
    assert!(f(&saved, "observedDependencies").get("peer").is_some());
    let c = &f(&saved, "comments").as_array().unwrap()[0];
    support::same(f(c, "handoff"), f(&req, "handoff"));
    assert!(
        f(&saved, "commentDeliveries")
            .as_array()
            .unwrap()
            .is_empty()
    );
}
#[tokio::test(flavor = "current_thread")]
async fn claimed_native_option_edits_request_execution_change_without_changing_assignment_revision()
{
    let mut task = task();
    claim(&mut task);
    let world = World::new(task.clone());
    let mut edit = request("edit");
    let mut fields = f(&task, "fields").clone();
    fields.insert("nativeOptions", Json::object([("model", "next".into())]));
    edit.insert("fields", fields);
    publish(&world, &edit, "user", None, None).await.unwrap();
    let saved = world.saved().value;
    assert_eq!(f(&saved, "executionChangeRequested"), &Json::Bool(true));
    assert_eq!(f(&saved, "workRevision"), &Json::Number(0.0));
    assert!(f(&saved, "comments").as_array().unwrap().is_empty());
}
#[tokio::test(flavor = "current_thread")]
async fn defer_preserves_runtime_availability_and_a_claimed_worker_records_completion_intent() {
    let mut task = task();
    claim(&mut task);
    task.insert(
        "runtimeWait",
        Json::object([("reason", "native".into()), ("retryAt", Json::Null)]),
    );
    let world = World::new(task.clone());
    let mut req = request("defer");
    req.insert("wakeAt", "2026-10-24T00:00:00.000Z".into());
    publish(&world, &req, "worker", None, None).await.unwrap();
    let saved = world.saved().value;
    assert_eq!(f(&saved, "workflowState"), &Json::from("in_progress"));
    support::same(f(&saved, "runtimeWait"), f(&task, "runtimeWait"));
    let c = &f(&saved, "comments").as_array().unwrap()[0];
    assert_eq!(f(f(c, "handoff"), "workflowState"), &Json::from("waiting"));
    assert_eq!(f(&saved, "agentCommentCount"), &Json::Number(1.0));
}
#[tokio::test(flavor = "current_thread")]
async fn append_only_publication_rebases_but_assignment_changes_prevent_execution_rebase() {
    let world = World::new(task());
    let mut edited = world.saved().value;
    edited
        .required_mut("fields")
        .unwrap()
        .insert("title", "Concurrent title".into());
    world.replace(&edited);
    publish(&world, &comment_request(), "user", None, None)
        .await
        .unwrap();
    let saved = world.saved().value;
    assert_eq!(
        f(f(&saved, "fields"), "title"),
        &Json::from("Concurrent title")
    );
    let mut start = request("start");
    start.insert("commentIds", Json::Array(vec!["input".into()]));
    let e = publish(&world, &start, "scheduler", None, None)
        .await
        .unwrap_err();
    assert_eq!(e.code, "revision_conflict");
    assert_eq!(f(&world.saved().pin, "revision"), &Json::Number(4.0));
}
#[tokio::test(flavor = "current_thread")]
async fn retarget_changes_the_question_without_creating_an_answer_or_queued_input() {
    let mut task = task();
    task.insert(
        "comments",
        Json::Array(vec![support::question(
            "q",
            Json::object([("kind", "user".into())]),
        )]),
    );
    let world = World::new(task);
    world.create("task-board/task", "peer", &support::task());
    let mut req = request("retargetQuestion");
    req.insert("questionId", "q".into());
    req.insert(
        "recipient",
        Json::object([("kind", "task".into()), ("taskId", "peer".into())]),
    );
    publish(&world, &req, "user", None, None).await.unwrap();
    let saved = world.saved().value;
    let comments = f(&saved, "comments").as_array().unwrap();
    assert_eq!(comments.len(), 1);
    support::same(f(&comments[0], "recipient"), f(&req, "recipient"));
    assert_eq!(ivy_sdk::open_task_questions(&saved, None).unwrap().len(), 1);
    assert!(
        f(&saved, "commentDeliveries")
            .as_array()
            .unwrap()
            .is_empty()
    );
}
#[test]
fn comment_capacity_is_per_persisted_header_and_entry_instead_of_total_hydrated_history() {
    let mut task = task();
    let mut c = support::question("q", Json::object([("kind", "user".into())]));
    c.insert("body", "x".repeat(4096).into());
    task.insert(
        "comments",
        Json::Array(
            (0..600)
                .map(|n| {
                    let mut c = c.clone();
                    c.insert("commentId", n.to_string().into());
                    c
                })
                .collect(),
        ),
    );
    validate_task_content(&task, &"task".into()).unwrap();
    assert!(result_comment_fits(&task, &c, &pin("op", 1.0), &"task".into()).unwrap());
    c.insert("body", "x".repeat(2 * 1024 * 1024).into());
    assert!(!result_comment_fits(&task, &c, &pin("op", 1.0), &"task".into()).unwrap());
    task.insert(
        "attachments",
        Json::Array(vec![Json::object([(
            "filename",
            "x".repeat(2 * 1024 * 1024).into(),
        )])]),
    );
    assert_eq!(
        validate_task_content(&task, &"task".into())
            .unwrap_err()
            .code,
        "content_too_large"
    );
}
#[path = "task-board-engine-actions-journal.rs"]
mod journal;

#[tokio::test(flavor = "current_thread")]
async fn admitted_handoff_keeps_original_binding_and_tolerates_pending_native_option_changes() {
    let mut initial = task();
    claim(&mut initial);
    let world = World::new(initial.clone());
    let ops = Operations {
        store: &world,
        now: || AT.to_string(),
    };
    let mut req = comment_request();
    req.insert("purpose", "handoff".into());
    req.insert(
        "handoff",
        Json::object([("workflowState", "waiting".into())]),
    );
    let bound = std::cell::Cell::new(0);
    let original = ops
        .begin_with_handoff(&req, &actor("worker"), || async {
            bound.set(bound.get() + 1);
            Ok(Json::object([
                ("run", f(&initial, "lastRun").clone()),
                ("work", handoff_work(&initial)?),
            ]))
        })
        .await
        .unwrap();
    assert_eq!(bound.get(), 1);
    let mut current = initial.clone();
    current
        .required_mut("fields")
        .unwrap()
        .insert("nativeOptions", Json::object([("model", "next".into())]));
    current
        .required_mut("fields")
        .unwrap()
        .insert("priority", Json::Number(4.0));
    current.insert("executionChangeRequested", true.into());
    current.insert("workRevision", Json::Number(1.0));
    world.replace(&current);
    let replay = ops
        .begin_with_handoff(&req, &actor("worker"), || async {
            panic!("The retained handoff binding must not be recomputed")
        })
        .await
        .unwrap();
    support::same(
        f(&replay.value, "handoffWork"),
        f(&original.value, "handoffWork"),
    );
    publish_operation(&world, &ops, &replay).await.unwrap();
    let saved = world.saved().value;
    support::same(
        f(f(&saved, "fields"), "nativeOptions"),
        f(f(&current, "fields"), "nativeOptions"),
    );
    assert_eq!(f(&saved, "executionChangeRequested"), &Json::Bool(true));
    assert_eq!(f(&saved, "workflowState"), &Json::from("in_progress"));
}

#[tokio::test(flavor = "current_thread")]
async fn admitted_handoff_rejects_replaced_claim_or_changed_work_before_any_task_write() {
    for change in ["title", "run"] {
        let mut initial = task();
        claim(&mut initial);
        let world = World::new(initial.clone());
        let ops = Operations {
            store: &world,
            now: || AT.to_string(),
        };
        let mut req = comment_request();
        req.insert("purpose", "handoff".into());
        req.insert(
            "handoff",
            Json::object([("workflowState", "review".into())]),
        );
        let op = ops
            .begin_with_handoff(&req, &actor("worker"), || async {
                Ok(Json::object([
                    ("run", f(&initial, "lastRun").clone()),
                    ("work", handoff_work(&initial)?),
                ]))
            })
            .await
            .unwrap();
        let mut current = initial.clone();
        if change == "title" {
            current
                .required_mut("fields")
                .unwrap()
                .insert("title", "Different assignment".into());
        } else {
            current
                .required_mut("claim")
                .unwrap()
                .insert("run", pin("other", 1.0));
        }
        world.replace(&current);
        assert_eq!(
            publish_operation(&world, &ops, &op).await.unwrap_err().code,
            "task_board_handoff_changed"
        );
        assert_eq!(f(&world.saved().pin, "revision"), &Json::Number(2.0));
        assert!(f(&world.saved().value, "publication").is_null());
    }
}
