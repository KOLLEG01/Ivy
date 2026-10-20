use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString, Outcome,
    json::{canonical, parse},
    limits::MANAGEMENT_FRAME_BYTES,
};
use ivy_task_board::{
    coordination::next_steps::{NextStepPolicies, NextStepStore, assess_steps, step_condition},
    engine_read::{
        comment_reads::{CommentReadPolicies, comment_summary},
        condition_policy::{ConditionStore, task_condition},
        conversation::{
            ConversationPolicies, cross_agent_comment, open_question_ids, queued_task_edits,
            queued_user_input, run_handoff, unanswered_question, unread_comments,
        },
    },
};
use std::{
    cell::Cell,
    collections::HashMap,
    future::Future,
    pin::pin,
    sync::Arc,
    task::{Context, Poll, Wake},
};

fn field<'a>(v: &'a Json, key: &str) -> &'a Json {
    static NULL: Json = Json::Null;
    v.get(key).unwrap_or(&NULL)
}
fn items(v: &Json) -> &[Json] {
    v.as_array().unwrap_or(&[])
}
fn string<'a>(v: &'a Json, key: &str) -> &'a JsString {
    v.required(key).unwrap().as_js_string().unwrap()
}
fn is(v: &Json, key: &str, expected: &str) -> bool {
    v.get(key) == Some(&expected.into())
}
fn fixture() -> Json {
    parse(
        include_str!("task-board-conversation.json"),
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
fn ids(values: Vec<JsString>) -> Json {
    Json::Array(values.into_iter().map(Json::String).collect())
}

#[test]
fn current_conversation_projections_match_original() {
    let fixture = fixture();
    let mut count = 0;
    for case in items(field(&fixture, "cases"))
        .iter()
        .filter(|c| !is(c, "kind", "bridge"))
    {
        let name = string(case, "name").to_utf8().unwrap();
        let input = field(case, "input");
        let before = input.clone();
        let calls = Cell::new(0);
        let resolver = |task: &Json, run: Option<&JsString>| -> IvyResult<Vec<Json>> {
            assert_eq!(task, field(input, "task"));
            calls.set(calls.get() + 1);
            let view = if let Some(run) = run {
                assert_eq!(run, string(input, "runId"));
                "run"
            } else {
                "all"
            };
            Ok(items(field(field(case, "resolverViews"), view)).to_vec())
        };
        let result = if is(case, "kind", "cross") {
            cross_agent_comment(field(input, "comment"), string(input, "taskId")).into()
        } else if is(case, "kind", "unread") {
            unread_comments(
                field(input, "task"),
                string(input, "taskId"),
                input.get("state"),
            )
            .unwrap()
        } else if is(case, "kind", "handoff") {
            run_handoff(field(input, "task"), string(input, "runId")).unwrap_or(Json::Null)
        } else if is(case, "kind", "queuedUser") {
            queued_user_input(field(input, "task")).into()
        } else if is(case, "kind", "queuedEdits") {
            ids(queued_task_edits(field(input, "task")))
        } else {
            assert!(is(case, "kind", "questions"));
            Json::object([
                (
                    "openIds",
                    ids(open_question_ids(&resolver, field(input, "task")).unwrap()),
                ),
                (
                    "unanswered",
                    unanswered_question(&resolver, field(input, "task"), string(input, "runId"))
                        .unwrap()
                        .into(),
                ),
            ])
        };
        same(&result, field(case, "output"), &name);
        assert_eq!(
            calls.get(),
            if is(case, "kind", "questions") { 2 } else { 0 },
            "{name}"
        );
        same(input, &before, &format!("{name} unchanged input"));
        count += 1;
    }
    assert_eq!(count, 65);
}

struct NoRead;
impl NextStepStore for NoRead {
    async fn read(&self, _: &str, _: &Json, _: bool) -> IvyResult<Json> {
        Err(IvyError::new(
            "unexpected_fixture_read",
            "Unexpected foreign/full-record read.",
        ))
    }
}
impl ConditionStore for NoRead {
    fn failed_handoff(&self, _: &JsString) -> IvyResult<Option<Json>> {
        Err(IvyError::new(
            "unexpected_fixture_handoff",
            "Unexpected retained handoff read.",
        ))
    }
}
struct NoWake;
impl Wake for NoWake {
    fn wake(self: Arc<Self>) {}
}
fn complete<T>(future: impl Future<Output = T>) -> T {
    let waker = Arc::new(NoWake).into();
    let mut future = pin!(future);
    for _ in 0..64 {
        if let Poll::Ready(value) = future.as_mut().poll(&mut Context::from_waker(&waker)) {
            return value;
        }
    }
    panic!("Finite conversation projection unexpectedly blocked");
}

#[test]
fn current_question_views_compose_with_owned_policies() {
    let fixture = fixture();
    let mut count = 0;
    for case in items(field(&fixture, "cases"))
        .iter()
        .filter(|c| is(c, "kind", "bridge"))
    {
        let name = string(case, "name").to_utf8().unwrap();
        let input = field(case, "input");
        let task = field(input, "task");
        let before = input.clone();
        let calls = Cell::new(0);
        let clocks = Cell::new(0);
        let conditions = Cell::new(0);
        let resolver = |value: &Json, run: Option<&JsString>| -> IvyResult<Vec<Json>> {
            assert_eq!(value, task);
            assert!(run.is_none());
            calls.set(calls.get() + 1);
            Ok(items(field(field(case, "resolverViews"), "all")).to_vec())
        };
        let validate =
            |_: &Json| -> IvyResult<()> { panic!("Read composition cannot validate a write") };
        let now = || {
            clocks.set(clocks.get() + 1);
            string(&fixture, "at").clone()
        };
        let condition = |value: &Json, steps: Option<&Json>| {
            assert_eq!(value, task);
            conditions.set(conditions.get() + 1);
            task_condition(&NoRead, value, steps)
        };
        let policies = ConversationPolicies {
            questions: &resolver,
            validate_next_step: &validate,
            now: &now,
            condition: &condition,
        };
        let summary = comment_summary(&policies, task, string(input, "taskId")).unwrap();
        let assessments = complete(assess_steps(
            &NoRead,
            &policies,
            task,
            &policies.now(),
            &HashMap::new(),
            None,
        ))
        .unwrap();
        let steps = step_condition(&assessments);
        let result = Json::object([
            ("summary", summary),
            (
                "assessments",
                Json::Array(assessments.iter().map(|a| a.as_json()).collect()),
            ),
            (
                "condition",
                policies.execution_condition(task, Some(&steps)).unwrap(),
            ),
        ]);
        same(&result, field(case, "output"), &name);
        assert_eq!(
            (calls.get(), clocks.get(), conditions.get()),
            (2, 1, 1),
            "{name}"
        );
        same(input, &before, &format!("{name} unchanged input"));
        count += 1;
    }
    assert_eq!(count, 5);
}

fn error_value(e: IvyError) -> Json {
    Json::object([
        ("code", e.code.into()),
        ("message", e.message.into()),
        ("outcome", e.outcome.as_str().into()),
        ("details", e.details.unwrap_or(Json::Null)),
    ])
}
#[test]
fn supplied_question_and_policy_failures_remain_exact() {
    let task = Json::object([("comments", Json::Array(vec![]))]);
    let run = JsString(vec![114, 117, 110, 45, 0xd800]);
    let mut sentinel = IvyError::new(
        "question_source_unavailable",
        "Original source unavailable.",
    );
    sentinel.outcome = Outcome::Unknown;
    sentinel.details = Some(Json::object([("runId", Json::String(run.clone()))]));
    let expected = error_value(sentinel.clone());
    let resolver = |value: &Json, _: Option<&JsString>| -> IvyResult<Vec<Json>> {
        assert_eq!(value, &task);
        Err(sentinel.clone())
    };
    let validate = |value: &Json| -> IvyResult<()> {
        assert_eq!(value, &task);
        Err(sentinel.clone())
    };
    let now = || run.clone();
    let condition = |value: &Json, steps: Option<&Json>| -> IvyResult<Json> {
        assert_eq!(value, &task);
        assert_eq!(steps, Some(&task));
        Err(sentinel.clone())
    };
    let policies = ConversationPolicies {
        questions: &resolver,
        validate_next_step: &validate,
        now: &now,
        condition: &condition,
    };
    let failures = [
        open_question_ids(&resolver, &task).map(|_| ()).unwrap_err(),
        unanswered_question(&resolver, &task, &run)
            .map(|_| ())
            .unwrap_err(),
        policies.open_questions(&task).map(|_| ()).unwrap_err(),
        comment_summary(&policies, &task, &run)
            .map(|_| ())
            .unwrap_err(),
        complete(assess_steps(
            &NoRead,
            &policies,
            &task,
            &run,
            &HashMap::new(),
            None,
        ))
        .map(|_| ())
        .unwrap_err(),
        policies.validate_next_step(&task).unwrap_err(),
        policies
            .execution_condition(&task, Some(&task))
            .unwrap_err(),
    ];
    for error in failures {
        same(&error_value(error), &expected, "supplied error identity");
    }
    assert_eq!(policies.now(), run);
}
