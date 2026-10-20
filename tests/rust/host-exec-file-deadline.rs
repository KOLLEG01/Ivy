use super::{FileCommand, FileFailure, FileOptions, exec_file, exec_file_without_timeout};
use std::path::PathBuf;

#[tokio::test]
async fn original_unset_timeout_keeps_output_ownership_and_finite_callers_keep_their_deadline() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let node = std::env::var_os("IVY_RESTORE_NODE").expect("pinned Node fixture workload");
    let job = std::env::var_os("IVY_BACKUP_JOB").map(PathBuf::from);
    let command = || FileCommand {
        executable: node.clone(),
        args: vec![
            "--eval".into(),
            "setTimeout(()=>process.stdout.write('completed'),80)".into(),
        ],
        cwd: None,
        timeout_ms: 1,
        maximum_buffer_bytes: 128,
    };
    let unbounded = exec_file_without_timeout(
        command(),
        root.clone(),
        FileOptions {
            job_launcher: job.clone(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(unbounded.failure, None);
    assert_eq!(unbounded.stdout, "completed".into());
    assert_eq!(unbounded.exit_code, Some(0));
    let bounded = exec_file(
        command(),
        root,
        FileOptions {
            job_launcher: job,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(bounded.failure, Some(FileFailure::Timeout));
}
