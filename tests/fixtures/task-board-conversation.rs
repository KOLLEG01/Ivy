use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString, Outcome,
    json::{canonical, parse},
    limits::MANAGEMENT_FRAME_BYTES,
};
use ivy_task_board::{
    engine_read::read_policy::QuestionPolicies,
    engine_read::{
        comment_reads::{CommentReadPolicies, comment_summary},
        conversation::{
            ConversationPolicies, cross_agent_comment, open_question_ids, queued_task_edits,
            queued_user_input, run_handoff, unanswered_question, unread_comments,
        },
    },
};
use std::cell::Cell;

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

#[test]
fn current_shared_resolver_drives_owned_question_projections() {
    let fixture = fixture();
    let condition = |_: &Json, _: Option<&Json>| -> IvyResult<Json> {
        panic!("Question reads cannot query execution state")
    };
    let policies = ConversationPolicies::with_current_questions(&condition);
    let mut count = 0;
    for case in items(field(&fixture, "cases"))
        .iter()
        .filter(|c| is(c, "kind", "questions"))
    {
        let input = field(case, "input");
        let before = input.clone();
        let task = field(input, "task");
        let run = string(input, "runId");
        let name = string(case, "name").to_utf8().unwrap();
        let views = field(case, "resolverViews");
        same(
            &Json::Array((policies.questions)(task, None).unwrap()),
            field(views, "all"),
            &format!("{name} full ordered comments"),
        );
        same(
            &Json::Array((policies.questions)(task, Some(run)).unwrap()),
            field(views, "run"),
            &format!("{name} native run view"),
        );
        let actual = Json::object([
            ("openIds", ids(policies.open_questions(task).unwrap())),
            (
                "unanswered",
                unanswered_question(policies.questions, task, run)
                    .unwrap()
                    .into(),
            ),
        ]);
        same(&actual, field(case, "output"), &name);
        same(input, &before, &format!("{name} unchanged input"));
        count += 1;
    }
    assert_eq!(count, 23);
}

#[test]
fn supplied_question_and_policy_failures_remain_exact() {
    let task = Json::object([("comments", Json::Array(vec![]))]);
    let run = JsString(vec![114, 117, 110, 45, 0xd800]);
    let mut sentinel = IvyError::new(
        "question_source_unavailable",
        "Original source unavailable.",
    );
    sentinel.outcome = Outcome::Unknown.into();
    sentinel.details = Some(Json::object([("runId", Json::String(run.clone()))]));
    let resolver = |value: &Json, _: Option<&JsString>| -> IvyResult<Vec<Json>> {
        assert_eq!(value, &task);
        Err(sentinel.clone())
    };
    let condition = |value: &Json, steps: Option<&Json>| -> IvyResult<Json> {
        assert_eq!(value, &task);
        assert_eq!(steps, Some(&task));
        Err(sentinel.clone())
    };
    let policies = ConversationPolicies {
        questions: &resolver,
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
        policies
            .execution_condition(&task, Some(&task))
            .unwrap_err(),
    ];
    for error in failures {
        assert_eq!(error.code, sentinel.code);
        assert_eq!(error.message_js(), sentinel.message_js());
        assert_eq!(error.outcome.to_json(), sentinel.outcome.to_json());
        assert_eq!(error.details, sentinel.details);
    }
}
