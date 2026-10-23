use super::{Root, field, text};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use ivy_agent_manager::{
    runtime::canonical_base64,
    staged_files::{MAXIMUM_BYTES, RETENTION, StagedFiles},
};
use ivy_contracts::{IvyError, IvyJson, JsString, agent_schema::validate_agent, hash::digest};
use ivy_host_runtime::{filesystem, node_path};
use ivy_sdk::service::InvocationContext;
use std::{
    fs,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, UNIX_EPOCH},
};

#[tokio::test]
async fn staged_file_storage_preserves_bytes_expiry_limits_and_native_names() {
    let root = Root::new();
    let directory = root.path.join("staged");
    let now = UNIX_EPOCH + Duration::from_millis(1_792_281_600_000);
    let calls = Arc::new(AtomicUsize::new(0));
    let owner = Arc::new(StagedFiles {
        root: node_path::from_native(&directory),
        decode: Arc::new({
            let calls = calls.clone();
            move |value| {
                calls.fetch_add(1, Ordering::SeqCst);
                canonical_base64(value)
            }
        }),
        now: Arc::new(move || now),
    });
    for (name, bytes, expected_name) in [
        ("../hello.txt", b"x".as_slice(), "hello.txt"),
        ("empty", b"".as_slice(), "empty"),
        ("one/two.txt", b"abc".as_slice(), "two.txt"),
        ("binary", &[0, 255, 128], "binary"),
        ("../CON.", b"x".as_slice(), "_CON"),
        (" \t...", b"x".as_slice(), "_"),
        (" ...", b"x".as_slice(), "file"),
        ("one/x<bad?.txt", b"x".as_slice(), "x_bad_.txt"),
    ] {
        let request = IvyJson::object([
            ("name", name.into()),
            ("dataBase64", STANDARD.encode(bytes).into()),
        ]);
        let context = InvocationContext::detached(
            "caller".into(),
            None,
            1,
            Default::default(),
            IvyError::internal(),
        );
        context.signal.cancel();
        let result = (owner.handler())(request, context).await.unwrap();
        validate_agent("StagedFile", &result).unwrap();
        assert_eq!(text(&result, "name"), expected_name.into());
        let path = node_path::to_native(&text(&result, "path"));
        let hash = digest(bytes);
        assert_eq!(path.parent().unwrap(), directory.join(&hash[7..]));
        assert_eq!(fs::read(&path).unwrap(), bytes);
        assert_eq!(
            field(&result, "byteLength"),
            &IvyJson::Number(bytes.len() as f64)
        );
        assert_eq!(field(&result, "contentHash"), &hash.into());
        assert_eq!(
            owner
                .stage(&name.into(), &STANDARD.encode(bytes).into())
                .await
                .unwrap(),
            result
        );
        assert_eq!(
            fs::metadata(path.parent().unwrap())
                .unwrap()
                .modified()
                .unwrap(),
            now
        );
    }
    // New names obey the cap; an already stored longer name is retained.
    let reserved: JsString = format!("con.{}", "x".repeat(124)).into();
    let content = directory.join(&digest(b"x")[7..]);
    let old_name = format!("_{}", reserved.to_utf8().unwrap());
    fs::write(content.join(&old_name), b"x").unwrap();
    let result = owner.stage(&reserved, &"eA==".into()).await.unwrap();
    assert_eq!(text(&result, "name").0.len(), 128);
    assert_eq!(fs::read(content.join(old_name)).unwrap(), b"x");
    #[cfg(windows)]
    {
        let name: JsString = format!("{}😀", "a".repeat(127)).into();
        let result = owner.stage(&name, &"eA==".into()).await.unwrap();
        assert_eq!(text(&result, "name"), JsString(name.0[..128].to_vec()));
        assert_eq!(
            fs::read(node_path::to_native(&text(&result, "path"))).unwrap(),
            b"x"
        );
    }
    for encoded in [
        "eA", "eA=", "eA===", "eB==", "e A==", "eA==\n", "eA-_", "!!!!",
    ] {
        assert_eq!(
            owner
                .stage(&"bad".into(), &encoded.into())
                .await
                .unwrap_err()
                .code,
            "invalid_arguments"
        );
    }
    assert_eq!(
        owner
            .stage(&"bad".into(), &JsString(vec![0xd800]))
            .await
            .unwrap_err()
            .code,
        "invalid_arguments"
    );
    let maximum = vec![120; MAXIMUM_BYTES];
    let result = owner
        .stage(&"maximum".into(), &STANDARD.encode(&maximum).into())
        .await
        .unwrap();
    assert_eq!(
        fs::read(node_path::to_native(&text(&result, "path"))).unwrap(),
        maximum
    );
    assert_eq!(
        owner
            .stage(
                &"too-large".into(),
                &STANDARD.encode(vec![120; MAXIMUM_BYTES + 1]).into()
            )
            .await
            .unwrap_err()
            .code,
        "invalid_arguments"
    );
    let previous = calls.load(Ordering::SeqCst);
    let context = InvocationContext::detached(
        "caller".into(),
        None,
        1,
        Default::default(),
        IvyError::internal(),
    );
    let invalid = IvyJson::object([
        ("name", "bad".into()),
        ("dataBase64", "eA==".into()),
        ("extra", true.into()),
    ]);
    assert!((owner.handler())(invalid, context).await.is_err());
    assert_eq!(calls.load(Ordering::SeqCst), previous);

    for (name, time) in [
        ("expired", now - RETENTION - Duration::from_millis(1)),
        ("boundary", now - RETENTION),
        ("future", now + Duration::from_secs(1)),
    ] {
        let path = directory.join(name);
        fs::create_dir(&path).unwrap();
        fs::write(path.join("keep"), b"preserve").unwrap();
        let ms = chrono::DateTime::<chrono::Utc>::from(time).timestamp_millis();
        filesystem::set_times_ms(&path, ms, ms).unwrap();
        if name == "expired" {
            let mut permissions = fs::metadata(path.join("keep")).unwrap().permissions();
            permissions.set_readonly(true);
            fs::set_permissions(path.join("keep"), permissions).unwrap();
        }
    }
    fs::write(directory.join("ordinary-file"), b"retain").unwrap();
    let outside = root.path.join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("keep"), b"outside bytes").unwrap();
    filesystem::directory_link(&outside, &directory.join("outside-link")).unwrap();
    owner.stage(&"expiry".into(), &"eA==".into()).await.unwrap();
    assert!(!directory.join("expired").exists());
    for name in ["boundary", "future"] {
        assert_eq!(
            fs::read(directory.join(name).join("keep")).unwrap(),
            b"preserve"
        );
    }
    assert_eq!(
        fs::read(directory.join("ordinary-file")).unwrap(),
        b"retain"
    );
    assert!(
        fs::symlink_metadata(directory.join("outside-link"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(fs::read(outside.join("keep")).unwrap(), b"outside bytes");
}
