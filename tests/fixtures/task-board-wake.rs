use ivy_contracts::{IvyError, IvyJson as Json, IvyResult, JsString, hash::hash_json};
use ivy_task_board::wake::*;
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
};

fn task() -> Json {
    Json::object([
        ("fields", Json::object([("control", "agent".into())])),
        ("workflowState", "waiting".into()),
        ("claim", Json::Null),
        ("publication", Json::Null),
        ("runtimeWait", Json::Null),
        ("wakeAt", Json::Null),
        ("dependencies", Json::Array(vec![])),
        (
            "observedDependencies",
            Json::object([] as [(&str, Json); 0]),
        ),
        ("comments", Json::Array(vec![])),
        ("commentDeliveries", Json::Array(vec![])),
        ("updatedAt", "2026-10-23T00:00:00.000Z".into()),
    ])
}
fn watched() -> Json {
    Json::object([
        (
            "metadata",
            Json::object([("effectivelyArchived", false.into())]),
        ),
        (
            "value",
            Json::object([
                ("workRevision", Json::Number(4.0)),
                ("acceptedReview", Json::Null),
                ("workflowState", "waiting".into()),
            ]),
        ),
    ])
}
#[derive(Default)]
struct Reads {
    records: RefCell<HashMap<JsString, Json>>,
    calls: RefCell<Vec<JsString>>,
    active: Cell<usize>,
    maximum: Cell<usize>,
    failure: RefCell<Option<IvyError>>,
}
impl WakeStore for Reads {
    async fn watched_task(&self, id: &JsString) -> IvyResult<Json> {
        self.calls.borrow_mut().push(id.clone());
        self.active.set(self.active.get() + 1);
        self.maximum.set(self.maximum.get().max(self.active.get()));
        tokio::task::yield_now().await;
        self.active.set(self.active.get() - 1);
        if let Some(error) = self.failure.borrow().as_ref() {
            return Err(error.clone());
        }
        self.records
            .borrow()
            .get(id)
            .cloned()
            .ok_or_else(|| IvyError::new("not_found", "Deleted fixture task."))
    }
    fn deleted(&self, error: &IvyError) -> bool {
        WakeFailure::deleted(error)
    }
}
#[tokio::test(flavor = "current_thread")]
async fn watched_headers_are_bounded_and_only_material_changes_wake() {
    let reads = Reads::default();
    let mut own = task();
    let ids: Vec<_> = (0..19)
        .map(|i| {
            let id: JsString = format!("peer{i}").into();
            reads.records.borrow_mut().insert(id.clone(), watched());
            Json::String(id)
        })
        .collect();
    own.insert("dependencies", Json::Array(ids));
    let snapshot = dependency_snapshot(&reads, &own).await.unwrap();
    assert_eq!(reads.maximum.get(), 8);
    assert_eq!(reads.calls.borrow().len(), 19);
    own.insert("observedDependencies", snapshot.clone());
    assert!(!dependencies_changed(&own, &snapshot).unwrap());
    let id = JsString::from("peer0");
    for (field, replacement, changed) in [
        ("claim", Json::object([("phase", "running".into())]), false),
        (
            "publication",
            Json::object([("objectId", "operation".into())]),
            false,
        ),
        ("workflowState", "todo".into(), false),
        ("workRevision", Json::Number(5.0), true),
        (
            "acceptedReview",
            Json::object([
                ("objectId", "review".into()),
                ("revision", Json::Number(1.0)),
            ]),
            true,
        ),
        ("workflowState", "done".into(), true),
        ("workflowState", "cancelled".into(), true),
    ] {
        let mut document = watched();
        let mut body = document.required("value").unwrap().clone();
        body.insert(field, replacement);
        document.insert("value", body);
        reads.records.borrow_mut().insert(id.clone(), document);
        assert_eq!(
            dependencies_changed(&own, &dependency_snapshot(&reads, &own).await.unwrap()).unwrap(),
            changed,
            "{field}"
        );
    }
    let mut archived = watched();
    archived.insert(
        "metadata",
        Json::object([("effectivelyArchived", true.into())]),
    );
    reads.records.borrow_mut().insert(id.clone(), archived);
    assert!(dependencies_changed(&own, &dependency_snapshot(&reads, &own).await.unwrap()).unwrap());
    reads.records.borrow_mut().remove(&id);
    let deleted = dependency_snapshot(&reads, &own).await.unwrap();
    assert_eq!(
        deleted.get_key(&id),
        Some(&Json::from(hash_json(&Json::from("deleted")).unwrap()))
    );
    assert_eq!(dependency_snapshot(&reads, &own).await.unwrap(), deleted);
}

#[tokio::test(flavor = "current_thread")]
async fn watch_validation_checks_identity_and_retains_nonabsence_errors() {
    let reads = Reads::default();
    reads.records.borrow_mut().insert("peer".into(), watched());
    let ids = vec![Json::from("peer")];
    validate_dependencies(&reads, Some(&"self".into()), &ids)
        .await
        .unwrap();
    assert_eq!(
        validate_dependencies(&reads, Some(&"peer".into()), &ids)
            .await
            .unwrap_err()
            .code,
        "invalid_arguments"
    );
    let mut own = task();
    own.insert("dependencies", Json::Array(ids));
    for code in [
        "service_unavailable",
        "task_board_scope_mismatch",
        "object_archived",
    ] {
        let error = IvyError::new(code, "Original provider error.")
            .with_outcome(ivy_contracts::Outcome::Unknown);
        *reads.failure.borrow_mut() = Some(error.clone());
        let actual = dependency_snapshot(&reads, &own).await.unwrap_err();
        assert_eq!(actual.code, error.code);
        assert_eq!(actual.outcome.to_json(), error.outcome.to_json());
    }
}

fn queue(task: &mut Json, kind: &str, peer: bool) {
    let mut comment = Json::object([
        ("commentId", "input".into()),
        ("authorKind", kind.into()),
        ("body", "Ticket edited".into()),
    ]);
    if peer {
        comment.insert("sourceTaskId", "peer".into());
    }
    task.insert("comments", Json::Array(vec![comment]));
    task.insert(
        "commentDeliveries",
        Json::Array(vec![Json::object([
            ("commentId", "input".into()),
            ("state", "queued".into()),
        ])]),
    );
}
#[tokio::test(flavor = "current_thread")]
async fn wake_precedence_keeps_comments_retries_appointments_and_manual_states_independent() {
    let reads = Reads::default();
    let at: JsString = "2026-10-23T00:00:30.000Z".into();
    let mut own = task();
    assert!(
        !should_wake(&reads, &own, &"2026-10-23T00:00:29.999Z".into())
            .await
            .unwrap()
    );
    assert!(should_wake(&reads, &own, &at).await.unwrap());
    own.insert("workflowState", "todo".into());
    own.insert("wakeAt", "2027-01-01T00:00:00.000Z".into());
    assert!(should_wake(&reads, &own, &at).await.unwrap());
    own.insert("workflowState", "waiting".into());
    assert!(!should_wake(&reads, &own, &at).await.unwrap());
    own.insert("wakeAt", Json::String(at.clone()));
    assert!(should_wake(&reads, &own, &at).await.unwrap());
    own.insert(
        "completionIssue",
        Json::object([("retryAt", "2027-01-01T00:00:00.000Z".into())]),
    );
    assert!(!should_wake(&reads, &own, &at).await.unwrap());
    queue(&mut own, "agent", true);
    for state in ["review", "done", "backlog", "cancelled"] {
        own.insert("workflowState", state.into());
        assert!(should_wake(&reads, &own, &at).await.unwrap(), "{state}");
        assert!(next_execution_mode(&own, &at));
    }
    own.insert(
        "runtimeWait",
        Json::object([("retryAt", "2027-01-01T00:00:00.000Z".into())]),
    );
    assert!(!should_wake(&reads, &own, &at).await.unwrap());
    own.insert("runtimeWait", Json::Null);
    own.insert("claim", Json::object([] as [(&str, Json); 0]));
    assert!(!should_wake(&reads, &own, &at).await.unwrap());
    assert!(!next_execution_mode(&own, &at));
    assert!(
        reads.calls.borrow().is_empty(),
        "Earlier local decisions need no peer reads"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn questions_do_not_gate_independent_wakes_and_agent_updates_do_not_wake() {
    let reads = Reads::default();
    let at: JsString = "2026-10-23T00:00:30.000Z".into();
    let mut own = task();
    queue(&mut own, "agent", false);
    own.insert("wakeAt", "2027-01-01T00:00:00.000Z".into());
    assert!(!comments_pending(&own));
    assert!(!should_wake(&reads, &own, &at).await.unwrap());
    queue(&mut own, "user", false);
    assert!(should_wake(&reads, &own, &at).await.unwrap());
    let user = Json::object([("recipient", Json::object([("kind", "user".into())]))]);
    let peer = Json::object([(
        "recipient",
        Json::object([("kind", "task".into()), ("taskId", "peer".into())]),
    )]);
    assert_eq!(
        wake_condition(&own, &[user.clone()]).get("state"),
        Some(&"needs_user".into())
    );
    own.insert("wakeAt", Json::Null);
    assert_eq!(
        wake_condition(&own, &[peer]).get("state"),
        Some(&"blocked_dependency".into())
    );
    own.insert("workflowState", "todo".into());
    assert_eq!(
        wake_condition(&own, &[user]).get("state"),
        Some(&"queued".into())
    );
}
