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
            Json::object([("state", "unknown".into())]),
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
    let actual =
        continuation_workspace(&task.value, &previous, &observed(thread.clone()), None).unwrap();
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
            continuation_workspace(&task.value, &previous, &observed(changed), None)
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
        continuation_workspace(&task.value, &previous, &failed, None)
            .unwrap_err()
            .code,
        "task_board_native_read_failed"
    );
    let mut no_project = previous;
    no_project.insert("project", Json::Null);
    let mut thread = thread;
    thread.remove("projectId");
    assert!(
        continuation_workspace(&task.value, &no_project, &observed(thread), None)
            .unwrap()
            .get("nativeProjectId")
            .is_none()
    );
}

#[test]
fn continuation_accepts_native_owner_resolved_alias_without_changing_saved_workspace() {
    let task = task();
    let canonical = Json::from(r"\\?\D:\workspace");
    let previous = Json::object([
        ("canonicalCwd", canonical.clone()),
        ("intendedPath", canonical.clone()),
        ("project", Json::object([("nativeId", "project".into())])),
    ]);
    let thread = Json::object([
        ("id", "saved".into()),
        ("ephemeral", false.into()),
        ("cwd", r"D:\workspace".into()),
        ("projectId", "project".into()),
    ]);
    let observation = observed(thread.clone());
    assert!(continuation_workspace(&task.value, &previous, &observation, None).is_err());
    let actual =
        continuation_workspace(&task.value, &previous, &observation, Some(&canonical)).unwrap();
    assert_eq!(actual.required("canonicalCwd").unwrap(), &canonical);
    assert_eq!(actual.required("intendedPath").unwrap(), &canonical);
    assert_eq!(
        actual.required("nativeProjectId").unwrap(),
        &Json::from("project")
    );
    let other = Json::from(r"\\?\D:\other");
    assert!(continuation_workspace(&task.value, &previous, &observation, Some(&other)).is_err());
    for (key, value) in [
        ("id", "other".into()),
        ("projectId", "other-project".into()),
        ("ephemeral", true.into()),
    ] {
        let mut changed = thread.clone();
        changed.insert(key, value);
        assert!(
            continuation_workspace(&task.value, &previous, &observed(changed), Some(&canonical))
                .is_err()
        );
    }
}

#[test]
fn migrated_continuation_uses_current_addresses_without_rewriting_the_original_run() {
    use ivy_contracts::hash::hash_json;
    use ivy_contracts::native_backend::NativeProvider;
    for scoped in [false, true] {
        let backend =
            BackendIdentity::new("joint".into(), NativeProvider::Codex, "primary".into()).unwrap();
        let old = scoped.then(|| {
            BackendIdentity::new("previous".into(), NativeProvider::Codex, "primary".into())
                .unwrap()
        });
        let source = BackendIdentity::select_arguments(
            old.as_ref(),
            Json::object([
                ("hostId", "physical".into()),
                ("serviceNodeId", "old-node".into()),
            ]),
        )
        .unwrap();
        let target = backend
            .arguments(Json::object([
                ("hostId", "physical".into()),
                ("serviceNodeId", "joint-node".into()),
            ]))
            .unwrap();
        let primary = Json::object([
            ("serviceNodeId", "old-node".into()),
            (
                "namespace",
                old.as_ref().map_or("codex", BackendIdentity::id).into(),
            ),
            ("kind", "thread".into()),
            ("nativeId", "saved".into()),
        ]);
        let workspace = Json::object([
            ("hostId", "physical".into()),
            ("serviceNodeId", "old-node".into()),
            ("canonicalCwd", r"\\?\D:\work\Größe".into()),
            ("intendedPath", r"D:\work\Größe".into()),
            (
                "repository",
                Json::object([("commit", "original".into()), ("dirty", true.into())]),
            ),
            (
                "project",
                Json::object([
                    ("serviceNodeId", "old-node".into()),
                    (
                        "namespace",
                        old.as_ref().map_or("codex", BackendIdentity::id).into(),
                    ),
                    ("kind", "project".into()),
                    ("nativeId", "same-project".into()),
                ]),
            ),
        ]);
        let mut run = run();
        run.insert("target", source.clone());
        run.insert("workspace", workspace.clone());
        run.insert("primaryResourceRef", primary.clone());
        let original = hash_json(&run).unwrap();
        let mut task = task();
        let mut current_primary = primary;
        current_primary.insert("serviceNodeId", "joint-node".into());
        current_primary.insert("namespace", backend.id().into());
        task.value.insert("primaryResourceRef", current_primary);
        task.value.insert("primaryBackend", backend.json());
        task.value.insert(
            "nativeRoutes",
            Json::Array(vec![Json::object([
                ("source", source.clone()),
                ("target", target.clone()),
            ])]),
        );
        validate_continuation(&task, &run).unwrap();
        assert_eq!(routed_target(&task.value, &source).unwrap(), target);
        let moved = routed_workspace(&task.value, &source, &workspace).unwrap();
        assert_eq!(moved.get("serviceNodeId"), Some(&"joint-node".into()));
        for key in ["canonicalCwd", "intendedPath", "repository"] {
            assert_eq!(moved.get(key), workspace.get(key));
        }
        let project = moved.required("project").unwrap();
        assert_eq!(project.get("serviceNodeId"), Some(&"joint-node".into()));
        assert_eq!(project.get("namespace"), Some(&"joint".into()));
        assert_eq!(project.get("nativeId"), Some(&"same-project".into()));
        let mut foreign_workspace = workspace.clone();
        foreign_workspace.insert("hostId", "foreign".into());
        assert_eq!(
            routed_workspace(&task.value, &source, &foreign_workspace)
                .unwrap_err()
                .code,
            "task_board_workspace_mismatch"
        );
        let mut foreign_run = run.clone();
        let mut target = source.clone();
        target.insert(
            "backend",
            BackendIdentity::new("wrong".into(), NativeProvider::Codex, "foreign".into())
                .unwrap()
                .json(),
        );
        foreign_run.insert("target", target);
        assert_eq!(
            validate_continuation(&task, &foreign_run).unwrap_err().code,
            "task_board_execution_unresolved"
        );
        assert_eq!(hash_json(&run).unwrap(), original);
    }
}
