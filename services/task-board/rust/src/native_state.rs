//! Projection of an exact native observation. Terminal outcomes cannot regress.
use ivy_contracts::{IvyError, IvyJson as Json, IvyResult, JsString};
fn field<'a>(v: &'a Json, k: &str) -> &'a Json {
    static NULL: Json = Json::Null;
    v.get(k).unwrap_or(&NULL)
}
fn equal(v: &Json, k: &str, s: &str) -> bool {
    v.get(k).is_some_and(|value| value.is_str(s))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunPhase {
    Starting,
    Running,
    WaitingInput,
    PublishingResult,
    Completed,
    Failed,
    CancelRequested,
    Cancelled,
    OutcomeUnknown,
}
impl RunPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Running => "running",
            Self::WaitingInput => "waiting_input",
            Self::PublishingResult => "publishing_result",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::CancelRequested => "cancel_requested",
            Self::Cancelled => "cancelled",
            Self::OutcomeUnknown => "outcome_unknown",
        }
    }
    pub fn read(value: Option<&Json>) -> Option<Self> {
        [
            Self::Starting,
            Self::Running,
            Self::WaitingInput,
            Self::PublishingResult,
            Self::Completed,
            Self::Failed,
            Self::CancelRequested,
            Self::Cancelled,
            Self::OutcomeUnknown,
        ]
        .into_iter()
        .find(|phase| value.is_some_and(|value| value.is_str(phase.as_str())))
    }
    pub fn terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutcomeState {
    Active,
    Unknown,
    Succeeded,
    Failed,
    Cancelled,
}
impl OutcomeState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Unknown => "unknown",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
    pub fn terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Cancelled)
    }
}

#[derive(Debug)]
pub struct NativeOutcome {
    pub state: OutcomeState,
    pub code: Option<JsString>,
}
impl NativeOutcome {
    /// Compare the boundary value without normalizing missing or malformed fields.
    pub fn matches(&self, value: &Json) -> bool {
        equal(value, "state", self.state.as_str())
            && match (&self.code, value.get("code")) {
                (None, Some(Json::Null)) => true,
                (Some(code), Some(Json::String(previous))) => code == previous,
                _ => false,
            }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunWait {
    OwnerUnconfirmed,
    Publication,
}
impl RunWait {
    fn reason(self) -> &'static str {
        match self {
            Self::OwnerUnconfirmed => "external_outcome",
            Self::Publication => "publication",
        }
    }
    fn detail(self) -> &'static str {
        match self {
            Self::OwnerUnconfirmed => {
                "The stored unfinished turn is visible, but its original native owner has not confirmed active execution."
            }
            Self::Publication => "Native execution ended; the ticket handoff is being published.",
        }
    }
}

/// One native projection. Observation metadata is borrowed only at publication.
#[derive(Debug)]
pub struct RunProjection {
    pub phase: RunPhase,
    pub outcome: NativeOutcome,
    pub waiting: Option<RunWait>,
}
impl RunProjection {
    pub fn waiting_json(&self, at: &JsString) -> Json {
        self.waiting.map_or(Json::Null, |wait| {
            Json::object([
                ("reason", wait.reason().into()),
                ("detail", wait.detail().into()),
                ("since", Json::String(at.clone())),
            ])
        })
    }
    /// Serialize once at the journal boundary, retaining the original field order.
    pub fn into_parts(self, at: &JsString, evidence: &Json) -> (Json, Json, Json) {
        let waiting = self.waiting_json(at);
        let unknown = self.outcome.state == OutcomeState::Unknown;
        let code = self.outcome.code.map(Json::String).unwrap_or(Json::Null);
        let state = self.outcome.state.as_str().into();
        let outcome = if unknown {
            Json::object([
                ("state", state),
                ("observedAt", Json::String(at.clone())),
                ("evidence", evidence.clone()),
                ("code", code),
            ])
        } else {
            Json::object([
                ("state", state),
                ("code", code),
                ("observedAt", Json::String(at.clone())),
                ("evidence", evidence.clone()),
            ])
        };
        (self.phase.as_str().into(), outcome, waiting)
    }
}
pub fn finished_run(run: &Json) -> bool {
    RunPhase::read(run.get("phase")).is_some_and(RunPhase::terminal)
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
            )
            .unwrap();
            assert_eq!(state.phase, RunPhase::WaitingInput);
        }
        assert_eq!(
            project_native_state(
                &run,
                &"inProgress".into(),
                &"epoch".into(),
                &thread(Json::Number(1.0)),
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
            )
            .unwrap_err()
            .code,
            "task_board_native_state_changed"
        );
    }
}
pub fn read_outcome(run: &Json, status: &JsString) -> IvyResult<NativeOutcome> {
    let prior = field(run, "externalOutcome");
    let state = if status.eq_str("completed") {
        OutcomeState::Succeeded
    } else if status.eq_str("failed") {
        OutcomeState::Failed
    } else if status.eq_str("interrupted") {
        if equal(prior, "state", "failed") {
            OutcomeState::Failed
        } else if !field(run, "cancellation").is_null() {
            OutcomeState::Cancelled
        } else {
            OutcomeState::Failed
        }
    } else {
        OutcomeState::Active
    };
    let code = if ["inProgress", "completed"].iter().any(|v| status.eq_str(v)) {
        None
    } else {
        let mut code = JsString::from("native_");
        code.0.extend_from_slice(&status.0);
        Some(code)
    };
    let refined_failure = field(run, "cancellation").is_null()
        && equal(prior, "state", "failed")
        && equal(prior, "code", "native_interrupted")
        && state == OutcomeState::Failed
        && code
            .as_ref()
            .is_some_and(|code| code.eq_str("native_failed"));
    let outcome = NativeOutcome { state, code };
    if !refined_failure
        && ["succeeded", "failed", "cancelled"]
            .iter()
            .any(|v| equal(prior, "state", v))
        && !outcome.matches(prior)
    {
        return Err(IvyError::new(
            "task_board_native_state_changed",
            "A native read cannot regress or reclassify an already known terminal outcome.",
        ));
    }
    Ok(outcome)
}
pub fn project_native_state(
    run: &Json,
    status: &JsString,
    epoch: &Json,
    thread: &Json,
) -> IvyResult<RunProjection> {
    let mut phase = RunPhase::PublishingResult;
    let mut waiting = None;
    let mut outcome = read_outcome(run, status)?;
    if status.eq_str("inProgress") {
        let native = field(thread, "status");
        if epoch.as_js_string().is_none_or(|value| value.0.is_empty())
            || !equal(native, "type", "active")
        {
            phase = if !field(run, "cancellation").is_null() {
                RunPhase::CancelRequested
            } else {
                RunPhase::OutcomeUnknown
            };
            outcome = NativeOutcome {
                state: OutcomeState::Unknown,
                code: Some("native_active_owner_unconfirmed".into()),
            };
            waiting = Some(RunWait::OwnerUnconfirmed);
        } else {
            phase = if !field(run, "cancellation").is_null() {
                RunPhase::CancelRequested
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
                        .any(|v| flag.eq_str(v))
                    {
                        needs_input = true;
                        break;
                    }
                }
                if needs_input {
                    RunPhase::WaitingInput
                } else {
                    RunPhase::Running
                }
            };
        }
    } else {
        waiting = Some(RunWait::Publication);
    }
    Ok(RunProjection {
        phase,
        waiting,
        outcome,
    })
}
