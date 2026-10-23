#[path = "host-regular-text-support.rs"]
mod support;
use ivy_contracts::JsString;
use ivy_host_runtime::{filesystem, native_path};
use std::{error::Error, fs};
use support::*;

#[tokio::test]
async fn regular_text_public_realpath_uses_native_names_and_missing_paths() {
    let fixture = Fixture::new();
    let directory = fixture.path("non-ascii ü 空 😀");
    fs::create_dir(&directory).unwrap();
    let file = directory.join("file");
    fs::write(&file, b"text").unwrap();
    for path in [
        fixture.root.clone(),
        file.clone(),
        directory.join("..").join("non-ascii ü 空 😀/file"),
    ] {
        assert_eq!(
            filesystem::realpath(text(&path)).await.unwrap(),
            Some(text(&fs::canonicalize(path).unwrap()))
        );
    }
    for path in [
        fixture.path("missing"),
        file.join("child"),
        fixture.path("missing/child"),
    ] {
        assert!(filesystem::realpath(text(&path)).await.unwrap().is_none());
    }
    assert!(filesystem::realpath("".into()).await.unwrap().is_none());
    let long = directory
        .join("long")
        .join("segment-".repeat(12))
        .join("segment-".repeat(12))
        .join("segment-".repeat(12));
    fs::create_dir_all(&long).unwrap();
    let tail = long.join("tail");
    fs::write(&tail, b"long").unwrap();
    assert_eq!(
        filesystem::realpath(text(&tail)).await.unwrap(),
        Some(text(&fs::canonicalize(&tail).unwrap()))
    );
    #[cfg(windows)]
    assert_eq!(
        filesystem::realpath(text(&fs::canonicalize(&file).unwrap()))
            .await
            .unwrap(),
        Some(text(&fs::canonicalize(&file).unwrap()))
    );
}

#[tokio::test]
async fn regular_text_public_enforces_byte_bounds_and_decodes_utf8() {
    let fixture = Fixture::new();
    let file = fixture.path("input");
    for (bytes, limit) in [
        (Vec::new(), 0),
        (b"line\n".to_vec(), 8),
        (vec![239, 187, 191, 0, 10, 13, 255], 64),
        ((0..=255).collect::<Vec<u8>>(), 1024),
        (
            vec![
                0xf0, 0x9f, 0x98, 0x80, 0xed, 0xa0, 0x80, 0xe2, 0x82, 0xc0, 0xaf, 0xf4, 0x90, 0x80,
                0x80,
            ],
            1024,
        ),
        (b"oversized".to_vec(), 8),
        (vec![b'a'; 4 * 1024 * 1024], 4 * 1024 * 1024),
        (vec![b'a'; 4 * 1024 * 1024 + 1], 4 * 1024 * 1024),
        (vec![b'b'; 8 * 1024 * 1024], 8 * 1024 * 1024),
        (vec![b'b'; 8 * 1024 * 1024 + 1], 8 * 1024 * 1024),
    ] {
        fs::write(&file, &bytes).unwrap();
        let result = filesystem::read_regular(text(&fixture.root), text(&file), limit).await;
        if bytes.len() > limit {
            assert!(result.unwrap_err().to_string().contains("size limit"));
        } else {
            assert_eq!(
                result.unwrap().unwrap(),
                JsString::from(String::from_utf8_lossy(&bytes).as_ref())
            );
        }
    }
}

#[tokio::test]
async fn regular_text_public_requires_a_contained_regular_file() {
    let fixture = Fixture::new();
    fixture.reset();
    let file = fixture.path("input");
    for path in [
        fixture.path("missing"),
        file.join("child"),
        fixture.path("missing/child"),
    ] {
        assert!(
            filesystem::read_regular(text(&fixture.root), text(&path), 1)
                .await
                .unwrap()
                .is_none()
        );
    }
    let child = fixture.path("directory");
    fs::create_dir(&child).unwrap();
    for path in [&fixture.root, &child] {
        assert!(
            filesystem::read_regular(text(&fixture.root), text(path), 8)
                .await
                .is_err()
        );
    }
    assert!(
        filesystem::read_regular(text(&child), text(&file), 8)
            .await
            .is_err()
    );
    let sibling = fixture.path("directory-sibling");
    fs::create_dir(&sibling).unwrap();
    let other = sibling.join("input");
    fs::write(&other, b"retained").unwrap();
    assert!(
        filesystem::read_regular(text(&child), text(&other), 8)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn regular_text_public_rejects_nul_paths_with_native_cause() {
    let fixture = Fixture::new();
    for path in [
        JsString::from("bad\0path"),
        JsString(vec![0xd800, 0, 0xdc00]),
    ] {
        for error in [
            filesystem::realpath(path.clone()).await.unwrap_err(),
            filesystem::read_regular(text(&fixture.root), path, 1)
                .await
                .unwrap_err(),
        ] {
            assert_eq!(
                (error.name, error.code),
                ("TypeError", Some("ERR_INVALID_ARG_VALUE"))
            );
            assert_eq!(
                error
                    .source()
                    .unwrap()
                    .downcast_ref::<std::io::Error>()
                    .unwrap()
                    .kind(),
                std::io::ErrorKind::InvalidInput
            );
        }
    }
}

#[tokio::test]
async fn regular_text_public_handles_hardlinks_sharing_errors_and_release() {
    let fixture = Fixture::new();
    fixture.reset();
    let file = fixture.path("input");
    let hardlink = fixture.path("hardlink");
    fs::hard_link(&file, &hardlink).unwrap();
    assert_eq!(
        filesystem::read_regular(text(&fixture.root), text(&hardlink), 64)
            .await
            .unwrap()
            .unwrap(),
        JsString::from("retained")
    );
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        let denied = fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&file)
            .unwrap();
        let error = filesystem::read_regular(text(&fixture.root), text(&file), 64)
            .await
            .unwrap_err();
        let cause = error
            .source()
            .unwrap()
            .downcast_ref::<std::io::Error>()
            .unwrap();
        assert_eq!(cause.raw_os_error(), Some(32));
        assert_eq!(error.to_string(), cause.to_string());
        drop(denied);
        for limit in [64, 1] {
            let result = filesystem::read_regular(text(&fixture.root), text(&file), limit).await;
            assert_eq!(result.is_ok(), limit == 64);
            // Exclusive reopening proves success and validation failure released the handle.
            drop(
                fs::OpenOptions::new()
                    .read(true)
                    .share_mode(0)
                    .open(&file)
                    .unwrap(),
            );
        }
    }
    #[cfg(unix)]
    {
        let link = fixture.path("link");
        std::os::unix::fs::symlink(&file, &link).unwrap();
        assert!(
            filesystem::read_regular(text(&fixture.root), text(&link), 64)
                .await
                .is_err()
        );
    }
    let renamed = fixture.path("renamed");
    fs::rename(&file, &renamed).unwrap();
    fs::remove_file(&renamed).unwrap();
}
