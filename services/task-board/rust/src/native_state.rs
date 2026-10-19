//! Projection of an exact native observation. Terminal outcomes cannot regress.
use ivy_contracts::{IvyError, IvyJson as Json, IvyResult, JsString, json::try_js_string};
fn field<'a>(v: &'a Json, k: &str) -> &'a Json {
    static NULL: Json = Json::Null;
    v.get(k).unwrap_or(&NULL)
}
fn equal(v: &Json, k: &str, s: &str) -> bool {
    v.get(k) == Some(&Json::from(s))
}
fn truthy(v: &Json) -> bool {
    match v {
        Json::Null => false,
        Json::Bool(v) => *v,
        Json::String(v) => !v.0.is_empty(),
        Json::Number(v) => *v != 0.0 && !v.is_nan(),
        _ => true,
    }
}
pub fn finished_run(run: &Json) -> bool {
    ["completed", "failed", "cancelled"]
        .iter()
        .any(|v| equal(run, "phase", v))
}
pub fn project_native_state(
    run: &Json,
    status: &JsString,
    epoch: &Json,
    thread: &Json,
    at: &JsString,
    evidence: &Json,
) -> IvyResult<Json> {
    let prior = field(run, "externalOutcome");
    let state = if status == &JsString::from("completed") {
        "succeeded"
    } else if status == &JsString::from("failed") {
        "failed"
    } else if status == &JsString::from("interrupted") {
        if equal(prior, "state", "failed") {
            "failed"
        } else if truthy(field(run, "cancellation")) {
            "cancelled"
        } else {
            "failed"
        }
    } else {
        "active"
    };
    let code = if ["inProgress", "completed"]
        .iter()
        .any(|v| status == &JsString::from(*v))
    {
        Json::Null
    } else {
        let mut code = JsString::from("native_");
        code.0.extend_from_slice(&status.0);
        Json::String(code)
    };
    if ["succeeded", "failed", "cancelled"]
        .iter()
        .any(|v| equal(prior, "state", v))
        && (!equal(prior, "state", state) || prior.get("code") != Some(&code))
    {
        return Err(IvyError::new(
            "task_board_native_state_changed",
            "A native read cannot regress or reclassify an already known terminal outcome.",
        ));
    }
    let mut phase = "publishing_result";
    let mut waiting = Json::Null;
    let mut outcome = Json::object([
        ("state", state.into()),
        ("code", code),
        ("observedAt", Json::String(at.clone())),
        ("evidence", evidence.clone()),
    ]);
    let wait = |reason: &str, detail: &str| {
        Json::object([
            ("reason", reason.into()),
            ("detail", detail.into()),
            ("since", Json::String(at.clone())),
        ])
    };
    if status == &JsString::from("inProgress") {
        let native = field(thread, "status");
        if run.get("nativeEpoch") != Some(epoch) || !equal(native, "type", "active") {
            phase = if truthy(field(run, "cancellation")) {
                "cancel_requested"
            } else {
                "outcome_unknown"
            };
            outcome = Json::object([
                ("state", "unknown".into()),
                ("observedAt", Json::String(at.clone())),
                ("evidence", evidence.clone()),
                ("code", "native_active_owner_unconfirmed".into()),
            ]);
            waiting = wait(
                "external_outcome",
                "The stored unfinished turn is visible, but its original native owner has not confirmed active execution.",
            );
        } else {
            phase = if truthy(field(run, "cancellation")) {
                "cancel_requested"
            } else {
                let mut needs_input = false;
                for flag in field(native, "activeFlags").as_array().unwrap_or(&[]) {
                    let flag = try_js_string(flag)?;
                    if ["waitingOnApproval", "waitingOnUserInput"]
                        .iter()
                        .any(|v| flag == JsString::from(*v))
                    {
                        needs_input = true;
                        break;
                    }
                }
                if needs_input {
                    "waiting_input"
                } else {
                    "running"
                }
            };
            if phase == "waiting_input" {
                waiting = wait(
                    "user",
                    "The native turn is waiting for approval or user input.",
                );
            }
        }
    } else {
        waiting = wait(
            "publication",
            "Native execution ended; the ticket handoff is being published.",
        );
    }
    Ok(Json::object([
        ("phase", phase.into()),
        ("waiting", waiting),
        ("externalOutcome", outcome),
    ]))
}
