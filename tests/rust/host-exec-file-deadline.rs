use super::{
    FileCommand, FileFailure, FileOptions, exec_file, exec_file_without_timeout, source_output,
};
use std::path::PathBuf;

#[tokio::test]
async fn deadlines_and_source_raw_byte_limits_preserve_native_ownership() {
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

    let source_root =
        std::env::temp_dir().join(format!("ivy-source-output-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&source_root).unwrap();
    #[cfg(windows)]
    {
        let helper = crate::native_plan::native_binary_path(&source_root, "ivy-job");
        std::fs::create_dir_all(helper.parent().unwrap()).unwrap();
        std::fs::copy(std::env::var_os("IVY_BACKUP_JOB").unwrap(), helper).unwrap();
    }
    for (maximum, stdout, stderr, expected) in [
        (4, "ffffffff", "", Some(("����", ""))),
        (2, "e282", "", Some(("�", ""))),
        (3, "efbbbf", "", Some(("\u{feff}", ""))),
        (4, "61626364", "65666768", None),
        (0, "", "", Some(("", ""))),
        (0, "61", "", None),
    ] {
        let result = source_output(FileCommand {
            executable: std::env::var_os("IVY_RESTORE_NODE").unwrap(),
            args: vec!["--eval".into(), format!("process.stdout.write(Buffer.from('{stdout}','hex'));process.stderr.write(Buffer.from('{stderr}','hex'))").into()],
            cwd: Some(source_root.clone()),
            timeout_ms: 3000,
            maximum_buffer_bytes: maximum,
        }, source_root.clone()).await.unwrap();
        if let Some((stdout, stderr)) = expected {
            assert_eq!(result.failure, None, "raw byte limit {maximum}");
            assert_eq!(
                (result.stdout, result.stderr),
                (stdout.into(), stderr.into())
            );
            assert_eq!((result.exit_code, result.signal), (Some(0), None));
        } else {
            assert!(
                matches!(result.failure, Some(FileFailure::MaxBuffer(_))),
                "{result:?}"
            );
            assert!(result.stdout.0.len() + result.stderr.0.len() <= maximum);
        }
    }
    assert!(source_root.starts_with(std::env::temp_dir()));
    std::fs::remove_dir_all(source_root).unwrap();
}
