use super::*;
use ivy_agent_manager::claude::workflow::WorkflowFiles;

#[tokio::test]
async fn host_workflow_realpaths_keep_native_names_and_missing_paths() {
    let root = Root::new();
    let directory = root.0.join("non-ascii ü 空 😀");
    fs::create_dir(&directory).unwrap();
    let file = directory.join("file");
    fs::write(&file, b"text").unwrap();
    let files = WorkflowFiles::host();
    for path in [
        root.0.clone(),
        file.clone(),
        directory.join("../non-ascii ü 空 😀/file"),
    ] {
        assert_eq!(
            (files.realpath)(node_path::from_native(&path))
                .await
                .unwrap(),
            Some(node_path::from_native(&fs::canonicalize(&path).unwrap()))
        );
    }
    for path in [
        root.0.join("missing"),
        file.join("child"),
        root.0.join("missing/child"),
    ] {
        assert_eq!(
            (files.realpath)(node_path::from_native(&path))
                .await
                .unwrap(),
            None
        );
    }
    assert_eq!((files.realpath)("".into()).await.unwrap(), None);
    let invalid = (files.realpath)("bad\0path".into()).await.unwrap_err();
    assert_eq!(invalid.name, "TypeError");
    assert_eq!(
        f(&invalid.to_wire(), "message"),
        &J::String(invalid.message)
    );
}

#[tokio::test]
async fn host_workflow_reads_preserve_bounds_decoding_containment_and_error_projection() {
    let root = Root::new();
    let file = root.0.join("input");
    let directory = node_path::from_native(&root.0);
    let path = node_path::from_native(&file);
    let files = WorkflowFiles::host();
    for bytes in [
        Vec::new(),
        b"line\n".to_vec(),
        vec![239, 187, 191, 0, 10, 13, 255],
        (0..=255).collect(),
    ] {
        fs::write(&file, &bytes).unwrap();
        assert_eq!(
            (files.read_regular)(directory.clone(), path.clone(), bytes.len())
                .await
                .unwrap(),
            Some(JsString::from(String::from_utf8_lossy(&bytes).as_ref()))
        );
    }
    fs::write(&file, b"oversized").unwrap();
    let error = (files.read_regular)(directory.clone(), path.clone(), 8)
        .await
        .unwrap_err();
    assert_eq!(error.name, "Error");
    assert!(String::from_utf16_lossy(&error.message.0).contains("size limit"));
    assert_eq!(f(&error.to_wire(), "message"), &J::String(error.message));
    let subdirectory = root.0.join("directory");
    fs::create_dir(&subdirectory).unwrap();
    assert!(
        (files.read_regular)(node_path::from_native(&subdirectory), path.clone(), 64)
            .await
            .is_err()
    );
    assert!(
        (files.read_regular)(directory.clone(), node_path::from_native(&subdirectory), 64)
            .await
            .is_err()
    );
    assert_eq!(
        (files.read_regular)(
            directory.clone(),
            node_path::from_native(&file.join("child")),
            64
        )
        .await
        .unwrap(),
        None
    );
    assert_eq!(
        (files.read_regular)(
            directory.clone(),
            node_path::from_native(&root.0.join("missing")),
            64
        )
        .await
        .unwrap(),
        None
    );
    assert_eq!(
        (files.read_regular)(directory, "bad\0path".into(), 1)
            .await
            .unwrap_err()
            .name,
        "TypeError"
    );
}

#[tokio::test]
async fn host_workflow_link_escape_and_sharing_errors_release_owned_handles() {
    let root = Root::new();
    let owned = root.0.join("owned");
    let outside = root.0.join("outside");
    fs::create_dir(&owned).unwrap();
    fs::create_dir(&outside).unwrap();
    let file = outside.join("input");
    fs::write(&file, b"retained").unwrap();
    let link = owned.join("link");
    ivy_host_runtime::filesystem::directory_link(&outside, &link).unwrap();
    let files = WorkflowFiles::host();
    assert!(
        (files.read_regular)(
            node_path::from_native(&owned),
            node_path::from_native(&link.join("input")),
            64
        )
        .await
        .is_err()
    );
    #[cfg(windows)]
    fs::remove_dir(&link).unwrap();
    #[cfg(unix)]
    fs::remove_file(&link).unwrap();
    let hardlink = outside.join("hardlink");
    fs::hard_link(&file, &hardlink).unwrap();
    assert_eq!(
        (files.read_regular)(
            node_path::from_native(&outside),
            node_path::from_native(&hardlink),
            64
        )
        .await
        .unwrap(),
        Some("retained".into())
    );
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        let denied = fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&file)
            .unwrap();
        let original = Command::new("node")
            .args(["--eval", "require('node:fs').readFileSync(process.argv[1])"])
            .arg(&file)
            .output()
            .unwrap();
        let blocked = !original.status.success();
        let read = (files.read_regular)(
            node_path::from_native(&outside),
            node_path::from_native(&file),
            64,
        )
        .await;
        assert_eq!(read.is_err(), blocked);
        if let Err(error) = read {
            assert_eq!(error.name, "Error");
            assert_eq!(f(&error.to_wire(), "message"), &J::String(error.message));
        }
        drop(denied);
        drop(
            fs::OpenOptions::new()
                .read(true)
                .share_mode(0)
                .open(&file)
                .unwrap(),
        );
    }
    let renamed = outside.join("renamed");
    fs::rename(&file, &renamed).unwrap();
    fs::remove_file(renamed).unwrap();
}
