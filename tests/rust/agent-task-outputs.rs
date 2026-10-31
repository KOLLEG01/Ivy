use super::*;
use ivy_agent_manager::task_outputs::cleanup;

fn policy(root: &Root) -> ProjectPolicy {
    ProjectPolicy::new(
        node_path::from_native(&root.0.join("service")),
        &IvyJson::object([(
            "internalProjectRoot",
            node_path::from_native(&root.0.join("work")).into(),
        )]),
        &[],
        node_path::Context::current().unwrap(),
        &node_path::from_native(&root.0),
    )
    .unwrap()
}

#[test]
fn cleanup_is_scoped_repeatable_and_resumes_interrupted_removal() {
    let root = Root::new();
    let policy = policy(&root);
    let outputs = root.0.join("work/outputs");
    let target = outputs.join("TASK-0123");
    fs::create_dir_all(target.join("nested")).unwrap();
    let outside = root.0.join("outside");
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("keep"), "preserve").unwrap();
    ivy_host_runtime::filesystem::directory_link(&outside, &target.join("linked")).unwrap();
    fs::write(target.join("nested/proof"), "temporary").unwrap();
    let mut permissions = fs::metadata(target.join("nested/proof"))
        .unwrap()
        .permissions();
    permissions.set_readonly(true);
    fs::set_permissions(target.join("nested/proof"), permissions).unwrap();
    fs::create_dir_all(outputs.join("TASK-0124")).unwrap();
    fs::write(outputs.join("TASK-0124/keep"), "other ticket").unwrap();
    fs::create_dir_all(root.0.join("work/taskboard-worktree-TASK-0123")).unwrap();
    let result = cleanup(&policy, &"TASK-0123".into(), &"archive-1".into()).unwrap();
    assert_eq!(result.get("removed"), Some(&true.into()));
    assert!(!target.exists());
    assert!(outside.join("keep").is_file());
    assert!(outputs.join("TASK-0124/keep").is_file());
    assert!(root.0.join("work/taskboard-worktree-TASK-0123").is_dir());
    assert_eq!(
        cleanup(&policy, &"TASK-0123".into(), &"archive-1".into())
            .unwrap()
            .get("removed"),
        Some(&false.into())
    );

    // A restarted request finishes the exact renamed directory, preserving any
    // new outputs created after the original rename.
    let hash = hash_json(&IvyJson::from("archive-2")).unwrap();
    let retiring = outputs.join(format!(".retiring-TASK-0123-{}", &hash[7..]));
    fs::create_dir_all(&retiring).unwrap();
    fs::write(retiring.join("old"), "remove").unwrap();
    fs::create_dir_all(&target).unwrap();
    fs::write(target.join("new"), "preserve").unwrap();
    cleanup(&policy, &"TASK-0123".into(), &"archive-2".into()).unwrap();
    assert!(!retiring.exists());
    assert!(target.join("new").is_file());
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        let locked = fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(target.join("new"))
            .unwrap();
        assert_eq!(
            cleanup(&policy, &"TASK-0123".into(), &"archive-3".into())
                .unwrap_err()
                .code,
            "task_output_cleanup_failed"
        );
        drop(locked);
        cleanup(&policy, &"TASK-0123".into(), &"archive-3".into()).unwrap();
        assert!(!target.exists());
    }
    for key in ["../TASK-0123", "TASK-x", "TASK-0123/other", ""] {
        assert_eq!(
            cleanup(&policy, &key.into(), &"archive-3".into())
                .unwrap_err()
                .code,
            "invalid_arguments"
        );
    }
}

#[test]
fn cleanup_rejects_redirected_outputs_and_protected_storage() {
    let root = Root::new();
    let policy = policy(&root);
    let outputs = root.0.join("work/outputs");
    let outside = root.0.join("outside");
    fs::create_dir_all(&outputs).unwrap();
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("keep"), "preserve").unwrap();
    fn link(target: &std::path::Path, path: &std::path::Path) {
        ivy_host_runtime::filesystem::directory_link(target, path).unwrap();
    }
    link(&outside, &outputs.join("TASK-0123"));
    assert_eq!(
        cleanup(&policy, &"TASK-0123".into(), &"archive-1".into())
            .unwrap_err()
            .code,
        "target_conflict"
    );
    assert!(outside.join("keep").is_file());
    fs::remove_dir_all(&outputs).unwrap();
    link(&outside, &outputs);
    assert_eq!(
        cleanup(&policy, &"TASK-0123".into(), &"archive-2".into())
            .unwrap_err()
            .code,
        "target_conflict"
    );
    assert!(outside.join("keep").is_file());
    let mut protected = policy;
    protected
        .protected_roots
        .push(node_path::from_native(&root.0.join("work")));
    assert_eq!(
        cleanup(&protected, &"TASK-0123".into(), &"archive-3".into())
            .unwrap_err()
            .code,
        "target_conflict"
    );
}
