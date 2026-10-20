use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString, Outcome,
    hash::hash_json,
    json::{canonical, parse},
    limits::MANAGEMENT_FRAME_BYTES,
};
use ivy_task_board::{
    coordination::next_steps::{NextStepPolicies, NextStepStore},
    engine_read::comment_reads::*,
};
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    future::Future,
    pin::pin,
    sync::Arc,
    task::{Context, Poll, Wake},
};

fn fixture() -> Json {
    parse(
        include_str!("task-board-comment-reads.json"),
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
fn truthy(value: &Json) -> bool {
    match value {
        Json::Null => false,
        Json::Bool(v) => *v,
        Json::Number(v) => *v != 0.0 && !v.is_nan(),
        Json::String(v) => !v.0.is_empty(),
        _ => true,
    }
}
fn empty() -> Json {
    Json::object([] as [(&str, Json); 0])
}
struct Policies {
    at: JsString,
    views: HashMap<String, Vec<JsString>>,
    now_calls: Cell<usize>,
    condition_calls: RefCell<Vec<Json>>,
    resolver_calls: Cell<usize>,
    condition: Json,
}
impl Policies {
    fn new(fixture: &Json, input: &Json) -> Self {
        let views = items(field(fixture, "questionViews"))
            .iter()
            .map(|view| {
                (
                    field(view, "key").as_str().unwrap(),
                    items(field(view, "open"))
                        .iter()
                        .map(|id| id.as_js_string().unwrap().clone())
                        .collect(),
                )
            })
            .collect();
        Self {
            at: field(fixture, "at").as_js_string().unwrap().clone(),
            views,
            now_calls: Cell::new(0),
            condition_calls: RefCell::default(),
            resolver_calls: Cell::new(0),
            condition: field(input, "condition").clone(),
        }
    }
}
impl NextStepPolicies for Policies {
    fn open_questions(&self, task: &Json) -> IvyResult<Vec<JsString>> {
        self.resolver_calls.set(self.resolver_calls.get() + 1);
        // Exact frozen outputs of the unchanged current shared resolver.
        self.views
            .get(&hash_json(field(task, "comments"))?)
            .cloned()
            .ok_or_else(|| {
                IvyError::new(
                    "fixture_missing_question_view",
                    "Unexpected conversation resolver input.",
                )
            })
    }
    fn validate_next_step(&self, _: &Json) -> IvyResult<()> {
        Err(IvyError::new(
            "fixture_unexpected_write_validation",
            "A read projection must not validate a write.",
        ))
    }
}
impl CommentReadPolicies for Policies {
    fn now(&self) -> JsString {
        self.now_calls.set(self.now_calls.get() + 1);
        self.at.clone()
    }
    fn execution_condition(&self, task: &Json, steps: Option<&Json>) -> IvyResult<Json> {
        self.condition_calls.borrow_mut().push(Json::object([
            ("taskHash", hash_json(task)?.into()),
            ("steps", steps.cloned().unwrap_or(Json::Null)),
        ]));
        Ok(if !self.condition.is_null() {
            self.condition.clone()
        } else {
            steps.cloned().unwrap_or_else(|| {
                Json::object([
                    ("state", "active".into()),
                    ("detail", "Fixture holds execution precedence.".into()),
                ])
            })
        })
    }
}
struct Memory {
    input: Json,
    trace: RefCell<Vec<Json>>,
    active: Cell<usize>,
    maximum: Cell<usize>,
    retained_headers: Cell<usize>,
}
impl Memory {
    fn new(input: &Json) -> Self {
        Self {
            input: input.clone(),
            trace: RefCell::default(),
            active: Cell::new(0),
            maximum: Cell::new(0),
            retained_headers: Cell::new(0),
        }
    }
}
fn unavailable(code: &str, message: &str, details: Json) -> IvyError {
    let mut error = IvyError::new(code, message);
    error.outcome = Outcome::Unknown;
    error.details = Some(details);
    error
}
impl NextStepStore for Memory {
    async fn read(&self, key: &str, address: &Json, retained: bool) -> IvyResult<Json> {
        self.trace.borrow_mut().push(Json::object([
            ("method", "read".into()),
            ("key", key.into()),
            ("id", address.clone()),
            (
                "flags",
                Json::Array(if retained {
                    vec![true.into(), true.into(), true.into()]
                } else {
                    vec![true.into(), true.into()]
                }),
            ),
        ]));
        if is(&self.input, "failure", "read") {
            return Err(unavailable(
                "reader_unavailable",
                "Original full read unavailable.",
                Json::object([("address", address.clone())]),
            ));
        }
        let id = address
            .as_js_string()
            .or_else(|| field(address, "objectId").as_js_string())
            .unwrap()
            .to_utf8()
            .unwrap();
        if key == "task-board/task" && address.as_js_string().is_none() && id == "self" {
            assert!(!retained);
            assert_eq!(address, field(field(&self.input, "header"), "pin"));
            return Ok(self.input.required("fullTask")?.clone());
        }
        if retained {
            self.retained_headers.set(self.retained_headers.get() + 1);
        }
        field(&self.input, "records")
            .get(&format!("{key}:{id}"))
            .cloned()
            .ok_or_else(|| IvyError::new("fixture_unexpected_read", "Unexpected read address."))
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
        Ok(self.input.get("summary").filter(|v| !v.is_null()).cloned())
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
        self.active.set(self.active.get() + 1);
        self.maximum.set(self.maximum.get().max(self.active.get()));
        if field(&self.input, "stalledStart").as_f64() == Some(start as f64) {
            return std::future::pending().await;
        }
        let mut yielded = false;
        std::future::poll_fn(|cx| {
            if yielded {
                Poll::Ready(())
            } else {
                yielded = true;
                cx.waker().wake_by_ref();
                Poll::Pending
            }
        })
        .await;
        self.active.set(self.active.get() - 1);
        if field(&self.input, "failureStart").as_f64() == Some(start as f64) {
            return Err(unavailable(
                "range_unavailable",
                "Original range unavailable.",
                Json::object([
                    ("start", Json::Number(start as f64)),
                    ("end", Json::Number(end as f64)),
                ]),
            ));
        }
        Ok(items(field(&self.input, "entries"))[start..end].to_vec())
    }
}
struct NoWake;
impl Wake for NoWake {
    fn wake(self: Arc<Self>) {}
}
fn complete<T>(future: impl Future<Output = T>) -> T {
    let waker = Arc::new(NoWake).into();
    let mut future = pin!(future);
    for _ in 0..256 {
        if let Poll::Ready(value) = future.as_mut().poll(&mut Context::from_waker(&waker)) {
            return value;
        }
    }
    panic!(
        "Finite projection reads did not complete; a stalled sibling must not hide an available error."
    );
}
fn error(error: IvyError) -> Json {
    let mut value = Json::object([
        ("code", error.code.into()),
        ("message", error.message.into()),
        ("outcome", error.outcome.as_str().into()),
    ]);
    if let Some(details) = error.details {
        value.insert("details", details);
    }
    Json::object([("error", value)])
}
fn same(a: &Json, b: &Json, context: &str) {
    assert_eq!(
        canonical(a, MANAGEMENT_FRAME_BYTES).unwrap(),
        canonical(b, MANAGEMENT_FRAME_BYTES).unwrap(),
        "{context}"
    );
}

#[test]
fn current_comment_reads_match_original_bounded_projections() {
    let fixture = fixture();
    assert_eq!(field(&fixture, "reusedCases").as_f64(), Some(7.0));
    assert_eq!(field(&fixture, "newCases").as_f64(), Some(38.0));
    for case in items(field(&fixture, "cases")) {
        let name = field(case, "name").as_str().unwrap();
        let input = field(case, "input");
        let saved = input.clone();
        let store = Memory::new(input);
        let policies = Policies::new(&fixture, input);
        let actual = if is(case, "kind", "read") {
            complete(read_task(
                &store,
                &policies,
                field(input, "header"),
                input.get("query").unwrap_or(&empty()),
            ))
        } else if is(case, "kind", "card") {
            task_card(
                &policies,
                field(input, "view"),
                input.get("summary").filter(|v| !v.is_null()),
                input
                    .get("total")
                    .and_then(Json::as_f64)
                    .map(|v| v as usize),
            )
        } else if is(case, "kind", "legacy") {
            task_view_page(
                &policies,
                field(input, "view"),
                input.get("query").unwrap_or(&empty()),
            )
        } else {
            comment_summary(
                &policies,
                field(field(input, "view"), "task"),
                field(field(input, "view"), "object")
                    .required("objectId")
                    .unwrap()
                    .as_js_string()
                    .unwrap(),
            )
        }
        .unwrap_or_else(error);
        if let Some(json) = case.get("outputJson") {
            assert_eq!(
                canonical(&actual, MANAGEMENT_FRAME_BYTES).unwrap(),
                json.as_str().unwrap(),
                "{name} lossless result"
            );
        } else {
            same(&actual, field(case, "output"), &name);
        }
        same(
            &Json::Array(store.trace.borrow().clone()),
            field(case, "trace"),
            &format!("{name} exact read trace"),
        );
        if let Some(controls) = case.get("controls") {
            same(
                &Json::Array(policies.condition_calls.borrow().clone()),
                field(controls, "conditions"),
                &format!("{name} condition inputs"),
            );
            assert_eq!(
                policies.now_calls.get() as f64,
                field(controls, "clockCalls").as_f64().unwrap(),
                "{name} clock"
            );
            assert_eq!(
                store.maximum.get() as f64,
                field(controls, "maximumRangeConcurrency").as_f64().unwrap(),
                "{name} batch concurrency"
            );
            assert!(
                store.maximum.get() <= 8,
                "{name} range reads exceed the original micro-batch"
            );
        }
        if is(case, "kind", "read") && truthy(field(input, "summary")) {
            assert_eq!(
                policies.resolver_calls.get(),
                0,
                "{name} indexed path hydrated the conversation"
            );
        }
        same(input, &saved, &format!("{name} caller input mutation"));
    }
}

#[test]
fn stable_sequence_cursor_survives_a_concurrent_append() {
    let fixture = fixture();
    let mut results = Vec::new();
    for name in [
        "sequence cursor before append",
        "sequence cursor after append",
    ] {
        let case = items(field(&fixture, "cases"))
            .iter()
            .find(|c| is(c, "name", name))
            .unwrap();
        let input = field(case, "input");
        let store = Memory::new(input);
        let policies = Policies::new(&fixture, input);
        results.push(
            complete(read_task(
                &store,
                &policies,
                field(input, "header"),
                field(input, "query"),
            ))
            .unwrap(),
        );
    }
    same(
        field(field(&results[0], "task"), "comments"),
        field(field(&results[1], "task"), "comments"),
        "stable comments after append",
    );
    same(
        field(field(&results[0], "commentPage"), "nextBefore"),
        field(field(&results[1], "commentPage"), "nextBefore"),
        "stable next cursor",
    );
    assert_ne!(
        field(&results[0], "object"),
        field(&results[1], "object"),
        "append must use a distinct immutable revision"
    );
}
