use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString, Outcome,
    json::{canonical, parse},
    limits::MANAGEMENT_FRAME_BYTES,
};
use ivy_task_board::{
    coordination::next_steps::{NextStepPolicies, NextStepStore},
    engine_read::{
        comment_reads::{CommentReadPolicies, CommentReadStore, read_task},
        condition_policy::{
            ConditionStore, claimed_workflow_state, execution_condition, task_condition,
        },
    },
};
use std::{
    cell::{Cell, RefCell},
    future::Future,
    pin::pin,
    sync::Arc,
    task::{Context, Poll, Wake},
};

fn fixture() -> Json {
    parse(
        include_str!("task-board-condition.json"),
        MANAGEMENT_FRAME_BYTES,
    )
    .unwrap()
}
fn field<'a>(value: &'a Json, key: &str) -> &'a Json {
    static NULL: Json = Json::Null;
    value.get(key).unwrap_or(&NULL)
}
fn items(value: &Json) -> &[Json] {
    value.as_array().unwrap_or(&[])
}
fn is(value: &Json, key: &str, expected: &str) -> bool {
    value.get(key) == Some(&expected.into())
}
fn same(a: &Json, b: &Json, name: &str) {
    assert_eq!(
        canonical(a, MANAGEMENT_FRAME_BYTES).unwrap(),
        canonical(b, MANAGEMENT_FRAME_BYTES).unwrap(),
        "{name}"
    );
}
fn error(error: IvyError) -> Json {
    let mut v = Json::object([
        ("code", error.code.into()),
        ("message", error.message.into()),
        ("outcome", error.outcome.as_str().into()),
    ]);
    if let Some(details) = error.details {
        v.insert("details", details);
    }
    Json::object([("error", v)])
}
struct Memory {
    input: Json,
    trace: RefCell<Vec<Json>>,
    clocks: Cell<usize>,
}
impl Memory {
    fn new(input: &Json) -> Self {
        Self {
            input: input.clone(),
            trace: RefCell::default(),
            clocks: Cell::new(0),
        }
    }
}
impl ConditionStore for Memory {
    fn failed_handoff(&self, run_id: &JsString) -> IvyResult<Option<Json>> {
        self.trace.borrow_mut().push(Json::object([
            ("method", "failedHandoff".into()),
            ("runId", Json::String(run_id.clone())),
        ]));
        if self.input.get("failure") == Some(&true.into()) {
            let mut error = IvyError::new(
                "handoff_unavailable",
                "Original retained handoff unavailable.",
            );
            error.outcome = Outcome::Unknown;
            error.details = Some(Json::object([("runId", Json::String(run_id.clone()))]));
            return Err(error);
        }
        Ok(self
            .input
            .get("failedHandoff")
            .filter(|v| !v.is_null())
            .cloned())
    }
}
impl NextStepStore for Memory {
    async fn read(&self, _: &str, _: &Json, _: bool) -> IvyResult<Json> {
        Err(IvyError::new(
            "unexpected_fixture_read",
            "Unexpected foreign or full-record read.",
        ))
    }
}
impl CommentReadStore for Memory {
    fn comment_summary(&self, task: &Json, tree: &Json, id: &JsString) -> IvyResult<Option<Json>> {
        assert_eq!(task, field(field(&self.input, "header"), "value"));
        assert_eq!(tree, field(task, "commentStore"));
        assert_eq!(id, &JsString::from("self"));
        self.trace
            .borrow_mut()
            .push(Json::object([("method", "summary".into())]));
        Ok(Some(self.input.required("summary")?.clone()))
    }
    async fn comment_range(
        &self,
        tree: &Json,
        id: &JsString,
        start: usize,
        end: usize,
    ) -> IvyResult<Vec<Json>> {
        assert_eq!(
            tree,
            field(field(field(&self.input, "header"), "value"), "commentStore")
        );
        assert_eq!(id, &JsString::from("self"));
        self.trace.borrow_mut().push(Json::object([
            ("method", "range".into()),
            ("tree", tree.clone()),
            ("id", Json::String(id.clone())),
            ("start", Json::Number(start as f64)),
            ("end", Json::Number(end as f64)),
        ]));
        Ok(items(field(&self.input, "entries"))[start..end].to_vec())
    }
}
struct ReadPolicy<'a> {
    memory: &'a Memory,
    at: &'a JsString,
}
impl NextStepPolicies for ReadPolicy<'_> {
    fn open_questions(&self, _: &Json) -> IvyResult<Vec<JsString>> {
        Err(IvyError::new(
            "unexpected_fixture_resolver",
            "Indexed reads must use their current QuestionIndex.",
        ))
    }
    fn validate_next_step(&self, _: &Json) -> IvyResult<()> {
        Err(IvyError::new(
            "unexpected_fixture_validation",
            "Read policy must not validate a write.",
        ))
    }
}
impl CommentReadPolicies for ReadPolicy<'_> {
    fn now(&self) -> JsString {
        self.memory.clocks.set(self.memory.clocks.get() + 1);
        self.at.clone()
    }
    fn execution_condition(&self, task: &Json, steps: Option<&Json>) -> IvyResult<Json> {
        task_condition(self.memory, task, steps)
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
        if let Poll::Ready(v) = future.as_mut().poll(&mut Context::from_waker(&waker)) {
            return v;
        }
    }
    panic!("Finite condition projection unexpectedly blocked");
}

#[test]
fn current_condition_and_caller_match_original() {
    let fixture = fixture();
    assert_eq!(items(field(&fixture, "cases")).len(), 91);
    let mut count = 0;
    for case in items(field(&fixture, "cases"))
        .iter()
        .filter(|c| !is(c, "kind", "read"))
    {
        let name = field(case, "name").as_str().unwrap();
        let input = field(case, "input");
        let saved = input.clone();
        let memory = Memory::new(input);
        let result = if is(case, "kind", "facts") {
            execution_condition(input)
        } else if is(case, "kind", "workflow") {
            Ok(Json::String(claimed_workflow_state(input)))
        } else {
            task_condition(&memory, field(input, "task"), input.get("steps"))
        }
        .unwrap_or_else(error);
        same(&result, field(case, "output"), &name);
        same(
            &Json::Array(memory.trace.borrow().clone()),
            field(case, "trace"),
            &format!("{name} eager store calls"),
        );
        assert_eq!(field(case, "clockCalls").as_f64(), Some(0.0));
        same(input, &saved, &format!("{name} input mutation"));
        count += 1;
    }
    assert_eq!(count, 87);
}

#[test]
fn current_condition_composes_with_bounded_reads() {
    let fixture = fixture();
    let at = field(&fixture, "at").as_js_string().unwrap();
    let mut count = 0;
    for case in items(field(&fixture, "cases"))
        .iter()
        .filter(|c| is(c, "kind", "read"))
    {
        let name = field(case, "name").as_str().unwrap();
        let input = field(case, "input");
        let saved = input.clone();
        let memory = Memory::new(input);
        let policy = ReadPolicy {
            memory: &memory,
            at,
        };
        let query = input
            .get("query")
            .cloned()
            .unwrap_or_else(|| Json::object([] as [(&str, Json); 0]));
        let result = complete(read_task(&memory, &policy, field(input, "header"), &query))
            .unwrap_or_else(error);
        same(&result, field(case, "output"), &name);
        same(
            &Json::Array(memory.trace.borrow().clone()),
            field(case, "trace"),
            &format!("{name} exact store calls"),
        );
        assert_eq!(
            memory.clocks.get() as f64,
            field(case, "clockCalls").as_f64().unwrap(),
            "{name} clock"
        );
        same(input, &saved, &format!("{name} input mutation"));
        count += 1;
    }
    assert_eq!(count, 4);
}
