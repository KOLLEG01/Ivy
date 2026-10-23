//! Projection of an exact native observation. Terminal outcomes cannot regress.
use ivy_contracts::{IvyError, IvyJson as Json, IvyResult, JsString};
fn field<'a>(v: &'a Json, k: &str) -> &'a Json {
    static NULL: Json = Json::Null;
    v.get(k).unwrap_or(&NULL)
}
fn equal(v: &Json, k: &str, s: &str) -> bool {
    v.get(k) == Some(&Json::from(s))
}
pub fn finished_run(run: &Json) -> bool {
    ["completed", "failed", "cancelled"]
        .iter()
        .any(|v| equal(run, "phase", v))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn active_flags_are_native_strings_and_terminal_outcomes_do_not_regress() {
        let run = Json::object([
            ("nativeEpoch", "epoch".into()),
            (
                "externalOutcome",
                Json::object([("state", "active".into())]),
            ),
        ]);
        let thread = |flag| {
            Json::object([(
                "status",
                Json::object([
                    ("type", "active".into()),
                    ("activeFlags", Json::Array(vec![flag])),
                ]),
            )])
        };
        for flag in ["waitingOnApproval", "waitingOnUserInput"] {
            let state = project_native_state(
                &run,
                &"inProgress".into(),
                &"epoch".into(),
                &thread(flag.into()),
                &"2026-10-23T00:00:00.000Z".into(),
                &Json::Null,
            )
            .unwrap();
            assert_eq!(
                state.required("phase").unwrap(),
                &Json::from("waiting_input")
            );
        }
        assert_eq!(
            project_native_state(
                &run,
                &"inProgress".into(),
                &"epoch".into(),
                &thread(Json::Number(1.0)),
                &"2026-10-23T00:00:00.000Z".into(),
                &Json::Null
            )
            .unwrap_err()
            .code,
            "task_board_native_state_invalid"
        );
        let mut terminal = run;
        terminal.insert(
            "externalOutcome",
            Json::object([("state", "succeeded".into()), ("code", Json::Null)]),
        );
        assert_eq!(
            project_native_state(
                &terminal,
                &"inProgress".into(),
                &"epoch".into(),
                &thread("running".into()),
                &"2026-10-23T00:00:00.000Z".into(),
                &Json::Null
            )
            .unwrap_err()
            .code,
            "task_board_native_state_changed"
        );
    }
}
pub fn read_outcome(run: &Json, status: &JsString) -> IvyResult<Json> {
    let prior = field(run, "externalOutcome");
    let state = if status == &JsString::from("completed") {
        "succeeded"
    } else if status == &JsString::from("failed") {
        "failed"
    } else if status == &JsString::from("interrupted") {
        if equal(prior, "state", "failed") {
            "failed"
        } else if !field(run, "cancellation").is_null() {
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
    let refined_failure = field(run, "cancellation").is_null()
        && equal(prior, "state", "failed")
        && equal(prior, "code", "native_interrupted")
        && state == "failed"
        && code == Json::from("native_failed");
    if !refined_failure
        && ["succeeded", "failed", "cancelled"]
            .iter()
            .any(|v| equal(prior, "state", v))
        && (!equal(prior, "state", state) || prior.get("code") != Some(&code))
    {
        return Err(IvyError::new(
            "task_board_native_state_changed",
            "A native read cannot regress or reclassify an already known terminal outcome.",
        ));
    }
    Ok(Json::object([("state", state.into()), ("code", code)]))
}
pub fn project_native_state(
    run: &Json,
    status: &JsString,
    epoch: &Json,
    thread: &Json,
    at: &JsString,
    evidence: &Json,
) -> IvyResult<Json> {
    let mut phase = "publishing_result";
    let mut waiting = Json::Null;
    let mut outcome = read_outcome(run, status)?;
    outcome.insert("observedAt", Json::String(at.clone()));
    outcome.insert("evidence", evidence.clone());
    let wait = |reason: &str, detail: &str| {
        Json::object([
            ("reason", reason.into()),
            ("detail", detail.into()),
            ("since", Json::String(at.clone())),
        ])
    };
    if status == &JsString::from("inProgress") {
        let native = field(thread, "status");
        if epoch.as_js_string().is_none_or(|value| value.0.is_empty())
            || !equal(native, "type", "active")
        {
            phase = if !field(run, "cancellation").is_null() {
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
            phase = if !field(run, "cancellation").is_null() {
                "cancel_requested"
            } else {
                let mut needs_input = false;
                for flag in field(native, "activeFlags").as_array().unwrap_or(&[]) {
                    let flag = flag.as_js_string().ok_or_else(|| {
                        IvyError::new(
                            "task_board_native_state_invalid",
                            "Native active flags must be strings.",
                        )
                    })?;
                    if ["waitingOnApproval", "waitingOnUserInput"]
                        .iter()
                        .any(|v| flag == &JsString::from(*v))
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
