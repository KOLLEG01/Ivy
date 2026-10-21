//! Current pure execution-condition policy and the Task-to-facts projection.
//! The actual failed-handoff reader remains an explicit composition boundary.
use ivy_contracts::{IvyError, IvyJson as Json, IvyResult, JsString};

/// Supplies the exact current Store.failedHandoff result, including its already
/// selected code/detail. No default lookup or legacy full-reader adapter exists.
pub trait ConditionStore<E = IvyError> {
    fn failed_handoff(&self, run_id: &JsString) -> Result<Option<Json>, E>;
}
fn field<'a>(value: &'a Json, key: &str) -> &'a Json {
    static NULL: Json = Json::Null;
    value.get(key).unwrap_or(&NULL)
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
fn text<'a>(value: &'a Json, key: &str) -> IvyResult<&'a JsString> {
    value
        .required(key)?
        .as_js_string()
        .ok_or_else(IvyError::internal)
}
fn concat(parts: impl IntoIterator<Item = JsString>) -> Json {
    Json::String(JsString(parts.into_iter().flat_map(|s| s.0).collect()))
}
fn condition(state: &str, detail: impl Into<Json>) -> Json {
    Json::object([("state", state.into()), ("detail", detail.into())])
}

pub fn claimed_workflow_state(runtime_wait: &Json) -> JsString {
    if is(runtime_wait, "reason", "workflow") {
        "waiting".into()
    } else {
        "in_progress".into()
    }
}

/// Accepts the current ExecutionFacts projection, not a legacy Task document.
/// Ingress contract validation and recovery scheduling are separate bindings.
pub fn execution_condition(facts: &Json) -> IvyResult<Json> {
    if truthy(field(facts, "publication")) {
        return Ok(condition(
            "publishing",
            "Publishing the current task update.",
        ));
    }
    let claim = field(facts, "claim");
    if truthy(claim) {
        if is(claim, "mode", "recovery") {
            return Ok(condition(
                "recovering",
                "Restoring the original completion intent.",
            ));
        }
        if is(claim, "phase", "waiting_input") {
            return Ok(condition(
                "needs_user",
                "The native execution needs an owner-bound answer.",
            ));
        }
        if ["outcome_unknown", "cancel_requested"]
            .iter()
            .any(|p| is(claim, "phase", p))
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
            .any(|p| is(claim, "phase", p))
        {
            "starting"
        } else {
            "active"
        };
        return Ok(condition(
            state,
            concat([
                if is(claim, "mode", "conversation") {
                    "Processing new comments on "
                } else {
                    "Working on "
                }
                .into(),
                text(claim, "hostId")?.clone(),
                ".".into(),
            ]),
        ));
    }
    if ["backlog", "done", "cancelled"]
        .iter()
        .any(|s| is(facts, "workflowState", s))
    {
        return Ok(condition("idle", "No task work is scheduled."));
    }
    let recovery = field(facts, "recovery");
    if truthy(recovery) {
        return Ok(condition(
            if field(recovery, "attempts")
                .as_f64()
                .is_some_and(|n| n < 2.0)
            {
                "recovering"
            } else {
                "failed"
            },
            facts
                .get("completionIssue")
                .filter(|v| !v.is_null())
                .cloned()
                .unwrap_or_else(|| "The original completion intent needs reconciliation.".into()),
        ));
    }
    let issue = field(facts, "completionIssue");
    if truthy(issue) {
        return Ok(condition("failed", issue.clone()));
    }
    let waiting = field(facts, "runtimeWait");
    if truthy(waiting) {
        return Ok(condition(
            if is(waiting, "reason", "external_outcome") {
                "recovering"
            } else if is(waiting, "reason", "workflow") {
                "workflow_attention"
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
    if truthy(field(facts, "steps")) {
        return Ok(field(facts, "steps").clone());
    }
    Ok(if is(facts, "control", "user") {
        condition("needs_user", "Ready for manual work.")
    } else {
        condition("queued", "Waiting for an authorized next step.")
    })
}

fn execution_mode(request: &Json) -> &'static str {
    if is(request, "action", "continue") && truthy(field(request, "recoveryFor")) {
        "recovery"
    } else if truthy(field(request, "coordinationOnly")) {
        "conversation"
    } else {
        "execution"
    }
}

/// Matches the current Engine.condition caller, including its eager retained
/// handoff lookup. This supplies the condition slot of CommentReadPolicies;
/// current persistent-store/resolver/clock wiring is deliberately not installed.
pub fn task_condition<E: From<IvyError>, S: ConditionStore<E>>(
    store: &S,
    task: &Json,
    steps: Option<&Json>,
) -> Result<Json, E> {
    let issue = field(task, "completionIssue");
    let original = if truthy(issue) {
        store
            .failed_handoff(text(field(issue, "run"), "objectId")?)?
            .filter(truthy)
    } else {
        None
    };
    let fields = task.required("fields")?;
    let claim = field(task, "claim");
    let claim = if truthy(claim) {
        Json::object([
            ("phase", claim.required("phase")?.clone()),
            ("hostId", field(claim, "target").required("hostId")?.clone()),
            ("mode", execution_mode(field(claim, "request")).into()),
        ])
    } else {
        Json::Null
    };
    let completion_issue = match original {
        Some(value) => concat([
            text(&value, "code")?.clone(),
            ": ".into(),
            text(&value, "detail")?.clone(),
        ]),
        None => field(issue, "detail").clone(),
    };
    let mut facts = Json::object([
        ("workflowState", task.required("workflowState")?.clone()),
        ("control", fields.required("control")?.clone()),
        ("publication", truthy(field(task, "publication")).into()),
        ("claim", claim),
        ("runtimeWait", field(task, "runtimeWait").clone()),
        (
            "executionRequirement",
            field(fields, "executionRequirement").clone(),
        ),
        ("completionIssue", completion_issue),
    ]);
    if let Some(value) = fields.get("requiredCapabilities") {
        facts.insert("requiredCapabilities", value.clone());
    }
    if let Some(value) = steps {
        facts.insert("steps", value.clone());
    }
    if is(issue, "code", "task_board_handoff_missing")
        && is(fields, "control", "agent")
        && truthy(field(task, "primaryResourceRef"))
    {
        facts.insert(
            "recovery",
            Json::object([
                (
                    "attempts",
                    issue
                        .get("attempts")
                        .filter(|v| !v.is_null())
                        .cloned()
                        .unwrap_or(Json::Number(0.0)),
                ),
                ("retryAt", field(issue, "retryAt").clone()),
            ]),
        );
    }
    execution_condition(&facts).map_err(E::from)
}
