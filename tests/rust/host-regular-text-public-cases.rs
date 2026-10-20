#[path = "host-regular-text-support.rs"]
mod support;
use ivy_contracts::JsString;
use ivy_host_runtime::{filesystem, filesystem::TextFileError, node_path, paths};
use std::fs;
use support::*;

async fn compare(label: &str, directory: &std::path::Path, path: JsString, limit: usize) {
    let input = request("read", directory, path.clone(), limit);
    pair(
        label,
        input.clone(),
        value(filesystem::read_regular(text(directory), path, limit).await),
        oracle(&input),
    );
}
#[tokio::test]
async fn regular_text_public_realpath_preserves_node_paths_and_missing_errors() {
    let fixture = Fixture::new();
    let directory = fixture.path("non-ascii ü 空 😀");
    fs::create_dir(&directory).unwrap();
    let file = directory.join("file");
    fs::write(&file, b"text").unwrap();
    let mut paths = vec![
        text(&fixture.root),
        text(&file),
        text(&directory.join("..\u{2f}non-ascii ü 空 😀/file")),
        text(&fixture.path("missing")),
        text(&file.join("child")),
        text(&fixture.path("missing/child")),
        JsString::from(""),
        JsString::from("bad\0path"),
    ];
    #[cfg(windows)]
    {
        paths.push(format!("\\\\?\\{}", file.display()).into());
    }
    let long = directory
        .join("long")
        .join("segment-".repeat(12))
        .join("segment-".repeat(12))
        .join("segment-".repeat(12));
    fs::create_dir_all(node_path::node_filesystem_path(&text(&long)).unwrap()).unwrap();
    fs::write(
        node_path::node_filesystem_path(&text(&long.join("tail-.."))).unwrap(),
        b"long",
    )
    .unwrap();
    paths.push(text(&long.join("tail-..")));
    for (i, path) in paths.into_iter().enumerate() {
        let input = request("realpath", &fixture.root, path.clone(), 0);
        pair(
            &format!("realpath-{i}"),
            input.clone(),
            value(filesystem::realpath(path).await),
            oracle(&input),
        );
    }
}

#[tokio::test]
async fn regular_text_public_limits_utf8_and_containment_match_original() {
    let fixture = Fixture::new();
    let file = fixture.path("input");
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
        compare(name, &fixture.root, text(&file), limit).await;
    }
    for path in [
        fixture.path("missing"),
        file.join("child"),
        fixture.path("missing/child"),
    ] {
        compare("missing-only", &fixture.root, text(&path), 1).await;
    }
    compare(
        "directory-nonregular",
        &fixture.root,
        text(&fixture.root),
        8,
    )
    .await;
    let child = fixture.path("directory");
    fs::create_dir(&child).unwrap();
    compare(
        "contained-directory-nonregular",
        &fixture.root,
        text(&child),
        8,
    )
    .await;
    compare("outside-root", &child, text(&file), 8).await;
    let prefix = fixture.path("directory-sibling");
    fs::create_dir(&prefix).unwrap();
    let other = prefix.join("input");
    fs::write(&other, b"retained").unwrap();
    compare("sibling-prefix", &child, text(&other), 8).await;
    let mut values = vec![
        "bad\0path".into(),
        "'\"`\0\n\t\\".into(),
        format!("{}\0\nend", "x".repeat(90)).into(),
        format!("{}\0", "x".repeat(200)).into(),
        JsString(vec![0xd800, 0, 0xdc00]),
    ];
    for n in [0u16, 8, 9, 10, 11, 12, 13, 31, 127, 128, 159] {
        values.push(JsString(vec![97, n, 0, 98]));
    }
    for (i, path) in values.into_iter().enumerate() {
        compare(&format!("nul-diagnostic-{i}"), &fixture.root, path, 1).await;
    }
}

#[tokio::test]
async fn regular_text_public_links_share_denials_and_handles_match_original() {
    let fixture = Fixture::new();
    fixture.reset();
    let link = fixture.path("link");
    #[cfg(windows)]
    let linked = std::os::windows::fs::symlink_file(fixture.path("input"), &link);
    #[cfg(unix)]
    let linked = std::os::unix::fs::symlink(fixture.path("input"), &link);
    match linked {
        Ok(()) => {
            compare("file-link", &fixture.root, text(&link), 64).await;
            fs::remove_file(&link).unwrap();
        }
        Err(error) => blocked("file-link-unavailable", &error),
    }
    let hardlink = fixture.path("hardlink");
    fs::hard_link(fixture.path("input"), &hardlink).unwrap();
    compare("regular-hardlink", &fixture.root, text(&hardlink), 64).await;
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        let denied = fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(fixture.path("input"))
            .unwrap();
        compare(
            "exclusive-sharing-denial",
            &fixture.root,
            text(&fixture.path("input")),
            64,
        )
        .await;
        drop(denied);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let permissions = fs::metadata(fixture.path("input")).unwrap().permissions();
        fs::set_permissions(fixture.path("input"), fs::Permissions::from_mode(0)).unwrap();
        compare(
            "account-permission-observation",
            &fixture.root,
            text(&fixture.path("input")),
            64,
        )
        .await;
        fs::set_permissions(fixture.path("input"), permissions).unwrap();
    }
    // Read, validation error and sharing denial all release the opened resource.
    let old = fixture.path("renamed");
    fs::rename(fixture.path("input"), &old).unwrap();
    fs::remove_file(old).unwrap();
}
