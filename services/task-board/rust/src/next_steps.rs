//! Current next-step policy over owner-supplied task headers and question views.
//! This layer performs no publication, native dispatch or durable admission.
use futures_util::{
    FutureExt,
    future::{LocalBoxFuture, Shared, join_all, poll_fn},
};
use ivy_contracts::{IvyError, IvyJson as Json, IvyResult, JsString, hash::hash_json};
use ivy_sdk::native_evidence::native_instant;
use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    future::Future,
    task::Poll,
};

/// Reads include archived objects and upgrade records, as in the workflow store.
/// Header reads must omit foreign conversations. Run reads retain the exact pin.
/// The current comment-tree store adapter is an explicit composition boundary;
/// the legacy Store cannot silently stand in for this header-capable reader.
#[allow(async_fn_in_trait)]
pub trait NextStepStore {
    async fn read(&self, key: &str, address: &Json, header_only: bool) -> IvyResult<Json>;
}

/// Bind the current question resolver and exact NextStep contract validator.
/// An indexed header assessment never calls the conversation resolver.
pub trait NextStepPolicies {
    fn open_questions(&self, task: &Json) -> IvyResult<Vec<JsString>>;
    fn validate_next_step(&self, step: &Json) -> IvyResult<()>;
}

#[derive(Clone, Debug)]
pub struct StepAssessment {
    pub step: Json,
    pub fingerprint: String,
    pub ready: bool,
    pub consumed: bool,
    pub pending: Vec<Json>,
    pub needs_user: Option<bool>,
}
impl StepAssessment {
    pub fn as_json(&self) -> Json {
        let mut result = Json::object([
            ("step", self.step.clone()),
            ("fingerprint", self.fingerprint.clone().into()),
            ("ready", self.ready.into()),
            ("consumed", self.consumed.into()),
            ("pending", Json::Array(self.pending.clone())),
        ]);
        if let Some(value) = self.needs_user {
            result.insert("needsUser", value.into());
        }
        result
    }
}
fn field<'a>(value: &'a Json, key: &str) -> &'a Json {
    static NULL: Json = Json::Null;
    value.get(key).unwrap_or(&NULL)
}
fn array(value: &Json) -> &[Json] {
    value.as_array().unwrap_or(&[])
}
fn string<'a>(value: &'a Json, key: &str) -> IvyResult<&'a JsString> {
    value
        .required(key)?
        .as_js_string()
        .ok_or_else(IvyError::internal)
}
fn is(value: &Json, key: &str, text: &str) -> bool {
    value.get(key) == Some(&text.into())
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
fn require(ok: bool, code: &str, message: &str) -> IvyResult<()> {
    if ok {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn question(condition: &Json) -> bool {
    is(condition, "kind", "user") || is(condition, "kind", "question")
}
fn dependency(condition: &Json) -> bool {
    is(condition, "kind", "task") || is(condition, "kind", "delivery")
}
type TaskReads<'a> = RefCell<HashMap<JsString, Shared<LocalBoxFuture<'a, IvyResult<Json>>>>>;
fn source<'a, S: NextStepStore>(
    store: &'a S,
    sources: &TaskReads<'a>,
    overrides: &HashMap<JsString, Json>,
    id: &JsString,
) -> LocalBoxFuture<'a, IvyResult<Json>> {
    if let Some(value) = overrides.get(id) {
        return futures_util::future::ready(Ok(value.clone())).boxed_local();
    }
    let mut sources = sources.borrow_mut();
    sources
        .entry(id.clone())
        .or_insert_with(|| {
            let address = Json::String(id.clone());
            async move { store.read("task-board/task", &address, true).await }
                .boxed_local()
                .shared()
        })
        .clone()
        .boxed_local()
}

pub async fn assess_steps<S: NextStepStore, P: NextStepPolicies>(
    store: &S,
    policies: &P,
    task: &Json,
    at: &JsString,
    overrides: &HashMap<JsString, Json>,
    index: Option<&[Json]>,
) -> IvyResult<Vec<StepAssessment>> {
    let open: HashSet<_> = if let Some(index) = index {
        index
            .iter()
            .filter(|q| truthy(field(q, "open")))
            .map(|q| string(q, "commentId").cloned())
            .collect::<IvyResult<_>>()?
    } else {
        policies.open_questions(task)?.into_iter().collect()
    };
    let own: Vec<_> = index
        .map(|index| index.iter().collect())
        .unwrap_or_else(|| {
            array(field(task, "comments"))
                .iter()
                .filter(|c| is(c, "purpose", "question") && !truthy(field(c, "sourceTaskId")))
                .collect()
        });
    let sources = TaskReads::default();
    let first_error = RefCell::new(None);
    let mut all = Box::pin(join_all(array(field(task, "nextSteps")).iter().map(
        |step| {
            let sources = &sources;
            let own = &own;
            let open = &open;
            let first_error = &first_error;
            async move {
                let result: IvyResult<StepAssessment> = async {
                    let mut pending = Vec::new();
                    let mut observations = Vec::new();
                    let mut outside_json = false;
                    for condition in array(field(step, "requires")) {
                        let (satisfied, observation) = if is(condition, "kind", "time") {
                            (string(condition, "at")? <= at, condition.clone())
                        } else if question(condition) {
                            let id = string(condition, "questionId")?;
                            require(
                                own.iter()
                                    .any(|q| field(q, "commentId").as_js_string() == Some(id)),
                                "task_board_comment_mismatch",
                                "A user condition must identify a question owned by this ticket.",
                            )?;
                            let resolved = !open.contains(id);
                            (
                                resolved,
                                Json::object([
                                    ("condition", condition.clone()),
                                    ("resolved", resolved.into()),
                                ]),
                            )
                        } else {
                            let other =
                                source(store, sources, overrides, string(condition, "taskId")?)
                                    .await?;
                            if is(condition, "kind", "delivery") {
                                let receipt = array(field(&other, "deliveries")).iter().find(|r| {
                                    field(field(r, "delivery"), "key") == field(condition, "key")
                                });
                                let identity = receipt.and_then(|r| r.get("identity"));
                                (
                                    receipt.is_some() && identity != condition.get("after"),
                                    Json::object([
                                        ("condition", condition.clone()),
                                        ("identity", identity.cloned().unwrap_or(Json::Null)),
                                    ]),
                                )
                            } else {
                                // The original reads the saved Run even for an until=done condition.
                                let running = if truthy(field(&other, "lastRun")) {
                                    let run = store
                                        .read("task-board/run", field(&other, "lastRun"), false)
                                        .await?;
                                    !["completed", "failed", "cancelled"]
                                        .iter()
                                        .any(|phase| is(&run, "phase", phase))
                                } else {
                                    false
                                };
                                let satisfied = if is(condition, "until", "done") {
                                    is(&other, "workflowState", "done")
                                        && !truthy(field(&other, "publication"))
                                } else {
                                    !truthy(field(&other, "claim"))
                                        && !truthy(field(&other, "publication"))
                                        && !running
                                };
                                let mut observation =
                                    Json::object([("condition", condition.clone())]);
                                // The canonical hash rejects undefined observation fields. Keep
                                // reading the remaining conditions before that hash boundary.
                                for (key, from) in [
                                    ("state", "workflowState"),
                                    ("result", "latestResult"),
                                    ("review", "acceptedReview"),
                                ] {
                                    if let Some(value) = other.get(from) {
                                        observation.insert(key, value.clone());
                                    } else {
                                        outside_json = true;
                                    }
                                }
                                (satisfied, observation)
                            }
                        };
                        observations.push(observation);
                        if !satisfied {
                            pending.push(condition.clone());
                        }
                    }
                    require(
                        !outside_json,
                        "invalid_arguments",
                        "Value is outside the JSON domain.",
                    )?;
                    let fingerprint = hash_json(&Json::object([
                        ("step", step.clone()),
                        ("observations", Json::Array(observations)),
                    ]))?;
                    let consumed = array(field(task, "consumedSteps")).iter().any(|value| {
                        field(value, "stepId") == field(step, "id")
                            && is(value, "fingerprint", &fingerprint)
                    });
                    let needs_user = pending.iter().any(|condition| {
                        question(condition)
                            && !own
                                .iter()
                                .find(|q| field(q, "commentId") == field(condition, "questionId"))
                                .is_some_and(|q| is(field(q, "recipient"), "kind", "task"))
                    });
                    Ok(StepAssessment {
                        step: step.clone(),
                        fingerprint,
                        ready: pending.is_empty() && !consumed,
                        consumed,
                        pending,
                        needs_user: Some(needs_user),
                    })
                }
                .await;
                if let Err(error) = &result {
                    let mut first = first_error.borrow_mut();
                    if first.is_none() {
                        *first = Some(error.clone());
                    }
                }
                result
            }
        },
    )));
    // Start all step reads, preserving shared in-flight headers. Return the first
    // completed error without waiting for another independent read to complete.
    poll_fn(|cx| {
        let result = all.as_mut().poll(cx);
        if let Some(error) = first_error.borrow_mut().take() {
            return Poll::Ready(Err(error));
        }
        result.map(|values| values.into_iter().collect())
    })
    .await
}

pub fn step_condition(assessments: &[StepAssessment]) -> Json {
    fn user(value: &StepAssessment) -> bool {
        value
            .needs_user
            .unwrap_or_else(|| value.pending.iter().any(question))
    }
    let condition = |state: &str, detail: String| {
        Json::object([("state", state.into()), ("detail", detail.into())])
    };
    if assessments.iter().any(|v| v.ready) {
        return condition("queued", "An authorized next step is ready.".into());
    }
    let pending: Vec<_> = assessments.iter().filter(|v| !v.consumed).collect();
    let automatic: Vec<_> = pending.iter().copied().filter(|v| !user(v)).collect();
    let timed: Vec<_> = automatic
        .iter()
        .filter(|v| v.pending.iter().all(|c| is(c, "kind", "time")))
        .collect();
    if !timed.is_empty() {
        let at = timed
            .iter()
            .filter_map(|v| {
                v.pending
                    .iter()
                    .filter_map(|c| field(c, "at").as_js_string())
                    .max()
            })
            .min();
        // An externally supplied empty-pending assessment has JS undefined here.
        let text = at
            .map(|v| String::from_utf16_lossy(&v.0))
            .unwrap_or_else(|| "undefined".into());
        return condition(
            "deferred",
            format!("Next authorized step is scheduled for {text}."),
        );
    }
    if automatic
        .iter()
        .any(|v| v.pending.iter().any(|c| !is(c, "kind", "time")))
    {
        return condition(
            "blocked_dependency",
            "Waiting for another ticket or its named delivery.".into(),
        );
    }
    if pending.iter().any(|v| user(v)) {
        return condition(
            "needs_user",
            "The remaining next steps require an answer to this ticket's own question.".into(),
        );
    }
    condition("workflow_attention", if assessments.is_empty() { "No next step is recorded." } else { "This unchanged next step has already been processed. Publish a new next step or delivery condition." }.into())
}

pub fn next_step_time(task: &Json) -> Option<JsString> {
    array(field(task, "nextSteps"))
        .iter()
        .flat_map(|step| array(field(step, "requires")))
        .filter(|c| is(c, "kind", "time"))
        .filter_map(|c| field(c, "at").as_js_string())
        .min()
        .cloned()
}

pub fn consume_step(task: &Json, selected: &StepAssessment) -> Vec<Json> {
    let mut values: Vec<_> = array(field(task, "consumedSteps"))
        .iter()
        .filter(|v| field(v, "stepId") != field(&selected.step, "id"))
        .cloned()
        .collect();
    values.push(Json::object([
        ("stepId", field(&selected.step, "id").clone()),
        ("fingerprint", selected.fingerprint.clone().into()),
    ]));
    values.drain(..values.len().saturating_sub(64));
    values
}

struct Validation<'a, S, P> {
    store: &'a S,
    policies: &'a P,
    own: &'a JsString,
    at: &'a JsString,
    sources: HashMap<JsString, Json>,
    assessments: HashMap<JsString, Vec<StepAssessment>>,
}
impl<S: NextStepStore, P: NextStepPolicies> Validation<'_, S, P> {
    async fn pending(&mut self, id: &JsString, task: &Json) -> IvyResult<Vec<StepAssessment>> {
        if !self.assessments.contains_key(id) {
            let values = assess_steps(
                self.store,
                self.policies,
                task,
                self.at,
                &self.sources,
                None,
            )
            .await?
            .into_iter()
            .filter(|v| !v.consumed)
            .collect();
            self.assessments.insert(id.clone(), values);
        }
        Ok(self.assessments[id].clone())
    }
    async fn closes_cycle(&mut self, id: &JsString) -> IvyResult<bool> {
        struct Frame {
            route: HashSet<JsString>,
            steps: Vec<Vec<JsString>>,
            step: usize,
            condition: usize,
            cyclic: bool,
        }
        let mut frames: Vec<Frame> = Vec::new();
        let mut next = Some((id.clone(), HashSet::new()));
        let mut result = None;
        loop {
            if let Some((id, mut route)) = next.take() {
                result = if &id == self.own {
                    Some(true)
                } else if route.contains(&id) {
                    Some(false)
                } else {
                    require(
                        self.sources.len() < 1024,
                        "task_board_dependency_limit",
                        "The next-step dependency graph exceeds its validation bound.",
                    )?;
                    if !self.sources.contains_key(&id) {
                        let other = self
                            .store
                            .read("task-board/task", &Json::String(id.clone()), false)
                            .await?;
                        self.sources.insert(id.clone(), other);
                    }
                    let other = self.sources[&id].clone();
                    if truthy(field(&other, "claim"))
                        || ["done", "cancelled", "review"]
                            .iter()
                            .any(|state| is(&other, "workflowState", state))
                        || array(field(&other, "nextSteps")).is_empty()
                    {
                        Some(false)
                    } else {
                        let remaining = self.pending(&id, &other).await?;
                        if remaining.is_empty() {
                            Some(false)
                        } else {
                            route.insert(id);
                            let steps = remaining
                                .iter()
                                .map(|step| {
                                    step.pending
                                        .iter()
                                        .filter(|c| dependency(c))
                                        .map(|c| string(c, "taskId").cloned())
                                        .collect()
                                })
                                .collect::<IvyResult<_>>()?;
                            frames.push(Frame {
                                route,
                                steps,
                                step: 0,
                                condition: 0,
                                cyclic: false,
                            });
                            None
                        }
                    }
                };
            }
            if let Some(value) = result.take() {
                if let Some(frame) = frames.last_mut() {
                    frame.cyclic |= value;
                } else {
                    return Ok(value);
                }
            }
            let frame = frames.last_mut().ok_or_else(IvyError::internal)?;
            if frame.step == frame.steps.len() {
                frames.pop();
                result = Some(true);
            } else if frame.condition == frame.steps[frame.step].len() {
                if !frame.cyclic {
                    frames.pop();
                    result = Some(false);
                } else {
                    frame.step += 1;
                    frame.condition = 0;
                    frame.cyclic = false;
                }
            } else {
                next = Some((
                    frame.steps[frame.step][frame.condition].clone(),
                    frame.route.clone(),
                ));
                frame.condition += 1;
            }
        }
    }
}

pub async fn validate_next_steps<S: NextStepStore, P: NextStepPolicies>(
    store: &S,
    policies: &P,
    own: Option<&JsString>,
    steps: &[Json],
    task: Option<&Json>,
    at: &JsString,
) -> IvyResult<()> {
    let mut identities = Vec::new();
    for step in steps {
        let id = step.get("id");
        require(
            !identities.contains(&id),
            "invalid_arguments",
            "Next-step identities must be unique.",
        )?;
        identities.push(id);
    }
    if let (Some(own), Some(task)) = (own.filter(|id| !id.0.is_empty()), task) {
        let mut value = task.clone();
        value.insert("nextSteps", Json::Array(steps.to_vec()));
        let mut validation = Validation {
            store,
            policies,
            own,
            at,
            sources: HashMap::from([(own.clone(), value.clone())]),
            assessments: HashMap::new(),
        };
        let remaining = validation.pending(own, &value).await?;
        let mut all_cyclic = !remaining.is_empty();
        for step in remaining {
            let mut cyclic = false;
            for condition in &step.pending {
                if dependency(condition)
                    && validation
                        .closes_cycle(string(condition, "taskId")?)
                        .await?
                {
                    cyclic = true;
                }
            }
            if !cyclic {
                all_cyclic = false;
            }
        }
        require(
            !all_cyclic,
            "task_board_dependency_cycle",
            "Every next step would wait indirectly on this ticket. Keep an independent progress path.",
        )?;
    }
    for step in steps {
        policies.validate_next_step(step)?;
        for condition in array(field(step, "requires")) {
            if is(condition, "kind", "time") {
                let value = string(condition, "at")?;
                let normalized = if value.contains_ascii(".") {
                    value.clone()
                } else {
                    value.to_utf8()?.replacen('Z', ".000Z", 1).into()
                };
                require(
                    native_instant(&normalized, None).is_ok(),
                    "task_board_invalid_date",
                    "A time condition needs a valid UTC timestamp.",
                )?;
            }
            if question(condition) {
                require(
                    task.is_some_and(|task| {
                        array(field(task, "comments")).iter().any(|c| {
                            field(c, "commentId") == field(condition, "questionId")
                                && is(c, "purpose", "question")
                                && !truthy(field(c, "sourceTaskId"))
                        })
                    }),
                    "task_board_comment_mismatch",
                    "A user condition must reference a question owned by this ticket.",
                )?;
            }
            if dependency(condition) {
                let id = string(condition, "taskId")?;
                require(
                    Some(id) != own,
                    "task_board_dependency_cycle",
                    "A task cannot wait on itself.",
                )?;
                store
                    .read("task-board/task", &Json::String(id.clone()), false)
                    .await?;
            }
        }
    }
    Ok(())
}
