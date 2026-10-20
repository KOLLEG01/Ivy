use super::*;
use ivy_agent_manager::claude::workflow::WorkflowFiles;

fn projected(original: &J) -> J {
    if f(original, "ok").as_bool() == Some(true) {
        J::object([("ok", true.into()), ("value", f(original, "value").clone())])
    } else {
        J::object([
            ("ok", false.into()),
            ("name", f(original, "name").clone()),
            ("message", f(original, "message").clone()),
            (
                "wire",
                J::object([
                    ("code", J::Number(-32000.0)),
                    ("message", f(original, "message").clone()),
                ]),
            ),
        ])
    }
}
fn value(result: Result<Option<JsString>>) -> J {
    match result {
        Ok(result) => J::object([
            ("ok", true.into()),
            ("value", result.map(J::String).unwrap_or(J::Null)),
        ]),
        Err(error) => J::object([
            ("ok", false.into()),
            ("name", error.name.into()),
            ("message", J::String(error.message.clone())),
            ("wire", error.to_wire()),
        ]),
    }
}
async fn pair(label: &str, operation: &str, directory: &Path, path: JsString, limit: usize) {
    let input = J::object([
        ("operation", operation.into()),
        ("directory", J::String(node_path::from_native(directory))),
        ("path", J::String(path.clone())),
        ("limit", J::Number(limit as f64)),
    ]);
    let mut child = Command::new("node")
        .arg("tests/fixtures/host-regular-text-oracle.mjs")
        .current_dir(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.."))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stringify(&input, usize::MAX).unwrap().as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let original = parse(std::str::from_utf8(&output.stdout).unwrap(), usize::MAX).unwrap();
    let files = WorkflowFiles::host();
    let actual = value(if operation == "realpath" {
        (files.realpath)(path).await
    } else {
        (files.read_regular)(node_path::from_native(directory), path, limit).await
    });
    // AdapterError intentionally exposes name/message, while the original I/O
    // also has an errno code. Keep the complete original observation in proof.
    let expected = projected(&original);
    if let Some(path) = std::env::var_os("IVY_WORKFLOW_FILES_PROOF") {
        writeln!(
            fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .unwrap(),
            "{}",
            stringify(
                &J::object([
                    ("label", label.into()),
                    ("input", input),
                    ("actual", actual.clone()),
                    ("original", original),
                    ("expected", expected.clone()),
                ]),
                usize::MAX
            )
            .unwrap()
        )
        .unwrap();
    }
    assert_eq!(
        canonical(&actual, usize::MAX).unwrap(),
        canonical(&expected, usize::MAX).unwrap(),
        "{label}"
    );
}

#[tokio::test]
async fn host_workflow_realpaths_keep_node_spelling_and_missing_errors() {
    let root = Root::new();
    let directory = root.0.join("non-ascii ü 空 😀");
    fs::create_dir(&directory).unwrap();
    let file = directory.join("file");
    fs::write(&file, b"text").unwrap();
    let mut paths = vec![
        node_path::from_native(&root.0),
        node_path::from_native(&file),
        node_path::from_native(&directory.join("../non-ascii ü 空 😀/file")),
        node_path::from_native(&root.0.join("missing")),
        node_path::from_native(&file.join("child")),
        node_path::from_native(&root.0.join("missing/child")),
        "".into(),
        "bad\0path".into(),
    ];
    #[cfg(windows)]
    paths.push(format!("\\\\?\\{}", file.display()).into());
    let long = directory
        .join("segment-".repeat(12))
        .join("segment-".repeat(12))
        .join("segment-".repeat(12));
    fs::create_dir_all(node_path::node_filesystem_path(&node_path::from_native(&long)).unwrap())
        .unwrap();
    let tail = long.join("tail-..");
    fs::write(
        node_path::node_filesystem_path(&node_path::from_native(&tail)).unwrap(),
        b"long",
    )
    .unwrap();
    paths.push(node_path::from_native(&tail));
    for (i, path) in paths.into_iter().enumerate() {
        pair(&format!("realpath-{i}"), "realpath", &root.0, path, 0).await;
    }
}

#[tokio::test]
async fn host_workflow_reads_preserve_bounds_bytes_containment_and_utf16_errors() {
    let root = Root::new();
    let file = root.0.join("input");
    for (name, bytes, limit) in [
        ("empty", Vec::new(), 0),
        ("ordinary", b"line\n".to_vec(), 8),
        ("bom-controls", vec![239, 187, 191, 0, 10, 13, 255], 64),
        ("invalid-utf8", (0..=255).collect::<Vec<u8>>(), 1024),
        (
            "utf8-boundaries",
            vec![
                0xf0, 0x9f, 0x98, 0x80, 0xed, 0xa0, 0x80, 0xe2, 0x82, 0xc0, 0xaf, 0xf4, 0x90, 0x80,
                0x80,
            ],
            1024,
        ),
        ("small-limit", b"oversized".to_vec(), 8),
        (
            "transcript-at-limit",
            vec![b'a'; 4 * 1024 * 1024],
            4 * 1024 * 1024,
        ),
        (
            "transcript-over-limit",
            vec![b'a'; 4 * 1024 * 1024 + 1],
            4 * 1024 * 1024,
        ),
        (
            "journal-at-limit",
            vec![b'b'; 8 * 1024 * 1024],
            8 * 1024 * 1024,
        ),
        (
            "journal-over-limit",
            vec![b'b'; 8 * 1024 * 1024 + 1],
            8 * 1024 * 1024,
        ),
    ] {
        fs::write(&file, bytes).unwrap();
        pair(name, "read", &root.0, node_path::from_native(&file), limit).await;
    }
    for (i, path) in [
        root.0.join("missing"),
        file.join("child"),
        root.0.join("missing/child"),
    ]
    .into_iter()
    .enumerate()
    {
        pair(
            &format!("missing-{i}"),
            "read",
            &root.0,
            node_path::from_native(&path),
            1,
        )
        .await;
    }
    pair(
        "directory-itself",
        "read",
        &root.0,
        node_path::from_native(&root.0),
        8,
    )
    .await;
    let directory = root.0.join("directory");
    fs::create_dir(&directory).unwrap();
    pair(
        "contained-nonregular",
        "read",
        &root.0,
        node_path::from_native(&directory),
        8,
    )
    .await;
    pair(
        "outside-root",
        "read",
        &directory,
        node_path::from_native(&file),
        8,
    )
    .await;
    let sibling = root.0.join("directory-sibling");
    fs::create_dir(&sibling).unwrap();
    fs::write(sibling.join("input"), b"retained").unwrap();
    pair(
        "sibling-prefix",
        "read",
        &directory,
        node_path::from_native(&sibling.join("input")),
        8,
    )
    .await;
    let mut paths = vec![
        "bad\0path".into(),
        "'\"`\0\n\t\\".into(),
        format!("{}\0\nend", "x".repeat(90)).into(),
        format!("{}\0", "x".repeat(200)).into(),
        JsString(vec![0xd800, 0, 0xdc00]),
    ];
    for n in [0u16, 8, 9, 10, 11, 12, 13, 31, 127, 128, 159] {
        paths.push(JsString(vec![97, n, 0, 98]));
    }
    for (i, path) in paths.into_iter().enumerate() {
        pair(&format!("nul-diagnostic-{i}"), "read", &root.0, path, 1).await;
    }
    // A validation failure also closes the opened handle.
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        drop(
            fs::OpenOptions::new()
                .read(true)
                .share_mode(0)
                .open(&file)
                .unwrap(),
        );
    }
}

#[tokio::test]
async fn host_workflow_links_and_sharing_keep_original_errors_and_release_handles() {
    let root = Root::new();
    let file = root.0.join("input");
    fs::write(&file, b"retained").unwrap();
    let link = root.0.join("link");
    #[cfg(windows)]
    let linked = std::os::windows::fs::symlink_file(&file, &link);
    #[cfg(unix)]
    let linked = std::os::unix::fs::symlink(&file, &link);
    match linked {
        Ok(()) => {
            pair(
                "file-symlink",
                "read",
                &root.0,
                node_path::from_native(&link),
                64,
            )
            .await;
            fs::remove_file(link).unwrap();
        }
        Err(error) => {
            if let Some(path) = std::env::var_os("IVY_WORKFLOW_FILES_CAPABILITY_PROOF") {
                writeln!(
                    fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(path)
                        .unwrap(),
                    "{}",
                    stringify(
                        &J::object([
                            ("label", "file-symlink".into()),
                            ("execution", "blocked".into()),
                            ("finding", J::Null),
                            (
                                "osError",
                                error
                                    .raw_os_error()
                                    .map(|n| J::Number(n as f64))
                                    .unwrap_or(J::Null)
                            ),
                            ("message", error.to_string().into()),
                        ]),
                        usize::MAX
                    )
                    .unwrap()
                )
                .unwrap();
            }
        }
    }
    let hardlink = root.0.join("hardlink");
    fs::hard_link(&file, &hardlink).unwrap();
    pair(
        "regular-hardlink",
        "read",
        &root.0,
        node_path::from_native(&hardlink),
        64,
    )
    .await;
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        let denied = fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&file)
            .unwrap();
        pair(
            "exclusive-sharing-denial",
            "read",
            &root.0,
            node_path::from_native(&file),
            64,
        )
        .await;
        drop(denied);
    }
    let renamed = root.0.join("renamed");
    fs::rename(&file, &renamed).unwrap();
    fs::remove_file(renamed).unwrap();
}
