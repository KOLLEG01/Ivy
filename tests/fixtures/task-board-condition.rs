use ivy_contracts::{IvyJson as Json, IvyResult, JsString};
use ivy_task_board::engine_read::{condition_policy::*, read_failure::ReadFailure};
use std::cell::Cell;
fn task() -> Json {
    Json::object([
        ("fields", Json::object([("control", "agent".into())])),
        ("workflowState", "waiting".into()),
        ("claim", Json::Null),
        ("publication", Json::Null),
        ("runtimeWait", Json::Null),
    ])
}
struct Evidence {
    calls: Cell<usize>,
}
impl ConditionStore for Evidence {
    fn failed_handoff(&self, id: &JsString) -> IvyResult<Option<Json>> {
        assert_eq!(id, &JsString::from("run"));
        self.calls.set(self.calls.get() + 1);
        Ok(Some(Json::object([
            ("code", "retained_error".into()),
            ("detail", "Original publication failed.".into()),
        ])))
    }
}
#[test]
fn current_condition_precedence_covers_active_terminal_issue_environment_review_and_wake() {
    let store = Evidence {
        calls: Cell::new(0),
    };
    let mut own = task();
    let wake = Json::object([
        ("state", "deferred".into()),
        ("detail", "Future appointment.".into()),
    ]);
    assert_eq!(task_condition(&store, &own, Some(&wake)).unwrap(), wake);
    for (phase, state) in [
        ("claimed", "starting"),
        ("run_created", "starting"),
        ("starting", "starting"),
        ("running", "active"),
        ("waiting_input", "needs_user"),
        ("outcome_unknown", "recovering"),
        ("cancel_requested", "recovering"),
        ("publishing_result", "publishing"),
    ] {
        own.insert(
            "claim",
            Json::object([
                ("phase", phase.into()),
                ("target", Json::object([("hostId", "host".into())])),
                (
                    "request",
                    Json::object([
                        ("coordinationOnly", true.into()),
                        ("recoveryFor", "old".into()),
                    ]),
                ),
            ]),
        );
        let condition = task_condition(&store, &own, Some(&wake)).unwrap();
        assert_eq!(condition.get("state"), Some(&state.into()));
        if phase == "running" {
            assert_eq!(condition.get("detail"), Some(&"Working on host.".into()));
        }
    }
    own.insert("publication", Json::object([] as [(&str, Json); 0]));
    assert_eq!(
        task_condition(&store, &own, Some(&wake))
            .unwrap()
            .get("detail"),
        Some(&"Publishing the current task update.".into())
    );
    own.insert("publication", Json::Null);
    own.insert("claim", Json::Null);
    own.insert(
        "completionIssue",
        Json::object([
            ("run", Json::object([("objectId", "run".into())])),
            ("detail", "Stored issue".into()),
            ("attempts", Json::Number(0.0)),
        ]),
    );
    let issue = task_condition(&store, &own, Some(&wake)).unwrap();
    assert_eq!(issue.get("state"), Some(&"failed".into()));
    assert_eq!(
        issue.get("detail"),
        Some(&"retained_error: Original publication failed.".into())
    );
    for state in ["backlog", "done", "cancelled"] {
        own.insert("workflowState", state.into());
        assert_eq!(
            task_condition(&store, &own, None).unwrap().get("state"),
            Some(&"idle".into())
        );
    }
    own.remove("completionIssue");
    own.insert("workflowState", "review".into());
    own.insert(
        "runtimeWait",
        Json::object([
            ("reason", "host".into()),
            ("detail", "Retrying owner.".into()),
        ]),
    );
    assert_eq!(
        task_condition(&store, &own, Some(&wake))
            .unwrap()
            .get("state"),
        Some(&"blocked_environment".into())
    );
    own.insert("runtimeWait", Json::Null);
    assert_eq!(
        task_condition(&store, &own, Some(&wake))
            .unwrap()
            .get("state"),
        Some(&"needs_user".into())
    );
    assert_eq!(
        claimed_workflow_state(&Json::object([("reason", "workflow".into())])),
        JsString::from("in_progress")
    );
    assert_eq!(
        store.calls.get(),
        4,
        "Retained evidence is eager even for terminal views"
    );
}
struct Failed(ReadFailure<String>);
impl ConditionStore<ReadFailure<String>> for Failed {
    fn failed_handoff(&self, _: &JsString) -> Result<Option<Json>, ReadFailure<String>> {
        Err(self.0.clone())
    }
}
#[test]
fn retained_condition_read_failure_keeps_its_native_cause() {
    let store = Failed(ReadFailure::source("Original storage failure".into()));
    let mut own = task();
    own.insert(
        "completionIssue",
        Json::object([("run", Json::object([("objectId", "run".into())]))]),
    );
    let error = task_condition(&store, &own, None).unwrap_err();
    assert!(error.ptr_eq(&store.0));
}
