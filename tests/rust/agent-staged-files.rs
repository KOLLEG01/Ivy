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
    let directory = root.0.join("staged");
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
        ("\u{85} .report. \u{85}", b"x".as_slice(), "report"),
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
    let outside = root.0.join("outside");
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
    native_stage_paths(now).await;
}

async fn native_stage_paths(now: std::time::SystemTime) {
    use std::{ffi::OsString, path::PathBuf, sync::Mutex};

    let root = Root::new();
    let order = Arc::new(Mutex::new(Vec::new()));
    let mut owner = StagedFiles::new(
        node_path::from_native(&root.0.join("invalid\0root")),
        Arc::new({
            let order = order.clone();
            move |value| {
                order.lock().unwrap().push("decode");
                canonical_base64(value)
            }
        }),
    );
    owner.now = Arc::new({
        let order = order.clone();
        move || {
            order.lock().unwrap().push("clock");
            now
        }
    });
    for (encoded, code) in [("!", "invalid_arguments"), ("eA==", "internal_error")] {
        assert_eq!(
            owner
                .stage(&"input".into(), &encoded.into())
                .await
                .unwrap_err()
                .code,
            code
        );
        assert_eq!(
            std::mem::take(&mut *order.lock().unwrap()),
            ["clock", "decode"]
        );
    }
    let blocked = root.0.join("blocked");
    fs::write(&blocked, b"retain blocker").unwrap();
    owner.root = node_path::from_native(&blocked.join("child"));
    assert_eq!(
        owner
            .stage(&"input".into(), &"eA==".into())
            .await
            .unwrap_err()
            .code,
        "internal_error"
    );
    assert_eq!(fs::read(&blocked).unwrap(), b"retain blocker");
    assert_eq!(
        std::mem::take(&mut *order.lock().unwrap()),
        ["clock", "decode"]
    );

    let target = root.0.join("physical");
    fs::create_dir(&target).unwrap();
    let linked = root.0.join("linked");
    filesystem::directory_link(&target, &linked).unwrap();
    let mut trailing = root.0.join("trailing").into_os_string();
    trailing.push(std::path::MAIN_SEPARATOR_STR);
    let mut variants = vec![
        ("absolute", root.0.join("absolute")),
        ("unicode-dot", root.0.join("中-🚀").join(".")),
        (
            "canonical",
            fs::canonicalize(&root.0).unwrap().join("canonical"),
        ),
        ("linked", linked),
        ("trailing", PathBuf::from(trailing)),
    ];
    let cwd = std::env::current_dir().unwrap();
    let mut base = cwd.as_path();
    let mut relative = PathBuf::new();
    loop {
        if root.0.starts_with(base) {
            relative.push(root.0.strip_prefix(base).unwrap());
            relative.push("relative");
            variants.push(("relative", relative));
            break;
        }
        let Some(parent) = base.parent() else { break };
        base = parent;
        relative.push("..");
    }
    #[cfg(windows)]
    {
        variants.push((
            "parent-missing",
            root.0.join("uncreated/../parent-missing"),
        ));
        variants.push(("parent-link", root.0.join("linked/../parent-link")));
        variants.push((
            "opaque-high",
            root.0.join(native_entry("root-high-", 0xd800)),
        ));
        variants.push((
            "opaque-low",
            root.0.join(native_entry("root-low-", 0xdfff)),
        ));
    }
    let mut stored = vec![];
    for (label, input) in variants {
        let alias = PathBuf::from(input.as_os_str().to_string_lossy().as_ref());
        let alias_marker = if alias != input {
            fs::create_dir_all(&alias).unwrap();
            let marker = alias.join("keep");
            fs::write(&marker, b"retain replacement root").unwrap();
            Some(marker)
        } else {
            None
        };
        // The public root remains mutable; every call must decode its current value.
        owner.root = node_path::from_native(&input);
        let bytes = format!("native bytes: {label}").into_bytes();
        let hash = digest(&bytes);
        let expected = node_path::lexical(&input.join(&hash[7..]).join("native.bin"));
        let result = owner
            .stage(&"../native.bin".into(), &STANDARD.encode(&bytes).into())
            .await
            .unwrap();
        validate_agent("StagedFile", &result).unwrap();
        assert_eq!(
            text(&result, "path"),
            node_path::from_native(&expected),
            "{label}"
        );
        assert_eq!(text(&result, "name"), "native.bin".into());
        assert_eq!(field(&result, "contentHash"), &hash.into());
        assert_eq!(
            field(&result, "byteLength"),
            &IvyJson::Number(bytes.len() as f64)
        );
        assert_eq!(
            fs::metadata(expected.parent().unwrap())
                .unwrap()
                .modified()
                .unwrap(),
            now
        );
        assert_eq!(
            std::mem::take(&mut *order.lock().unwrap()),
            ["clock", "decode"]
        );
        stored.push((expected, bytes));
        if let Some(marker) = alias_marker {
            assert_eq!(fs::read(marker).unwrap(), b"retain replacement root");
        }
    }
    for (path, bytes) in stored {
        assert_eq!(fs::read(path).unwrap(), bytes);
    }

    let entries = root.0.join("native-entries");
    fs::create_dir(&entries).unwrap();
    owner.root = node_path::from_native(&entries);
    let mut expired = vec![];
    let mut retained = vec![];
    for (label, unit, kept_time) in [
        ("high", 0xd800, now - RETENTION),
        ("low", 0xdfff, now + Duration::from_secs(1)),
    ] {
        let stale_name = native_entry(&format!("expired-{label}-"), unit);
        let stale = entries.join(&stale_name);
        let alias = entries.join(OsString::from(stale_name.to_string_lossy().as_ref()));
        assert_ne!(stale, alias);
        let keep = entries.join(native_entry(&format!("retained-{label}-"), unit));
        for (path, time) in [
            (&stale, now - RETENTION - Duration::from_millis(1)),
            (&alias, now + Duration::from_secs(1)),
            (&keep, kept_time),
        ] {
            fs::create_dir(path).unwrap();
            fs::write(path.join("keep"), b"native entry").unwrap();
            let ms = chrono::DateTime::<chrono::Utc>::from(time).timestamp_millis();
            filesystem::set_times_ms(path, ms, ms).unwrap();
        }
        expired.push(stale);
        retained.extend([alias, keep]);
    }
    owner.stage(&"expiry".into(), &"eA==".into()).await.unwrap();
    assert_eq!(
        std::mem::take(&mut *order.lock().unwrap()),
        ["clock", "decode"]
    );
    for path in expired {
        assert!(
            !path.exists(),
            "Expired native entry was not removed: {path:?}"
        );
    }
    for path in retained {
        assert_eq!(fs::read(path.join("keep")).unwrap(), b"native entry");
    }
}

fn native_entry(stem: &str, unit: u16) -> std::ffi::OsString {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStringExt;
        let mut value: Vec<_> = stem.encode_utf16().collect();
        value.push(unit);
        std::ffi::OsString::from_wide(&value)
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        let mut value = stem.as_bytes().to_vec();
        value.push(if unit == 0xd800 { 0xff } else { 0xfe });
        std::ffi::OsString::from_vec(value)
    }
}
