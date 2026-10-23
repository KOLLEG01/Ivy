use super::*;
fn task() -> Document {
    Document {
        pin: Json::object([("objectId", "task".into())]),
        value: Json::object([(
            "primaryResourceRef",
            Json::object([
                ("nativeId", "saved".into()),
                ("serviceNodeId", "node".into()),
            ]),
        )]),
        parent_id: None,
        content_hash: String::new(),
    }
}
fn run() -> Json {
    Json::object([
        ("taskId", "task".into()),
        (
            "primaryResourceRef",
            task().value.required("primaryResourceRef").unwrap().clone(),
        ),
        ("phase", "completed".into()),
        (
            "externalOutcome",
            Json::object([("state", "succeeded".into())]),
        ),
    ])
}
fn observed(thread: Json) -> Json {
    Json::object([(
        "reply",
        Json::object([("result", Json::object([("thread", thread)]))]),
    )])
}
#[test]
fn continuation_requires_own_terminal_known_run_and_same_context() {
    let task = task();
    let good = run();
    validate_continuation(&task, &good).unwrap();
    let mut reordered = good.clone();
    reordered.insert(
        "primaryResourceRef",
        Json::object([
            ("serviceNodeId", "node".into()),
            ("nativeId", "saved".into()),
        ]),
    );
    validate_continuation(&task, &reordered).unwrap();
    for (key, value) in [
        ("taskId", "other".into()),
        ("primaryResourceRef", Json::Null),
        ("phase", "running".into()),
        (
            "externalOutcome",
            Json::object([("state", "active".into())]),
        ),
        (
            "externalOutcome",
            Json::object([("state", "outcome_unknown".into())]),
        ),
    ] {
        let mut invalid = good.clone();
        invalid.insert(key, value);
        assert_eq!(
            validate_continuation(&task, &invalid).unwrap_err().code,
            "task_board_execution_unresolved"
        );
    }
    for phase in ["failed", "cancelled"] {
        let mut terminal = good.clone();
        terminal.insert("phase", phase.into());
        validate_continuation(&task, &terminal).unwrap();
    }
}
#[test]
fn native_observation_retains_workspace_and_rejects_changed_directory_project_or_identity() {
    let task = task();
    let previous = Json::object([
        ("canonicalCwd", "/saved".into()),
        ("intendedPath", "/intended".into()),
        ("project", Json::object([("nativeId", "project".into())])),
    ]);
    let thread = Json::object([
        ("id", "saved".into()),
        ("ephemeral", false.into()),
        ("cwd", "/saved".into()),
        ("projectId", "project".into()),
    ]);
    let actual = continuation_workspace(&task.value, &previous, &observed(thread.clone())).unwrap();
    assert_eq!(
        actual.required("nativeProjectId").unwrap(),
        &Json::from("project")
    );
    assert_eq!(
        actual.required("intendedPath").unwrap(),
        &Json::from("/intended")
    );
    for (key, value) in [
        ("id", "other".into()),
        ("ephemeral", true.into()),
        ("cwd", "/changed".into()),
        ("cwd", Json::Number(42.0)),
        ("projectId", "other-project".into()),
    ] {
        let mut changed = thread.clone();
        changed.insert(key, value);
        assert_eq!(
            continuation_workspace(&task.value, &previous, &observed(changed))
                .unwrap_err()
                .code,
            "task_board_workspace_mismatch"
        );
    }
    let failed = Json::object([(
        "reply",
        Json::object([("error", Json::object([("code", "unavailable".into())]))]),
    )]);
    assert_eq!(
        continuation_workspace(&task.value, &previous, &failed)
            .unwrap_err()
            .code,
        "task_board_native_read_failed"
    );
    let mut no_project = previous;
    no_project.insert("project", Json::Null);
    let mut thread = thread;
    thread.remove("projectId");
    assert!(
        continuation_workspace(&task.value, &no_project, &observed(thread))
            .unwrap()
            .get("nativeProjectId")
            .is_none()
    );
}
