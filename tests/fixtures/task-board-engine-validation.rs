#[path = "task-board-action-support.rs"]
mod support;
use ivy_contracts::{IvyJson as Json, IvyResult, JsString};
use ivy_task_board::engine_validation::{ActionValidation, check_aggregate};
use support::*;

async fn validate(world: &World, request: &Json, task: &Json, source: &str) -> IvyResult<()> {
    let store = world;
    let read = |address: Json,
                primary: bool|
     -> futures_util::future::LocalBoxFuture<
        '_,
        IvyResult<ivy_task_board::native_lifecycle::LifecycleDocument>,
    > { Box::pin(async move { world.read_task(&address, primary) }) };
    ActionValidation {
        store: &store,
        checks: &store,
        task_read: &read,
    }
    .validate(
        request,
        task,
        &actor(source),
        &AT.into(),
        &"operation".into(),
    )
    .await
}
#[tokio::test(flavor = "current_thread")]
async fn queued_external_inputs_admit_manual_and_terminal_turns_without_reopening_them() {
    for state in ["todo", "waiting", "review", "backlog", "done", "cancelled"] {
        for kind in ["user", "agent"] {
            let mut task = task();
            task.required_mut("fields")
                .unwrap()
                .insert("control", "user".into());
            task.insert("workflowState", state.into());
            let mut input = question("input", Json::object([("kind", "user".into())]));
            input.insert("authorKind", kind.into());
            if kind == "agent" {
                input.insert("sourceTaskId", "peer".into());
            }
            task.insert("comments", Json::Array(vec![input]));
            task.insert(
                "commentDeliveries",
                Json::Array(vec![Json::object([
                    ("commentId", "input".into()),
                    ("state", "queued".into()),
                ])]),
            );
            let world = World::new(task.clone());
            let mut start = request("start");
            start.insert("commentIds", Json::Array(vec!["input".into()]));
            validate(&world, &start, &task, "scheduler").await.unwrap();
            start.insert("commentIds", Json::Array(vec!["missing".into()]));
            assert_eq!(
                validate(&world, &start, &task, "scheduler")
                    .await
                    .unwrap_err()
                    .code,
                "task_board_invalid_transition"
            );
            assert_eq!(f(&world.saved().value, "workflowState"), &Json::from(state));
        }
    }
    let mut task = task();
    task.required_mut("fields")
        .unwrap()
        .insert("control", "user".into());
    task.insert(
        "comments",
        Json::Array(vec![question(
            "input",
            Json::object([("kind", "user".into())]),
        )]),
    );
    task.insert(
        "commentDeliveries",
        Json::Array(vec![Json::object([
            ("commentId", "input".into()),
            ("state", "queued".into()),
        ])]),
    );
    let world = World::new(task.clone());
    let mut start = request("start");
    start.insert("commentIds", Json::Array(vec!["input".into()]));
    assert_eq!(
        validate(&world, &start, &task, "scheduler")
            .await
            .unwrap_err()
            .code,
        "task_board_invalid_transition"
    );
}
#[tokio::test(flavor = "current_thread")]
async fn claimed_assignment_is_fixed_while_native_options_can_request_a_change() {
    let mut task = task();
    claim(&mut task);
    let world = World::new(task.clone());
    let mut edit = request("edit");
    let mut fields = f(&task, "fields").clone();
    fields.insert("nativeOptions", Json::object([("model", "next".into())]));
    edit.insert("fields", fields.clone());
    validate(&world, &edit, &task, "user").await.unwrap();
    assert_eq!(world.checks.get(), 2);
    fields.insert("title", "Another assignment".into());
    edit.insert("fields", fields);
    assert_eq!(
        validate(&world, &edit, &task, "user")
            .await
            .unwrap_err()
            .code,
        "task_board_claim_active"
    );
    let mut start = request("start");
    start.insert("commentIds", Json::Array(vec![]));
    assert_eq!(
        validate(&world, &start, &task, "scheduler")
            .await
            .unwrap_err()
            .code,
        "task_board_claim_active"
    );
}
#[tokio::test(flavor = "current_thread")]
async fn runtime_waits_belong_to_execution_while_workers_control_their_wake_schedule() {
    let mut task = task();
    let world = World::new(task.clone());
    let mut defer = request("defer");
    defer.insert(
        "runtimeWait",
        Json::object([
            ("reason", "host".into()),
            ("retryAt", "2026-10-23T00:01:00.000Z".into()),
        ]),
    );
    for source in ["user", "worker"] {
        assert_eq!(
            validate(&world, &defer, &task, source)
                .await
                .unwrap_err()
                .code,
            "task_board_forbidden"
        );
    }
    for source in ["scheduler", "recovery"] {
        validate(&world, &defer, &task, source).await.unwrap();
    }
    defer.insert(
        "runtimeWait",
        Json::object([("retryAt", Json::Number(1.0))]),
    );
    assert_eq!(
        validate(&world, &defer, &task, "scheduler")
            .await
            .unwrap_err()
            .code,
        "task_board_invalid_date"
    );
    claim(&mut task);
    defer.remove("runtimeWait");
    defer.insert("wakeAt", "2026-10-24T00:00:00.000Z".into());
    validate(&world, &defer, &task, "worker").await.unwrap();
    assert_eq!(
        validate(&world, &defer, &task, "scheduler")
            .await
            .unwrap_err()
            .code,
        "task_board_claim_active"
    );
}
#[tokio::test(flavor = "current_thread")]
async fn watches_use_current_headers_and_do_not_gate_on_foreign_done_or_legacy_graphs() {
    let task = task();
    let world = World::new(task.clone());
    let mut peer = support::task();
    peer.insert("workflowState", "in_progress".into());
    peer.insert("dependencies", Json::Array(vec!["task".into()]));
    world.create("task-board/task", "peer", &peer);
    let mut edit = request("edit");
    edit.insert("fields", f(&task, "fields").clone());
    edit.insert("dependencies", Json::Array(vec!["peer".into()]));
    validate(&world, &edit, &task, "user").await.unwrap();
    edit.insert("dependencies", Json::Array(vec!["task".into()]));
    assert_eq!(
        validate(&world, &edit, &task, "user")
            .await
            .unwrap_err()
            .code,
        "invalid_arguments"
    );
    edit.insert("dependencies", Json::Array(vec![]));
    edit.insert("wakeAt", "2026-10-24T00:00:00Z".into());
    assert_eq!(
        validate(&world, &edit, &task, "user")
            .await
            .unwrap_err()
            .code,
        "task_board_invalid_date"
    );
}
#[tokio::test(flavor = "current_thread")]
async fn questions_require_recipients_and_native_routing_must_match_the_canonical_source() {
    let task = task();
    let world = World::new(task.clone());
    let mut q = comment_request();
    q.insert("purpose", "question".into());
    assert_eq!(
        validate(&world, &q, &task, "worker")
            .await
            .unwrap_err()
            .code,
        "invalid_arguments"
    );
    q.insert(
        "recipient",
        Json::object([("kind", "task".into()), ("taskId", "task".into())]),
    );
    assert_eq!(
        validate(&world, &q, &task, "worker")
            .await
            .unwrap_err()
            .code,
        "invalid_arguments"
    );
    let mut source = support::task();
    source.insert(
        "comments",
        Json::Array(vec![question(
            "canonical",
            Json::object([("kind", "task".into()), ("taskId", "task".into())]),
        )]),
    );
    world.create("task-board/task", "peer", &source);
    q.remove("recipient");
    q.insert("sourceTaskId", "peer".into());
    q.insert(
        "questionSource",
        Json::object([("taskId", "peer".into()), ("commentId", "canonical".into())]),
    );
    validate(&world, &q, &task, "native").await.unwrap();
    q.insert("recipient", Json::object([("kind", "user".into())]));
    assert_eq!(
        validate(&world, &q, &task, "worker")
            .await
            .unwrap_err()
            .code,
        "task_board_forbidden"
    );
    q.required_mut("questionSource")
        .unwrap()
        .insert("commentId", "invented".into());
    assert_eq!(
        validate(&world, &q, &task, "native")
            .await
            .unwrap_err()
            .code,
        "task_board_forbidden"
    );
}
#[tokio::test(flavor = "current_thread")]
async fn only_the_user_or_original_agent_can_retarget_an_unanswered_ordinary_question() {
    let mut task = task();
    task.insert(
        "comments",
        Json::Array(vec![question("q", Json::object([("kind", "user".into())]))]),
    );
    let world = World::new(task.clone());
    let mut req = request("retargetQuestion");
    req.insert("questionId", "q".into());
    req.insert("recipient", Json::object([("kind", "user".into())]));
    validate(&world, &req, &task, "worker").await.unwrap();
    validate(&world, &req, &task, "user").await.unwrap();
    task.required_mut("comments")
        .unwrap()
        .as_array_mut()
        .unwrap()[0]
        .required_mut("author")
        .unwrap()
        .insert("principalId", "other".into());
    assert_eq!(
        validate(&world, &req, &task, "worker")
            .await
            .unwrap_err()
            .code,
        "task_board_forbidden"
    );
    let mut answer = comment_request();
    answer.insert("replyTo", "q".into());
    answer.insert("authorKind", "user".into());
    answer.insert("commentId", "answer".into());
    task.required_mut("comments")
        .unwrap()
        .as_array_mut()
        .unwrap()
        .push(answer);
    assert_eq!(
        validate(&world, &req, &task, "user")
            .await
            .unwrap_err()
            .code,
        "task_board_comment_mismatch"
    );
}
#[tokio::test(flavor = "current_thread")]
async fn handoff_intent_requires_its_own_worker_and_never_accepts_directed_source_identity() {
    let mut task = task();
    claim(&mut task);
    let world = World::new(task.clone());
    let mut req = comment_request();
    req.insert("purpose", "handoff".into());
    assert_eq!(
        validate(&world, &req, &task, "worker")
            .await
            .unwrap_err()
            .code,
        "task_board_handoff_required"
    );
    req.insert(
        "handoff",
        Json::object([("workflowState", "waiting".into())]),
    );
    validate(&world, &req, &task, "worker").await.unwrap();
    req.insert("sourceTaskId", "peer".into());
    assert_eq!(
        validate(&world, &req, &task, "worker")
            .await
            .unwrap_err()
            .code,
        "task_board_handoff_source"
    );
    req.remove("sourceTaskId");
    assert_eq!(
        validate(&world, &req, &task, "native")
            .await
            .unwrap_err()
            .code,
        "task_board_forbidden"
    );
}
#[tokio::test(flavor = "current_thread")]
async fn aggregate_restored_done_can_keep_progress_but_review_requires_its_own_final_result() {
    let mut task = task();
    task.insert("workflowState", "done".into());
    task.insert("latestResult", pin("result", 1.0));
    let world = World::new(task.clone());
    world.create(
        "task-board/result",
        "result",
        &Json::object([("taskId", "task".into()), ("kind", "progress".into())]),
    );
    check_aggregate(&&world, &task, &"task".into())
        .await
        .unwrap();
    task.insert("workflowState", "review".into());
    assert_eq!(
        check_aggregate(&&world, &task, &"task".into())
            .await
            .unwrap_err()
            .code,
        "task_board_result_mismatch"
    );
}
