//! Current execution-condition precedence over typed JSON facts.
use ivy_contracts::{IvyError, IvyJson as Json, IvyResult, JsString};
pub trait ConditionStore<E = IvyError> {
    fn failed_handoff(&self, run_id: &JsString) -> Result<Option<Json>, E>;
}
fn f<'a>(value: &'a Json, key: &str) -> &'a Json {
    static NULL: Json = Json::Null;
    value.get(key).unwrap_or(&NULL)
}
fn is(value: &Json, key: &str, text: &str) -> bool {
    value.get(key) == Some(&text.into())
}
fn condition(state: &str, detail: impl Into<Json>) -> Json {
    Json::object([("state", state.into()), ("detail", detail.into())])
}
pub fn claimed_workflow_state(_runtime_wait: &Json) -> JsString {
    "in_progress".into()
}
pub fn execution_condition(facts: &Json) -> IvyResult<Json> {
    if f(facts, "publication").as_bool() == Some(true) {
        return Ok(condition(
            "publishing",
            "Publishing the current task update.",
        ));
    }
    let claim = f(facts, "claim");
    if !claim.is_null() {
        if is(claim, "phase", "waiting_input") {
            return Ok(condition(
                "needs_user",
                "The native execution needs an owner-bound answer.",
            ));
        }
        if ["outcome_unknown", "cancel_requested"]
            .iter()
            .any(|phase| is(claim, "phase", phase))
        {
            return Ok(condition(
                "recovering",
                "Reconciling the original native execution.",
            ));
        }
        let state = if is(claim, "phase", "publishing_result") {
            "publishing"
        } else if ["claimed", "run_created", "starting"]
            .iter()
            .any(|phase| is(claim, "phase", phase))
        {
            "starting"
        } else {
            "active"
        };
        let host = claim
            .required("hostId")?
            .as_js_string()
            .ok_or_else(IvyError::internal)?;
        let mut detail = JsString::from("Working on ");
        detail.0.extend(&host.0);
        detail.0.push(46);
        return Ok(condition(state, Json::String(detail)));
    }
    if ["backlog", "done", "cancelled"]
        .iter()
        .any(|state| is(facts, "workflowState", state))
    {
        return Ok(condition("idle", "No task work is scheduled."));
    }
    if let Some(issue) = f(facts, "completionIssue")
        .as_js_string()
        .filter(|s| !s.0.is_empty())
    {
        return Ok(condition("failed", Json::String(issue.clone())));
    }
    let waiting = f(facts, "runtimeWait");
    if !waiting.is_null() {
        return Ok(condition(
            if is(waiting, "reason", "external_outcome") {
                "recovering"
            } else {
                "blocked_environment"
            },
            waiting.required("detail")?.clone(),
        ));
    }
    if is(facts, "workflowState", "review") {
        return Ok(condition(
            "needs_user",
            "The final result is ready for review.",
        ));
    }
    if !f(facts, "wake").is_null() {
        return Ok(f(facts, "wake").clone());
    }
    Ok(if is(facts, "control", "user") {
        condition("needs_user", "Ready for manual work.")
    } else {
        condition("queued", "Waiting for the agent to continue.")
    })
}
/// Project an already admitted Task without loading its conversation or peers.
/// The caller supplies the original retained failed-handoff evidence, if any.
pub fn task_condition_snapshot(
    task: &Json,
    wake: Option<&Json>,
    original: Option<&Json>,
) -> IvyResult<Json> {
    let claim = f(task, "claim");
    let claim = if claim.is_null() {
        Json::Null
    } else {
        Json::object([
            ("phase", claim.required("phase")?.clone()),
            (
                "hostId",
                claim.required("target")?.required("hostId")?.clone(),
            ),
        ])
    };
    let issue = if let Some(original) = original {
        let mut detail = original
            .required("code")?
            .as_js_string()
            .ok_or_else(IvyError::internal)?
            .clone();
        detail.0.extend(": ".encode_utf16());
        detail.0.extend(
            &original
                .required("detail")?
                .as_js_string()
                .ok_or_else(IvyError::internal)?
                .0,
        );
        Json::String(detail)
    } else {
        f(f(task, "completionIssue"), "detail").clone()
    };
    execution_condition(&Json::object([
        ("workflowState", task.required("workflowState")?.clone()),
        (
            "control",
            task.required("fields")?.required("control")?.clone(),
        ),
        ("publication", (!f(task, "publication").is_null()).into()),
        ("claim", claim),
        ("runtimeWait", f(task, "runtimeWait").clone()),
        ("completionIssue", issue),
        ("wake", wake.cloned().unwrap_or(Json::Null)),
    ]))
}
pub fn task_condition<E: From<IvyError>, S: ConditionStore<E>>(
    store: &S,
    task: &Json,
    wake: Option<&Json>,
) -> Result<Json, E> {
    let issue = f(task, "completionIssue");
    let original = if issue.is_null() {
        None
    } else {
        store.failed_handoff(
            issue
                .required("run")?
                .required("objectId")?
                .as_js_string()
                .ok_or_else(IvyError::internal)?,
        )?
    };
    task_condition_snapshot(task, wake, original.as_ref()).map_err(E::from)
}
