use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString, Outcome,
    hash::hash_json,
    json::{canonical, parse},
    limits::MANAGEMENT_FRAME_BYTES,
    schema::{CompiledSchema, SchemaValidators},
};
use ivy_task_board::coordination::next_steps::*;
use serde_json::{Value, json};
use std::{
    cell::RefCell,
    collections::HashMap,
    future::Future,
    pin::pin,
    sync::Arc,
    task::{Context, Poll, Wake},
};

fn fixture() -> Value {
    serde_json::from_str(include_str!("task-board-next-steps.json")).unwrap()
}
fn convert(value: &Value) -> Json {
    Json::from_serde(value).unwrap()
}
fn field<'a>(value: &'a Json, key: &str) -> &'a Json {
    static NULL: Json = Json::Null;
    value.get(key).unwrap_or(&NULL)
}
fn items(value: &Json) -> &[Json] {
    value.as_array().unwrap_or(&[])
}
fn task(patch: Value) -> Value {
    let mut value = json!({"taskKey":"TASK-0001","workflowState":"todo","claim":null,"publication":null,"lastRun":null,"latestResult":null,"acceptedReview":null,"deliveries":[],"comments":[],"commentDeliveries":[],"nextSteps":[],"consumedSteps":[],"runtimeWait":null});
    for (k, v) in patch.as_object().unwrap() {
        value[k] = v.clone();
    }
    value
}
fn step(id: &str, requires: Value) -> Value {
    json!({"id":id,"scope":format!("Original bounded scope {id}"),"requires":requires})
}
struct Policies {
    validator: Arc<CompiledSchema>,
    views: HashMap<String, Vec<JsString>>,
    calls: RefCell<usize>,
}
impl Policies {
    fn new(fixture: &Value) -> Self {
        let definitions = ivy_task_board::schema::task_board_contracts().unwrap();
        let task = definitions
            .iter()
            .find(|d| {
                field(d, "key") == &Json::from("task-board/task")
                    && field(d, "version") == &Json::from("2.3.0")
            })
            .unwrap();
        let schema = Json::object([
            ("$ref", "#/$defs/TaskBoardNextStep".into()),
            (
                "$defs",
                task.required("jsonSchema")
                    .unwrap()
                    .required("$defs")
                    .unwrap()
                    .clone(),
            ),
        ]);
        let validator = SchemaValidators::new().compile(&schema).unwrap();
        let views = fixture["questionViews"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| {
                (
                    v["key"].as_str().unwrap().to_owned(),
                    v["open"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|id| id.as_str().unwrap().into())
                        .collect(),
                )
            })
            .collect();
        Self {
            validator,
            views,
            calls: RefCell::new(0),
        }
    }
}
impl NextStepPolicies for Policies {
    fn open_questions(&self, task: &Json) -> IvyResult<Vec<JsString>> {
        *self.calls.borrow_mut() += 1;
        // These are frozen outputs of the unchanged shared owner resolver, not
        // a replacement Rust resolver or an assumption that questions are closed.
        let key = hash_json(field(task, "comments"))?;
        Ok(self
            .views
            .get(&key)
            .expect("exact original question view")
            .clone())
    }
    fn validate_next_step(&self, step: &Json) -> IvyResult<()> {
        canonical(step, MANAGEMENT_FRAME_BYTES)?;
        if self.validator.matches(step)? {
            Ok(())
        } else {
            Err(IvyError::new(
                "invalid_arguments",
                "Value does not match its TaskBoard workflow contract.",
            ))
        }
    }
}
struct Memory {
    records: HashMap<String, Json>,
    trace: RefCell<Vec<Json>>,
    failure: Option<String>,
    stalled: Option<String>,
    yield_header: bool,
}
impl Memory {
    fn new(input: &Value) -> Self {
        let mut records: HashMap<_, _> = input["records"]
            .as_object()
            .map(|v| v.iter().map(|(k, v)| (k.clone(), convert(v))).collect())
            .unwrap_or_default();
        if let Some(n) = input["chainLength"].as_u64() {
            for i in 1..=n {
                let steps = if i == n {
                    json!([])
                } else {
                    json!([step(
                        "next",
                        json!([{"kind":"task","taskId":format!("n{}",i+1),"until":"done"}])
                    )])
                };
                records.insert(
                    format!("task-board/task:n{i}"),
                    convert(&task(json!({"nextSteps":steps}))),
                );
            }
        }
        Self {
            records,
            trace: RefCell::default(),
            failure: input["failure"].as_str().map(str::to_owned),
            stalled: input["stalled"].as_str().map(str::to_owned),
            yield_header: false,
        }
    }
}
impl NextStepStore for Memory {
    async fn read(&self, key: &str, address: &Json, header_only: bool) -> IvyResult<Json> {
        self.trace.borrow_mut().push(Json::object([
            ("method", "read".into()),
            ("key", key.into()),
            ("id", address.clone()),
            (
                "flags",
                Json::Array(if header_only {
                    vec![true.into(), true.into(), true.into()]
                } else {
                    vec![true.into(), true.into()]
                }),
            ),
        ]));
        let id = address
            .as_js_string()
            .or_else(|| field(address, "objectId").as_js_string())
            .unwrap()
            .to_utf8()
            .unwrap();
        if self.stalled.as_ref() == Some(&id) {
            return std::future::pending().await;
        }
        if self.failure.as_ref() == Some(&id) {
            let mut error = IvyError::new("owner_unavailable", "Original read unavailable.");
            error.outcome = Outcome::Unknown;
            error.details = Some(Json::object([("address", id.into())]));
            return Err(error);
        }
        if self.yield_header && header_only {
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
        }
        Ok(self
            .records
            .get(&format!("{key}:{id}"))
            .expect("exact original read address")
            .clone())
    }
}
struct WakeReady;
impl Wake for WakeReady {
    fn wake(self: Arc<Self>) {}
}
fn complete<T>(future: impl Future<Output = T>) -> T {
    let waker = Arc::new(WakeReady).into();
    let mut future = pin!(future);
    for _ in 0..8 {
        if let Poll::Ready(value) = future.as_mut().poll(&mut Context::from_waker(&waker)) {
            return value;
        }
    }
    panic!("finite fixture reads did not complete");
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
fn selected(value: &Json) -> StepAssessment {
    StepAssessment {
        step: field(value, "step").clone(),
        fingerprint: field(value, "fingerprint").as_str().unwrap(),
        ready: false,
        consumed: false,
        pending: vec![],
        needs_user: None,
    }
}

#[test]
fn current_next_steps_match_frozen_original_observations() {
    let fixture = fixture();
    let policies = Policies::new(&fixture);
    let at: JsString = fixture["at"].as_str().unwrap().into();
    assert_eq!(fixture["reusedCases"], 26);
    assert_eq!(fixture["newCases"], 30);
    for case in fixture["cases"].as_array().unwrap() {
        let input = &case["input"];
        let port = Memory::new(input);
        let task = convert(&input["task"]);
        let before = task.clone();
        let calls = *policies.calls.borrow();
        let actual: IvyResult<Json> = match case["kind"].as_str().unwrap() {
            "assessment" => {
                let overrides = input["overrides"]
                    .as_object()
                    .map(|v| {
                        v.iter()
                            .map(|(k, v)| (JsString::from(k.clone()), convert(v)))
                            .collect()
                    })
                    .unwrap_or_default();
                let index = convert(&input["index"]);
                complete(assess_steps(
                    &port,
                    &policies,
                    &task,
                    &at,
                    &overrides,
                    index.as_array(),
                ))
                .map(|values| {
                    Json::object([
                        (
                            "assessments",
                            Json::Array(values.iter().map(StepAssessment::as_json).collect()),
                        ),
                        ("condition", step_condition(&values)),
                    ])
                })
            }
            "validate" => {
                let own = input["own"].as_str().map(JsString::from);
                let steps = convert(&input["steps"]);
                complete(validate_next_steps(
                    &port,
                    &policies,
                    own.as_ref(),
                    items(&steps),
                    (!task.is_null()).then_some(&task),
                    &at,
                ))
                .map(|()| Json::Null)
            }
            "consume" => Ok(Json::Array(consume_step(
                &task,
                &selected(&convert(&input["selected"])),
            ))),
            "time" => Ok(next_step_time(&task)
                .map(Json::String)
                .unwrap_or(Json::Null)),
            kind => panic!("unknown case {kind}"),
        };
        let mut actual = actual.unwrap_or_else(error);
        // The older saved validation observations store an error directly;
        // the new capture wraps it. Preserve both original fixture formats.
        if case["reusedObservation"] == true && case["kind"] == "validate" && !actual.is_null() {
            actual = field(&actual, "error").clone();
        }
        assert_eq!(
            actual.to_serde().unwrap(),
            case["output"],
            "{}",
            case["name"]
        );
        let trace = Json::Array(port.trace.borrow().clone());
        if let Some(hash) = case["traceHash"].as_str() {
            assert_eq!(
                hash_json(&trace).unwrap(),
                hash,
                "{} trace hash",
                case["name"]
            );
            assert_eq!(
                items(&trace).len() as u64,
                case["traceCount"].as_u64().unwrap()
            );
        } else {
            assert_eq!(
                trace.to_serde().unwrap(),
                case["trace"],
                "{} read trace",
                case["name"]
            );
        }
        assert_eq!(task, before, "input mutation: {}", case["name"]);
        if !input["index"].is_null() {
            assert_eq!(
                *policies.calls.borrow(),
                calls,
                "indexed assessment must not hydrate comments"
            );
        }
    }
}

#[test]
fn shared_in_flight_headers_resume_without_duplicate_reads() {
    let fixture = fixture();
    let policies = Policies::new(&fixture);
    let input = json!({"task":task(json!({"nextSteps":[step("a",json!([{"kind":"task","taskId":"other","until":"idle"}])),step("b",json!([{"kind":"task","taskId":"other","until":"idle"}]))]})),"records":{"task-board/task:other":task(json!({}))}});
    let mut port = Memory::new(&input);
    port.yield_header = true;
    let values = complete(assess_steps(
        &port,
        &policies,
        &convert(&input["task"]),
        &JsString::from("2026-10-20T12:00:00.000Z"),
        &HashMap::new(),
        None,
    ))
    .unwrap();
    assert_eq!(port.trace.borrow().len(), 1);
    assert!(values.iter().all(|v| v.ready));
}

#[test]
fn fingerprint_retains_lossless_strings_and_condition_fallback() {
    let fixture = fixture();
    let raw = fixture["losslessReference"]["taskJson"].as_str().unwrap();
    let task = parse(raw, MANAGEMENT_FRAME_BYTES).unwrap();
    let policies = Policies::new(&fixture);
    let port = Memory::new(&json!({}));
    let values = complete(assess_steps(
        &port,
        &policies,
        &task,
        &JsString::from("2026-10-20T12:00:00.000Z"),
        &HashMap::new(),
        None,
    ))
    .unwrap();
    let mut assessment = values[0].clone();
    assessment.ready = false;
    assessment.needs_user = None;
    assessment.pending = vec![convert(&json!({"kind":"question","questionId":"q"}))];
    assert_eq!(
        step_condition(&[assessment]).get("state"),
        Some(&Json::from("needs_user"))
    );
    let consumed = consume_step(&task, &values[0]);
    assert_eq!(
        field(&consumed[0], "stepId").as_js_string(),
        Some(&JsString(vec![0xd800]))
    );
    assert_eq!(
        values[0].fingerprint,
        fixture["losslessReference"]["fingerprint"]
            .as_str()
            .unwrap()
    );
}
