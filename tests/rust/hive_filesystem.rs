use super::*;
use ivy_contracts::hash::digest;
use ivy_host_runtime::{filesystem, native_path};
use std::{ffi::OsString, os::windows::ffi::OsStringExt};

fn opaque_name(prefix: &str, unit: u16) -> OsString {
    OsString::from_wide(&prefix.encode_utf16().chain([unit]).collect::<Vec<_>>())
}
fn native_root(unit: u16) -> (Root, PathBuf, PathBuf) {
    let root = Root::new();
    let name = opaque_name("native-é-", unit);
    let base = fs::canonicalize(&root.0).unwrap();
    let path = base.join(&name);
    let alias = base.join(name.to_string_lossy().as_ref());
    assert_ne!(path, alias);
    fs::create_dir(&path).unwrap();
    fs::create_dir(&alias).unwrap();
    fs::write(alias.join("sentinel"), b"preserved alias").unwrap();
    (root, path, alias)
}
fn descriptor(path: &str, media: &str, bytes: &[u8]) -> IvyJson {
    IvyJson::object([
        ("path", path.into()),
        ("mediaType", media.into()),
        ("contentHash", digest(bytes).into()),
        ("byteLength", IvyJson::Number(bytes.len() as f64)),
    ])
}

#[tokio::test]
async fn native_ui_windows_namespace_preserves_actual_staged_promoted_read_and_collected_asset_names()
 {
    let (_root, path, alias) = native_root(0xd800);
    let files = UiFiles::new(path.join("private"));
    let ui = JsString::from("sample-ui");
    let release = JsString::from("r1");
    let names = ["tail..", "space. ", "folder. /nested..", "é. ", "💡.."];
    let assets: Vec<_> = names
        .iter()
        .enumerate()
        .map(|(i, name)| descriptor(name, "text/plain", &[i as u8, 0, 255]))
        .collect();
    for (i, asset) in assets.iter().enumerate() {
        files
            .stage(&ui, &release, asset, &[i as u8, 0, 255])
            .unwrap();
        files
            .stage(&ui, &release, asset, &[i as u8, 0, 255])
            .unwrap();
    }
    let changed = descriptor(names[0], "text/plain", b"changed");
    assert_eq!(
        files
            .stage(&ui, &release, &changed, b"changed")
            .unwrap_err(),
        IvyError::new(
            "mutation_conflict",
            "Staged UI file already has other content."
        )
    );
    files.promote(&ui, &release, &assets).unwrap();
    files.promote(&ui, &release, &assets).unwrap();
    for (i, asset) in assets.iter().enumerate() {
        assert!(files.available(&ui, &release, asset));
        assert!(files.check(&ui, &release, asset).unwrap());
        let file = files
            .path(
                &ui,
                &release,
                asset.required("path").unwrap().as_js_string().unwrap(),
            )
            .unwrap();
        assert_eq!(fs::read(&file).unwrap(), [i as u8, 0, 255]);
        assert_eq!(
            files.read(&ui, &release, asset).await.unwrap(),
            [i as u8, 0, 255]
        );
    }
    let directory = files.directory(&ui, &release).unwrap();
    filesystem::set_times_ms(&directory, 0, 0).unwrap();
    files.collect(&ui, &HashSet::new(), 172800000.0).unwrap();
    assert!(!directory.exists());
    assert_eq!(
        fs::read(alias.join("sentinel")).unwrap(),
        b"preserved alias"
    );
}

#[tokio::test]
async fn native_ui_windows_namespace_installs_actual_bundle_files_and_cleans_failed_staging_before_retry()
 {
    let (_root, path, alias) = native_root(0xdfff);
    let source = path.join("bundle");
    fs::create_dir_all(source.join("folder. ")).unwrap();
    fs::write(source.join("tail.."), b"one").unwrap();
    fs::write(source.join("folder. /entry. "), br#"{ "z": 2, "a": 1 }"#).unwrap();
    let files = UiFiles::new(path.join("private"));
    let ui = JsString::from("sample-ui");
    let assets = [
        descriptor("tail..", "text/plain", b"one"),
        descriptor("folder. /entry. ", "application/json", br#"{"a":1,"z":2}"#),
    ];
    files
        .install(&source, &ui, &"r1".into(), &assets)
        .await
        .unwrap();
    assert_eq!(
        files.read(&ui, &"r1".into(), &assets[1]).await.unwrap(),
        br#"{"a":1,"z":2}"#
    );
    fs::remove_file(source.join("tail..")).unwrap();
    assert_eq!(
        files
            .install(&source, &ui, &"r2".into(), &assets)
            .await
            .unwrap_err(),
        IvyError::internal()
    );
    assert!(!files.directory(&ui, &"r2".into()).unwrap().exists());
    assert!(
        fs::read_dir(files.root.join(".incoming"))
            .unwrap()
            .next()
            .is_none()
    );
    fs::write(source.join("tail.."), b"one").unwrap();
    files
        .install(&source, &ui, &"r2".into(), &assets)
        .await
        .unwrap();
    assert_eq!(
        files.read(&ui, &"r2".into(), &assets[0]).await.unwrap(),
        b"one"
    );
    assert_eq!(
        fs::read(alias.join("sentinel")).unwrap(),
        b"preserved alias"
    );
}

#[tokio::test]
async fn native_ui_windows_namespace_root_keeps_logical_paths_without_creating_trimmed_aliases() {
    let (_root, parent, alias) = native_root(0xd800);
    let path = parent.join("native. ");
    let files = UiFiles::new(path.join("private"));
    let asset = descriptor("tail..", "text/plain", b"one");
    files
        .stage(&"sample-ui".into(), &"r1".into(), &asset, b"one")
        .unwrap();
    files
        .promote(&"sample-ui".into(), &"r1".into(), &[asset.clone()])
        .unwrap();
    assert_eq!(
        files
            .read(&"sample-ui".into(), &"r1".into(), &asset)
            .await
            .unwrap(),
        b"one"
    );
    let mut store = Store::open(Path::new(":memory:")).unwrap();
    files.migrate_legacy(&mut store).unwrap();
    assert_eq!(
        store.metadata("ui_files_v1").unwrap().as_deref(),
        Some("ready")
    );
    assert!(path.exists());
    assert!(!parent.join("native").exists());
    assert_eq!(
        fs::read(alias.join("sentinel")).unwrap(),
        b"preserved alias"
    );
}

#[tokio::test]
async fn native_sqlite_paths_keep_fresh_snapshot_ownership_and_refuse_replacement_aliases() {
    use ivy_hive::storage_read::{inspect_stopped_storage, snapshot_stopped_hive};
    use rusqlite::Connection;
    let root = Root::new();
    let source = root.0.join("storage-é-中/nested/source.sqlite");
    let store = Store::open(&source).unwrap();
    store.connection().execute_batch("CREATE TABLE native_payload(value BLOB NOT NULL); INSERT INTO native_payload VALUES(x'00FF0120');").unwrap();
    let destination = root.0.join("online-é-中.sqlite");
    let snapshot = store
        .backup_to(native_path::from_native(&destination), || Ok(()))
        .await
        .unwrap();
    assert_eq!(snapshot.bytes_hash, digest(fs::read(&destination).unwrap()));
    let copy = Connection::open(&destination).unwrap();
    assert_eq!(
        copy.query_row("SELECT value FROM native_payload", [], |r| r
            .get::<_, Vec<u8>>(0))
            .unwrap(),
        [0, 255, 1, 32]
    );
    drop(copy);
    for unit in [0xd800, 0xdfff] {
        for parent in [false, true] {
            let name = opaque_name(&format!("bad-{parent}-{unit:x}-"), unit);
            let (bad, alias) = if parent {
                let bad = root.0.join(&name);
                let alias = root.0.join(name.to_string_lossy().as_ref());
                fs::create_dir(&bad).unwrap();
                fs::create_dir(&alias).unwrap();
                (bad.join("snapshot.sqlite"), alias.join("snapshot.sqlite"))
            } else {
                (
                    root.0.join(&name),
                    root.0.join(name.to_string_lossy().as_ref()),
                )
            };
            let retained = Connection::open(&alias).unwrap();
            retained
                .execute_batch(
                    "CREATE TABLE sentinel(value TEXT); INSERT INTO sentinel VALUES('preserved');",
                )
                .unwrap();
            drop(retained);
            let before = fs::read(&alias).unwrap();
            let error = store
                .backup_to(native_path::from_native(&bad), || Ok(()))
                .await
                .err()
                .expect("SQLite must refuse unpaired filenames");
            assert_eq!(error, IvyError::internal());
            assert!(!bad.exists());
            assert_eq!(fs::read(&alias).unwrap(), before);
        }
    }
    store.close().unwrap();
    let before = fs::read(&source).unwrap();
    let missing = root.0.join("missing/no.sqlite");
    assert_eq!(
        inspect_stopped_storage(&missing, &[])
            .unwrap()
            .get("exists"),
        Some(&false.into())
    );
    assert!(!missing.parent().unwrap().exists());
    assert_eq!(
        inspect_stopped_storage(&source, &[]).unwrap().get("exists"),
        Some(&true.into())
    );
    let target = root.0.join("stopped-é-中.sqlite");
    snapshot_stopped_hive(&source, &target).unwrap();
    let copied = fs::read(&target).unwrap();
    assert_eq!(
        snapshot_stopped_hive(&source, &target).unwrap_err(),
        IvyError::new(
            "target_conflict",
            "Offline Hive snapshot needs an existing source and a fresh destination."
        )
    );
    assert_eq!(fs::read(&target).unwrap(), copied);
    let corrupt = root.0.join("corrupt.sqlite");
    fs::write(&corrupt, b"not a database").unwrap();
    let refused = root.0.join("refused.sqlite");
    assert!(snapshot_stopped_hive(&corrupt, &refused).is_err());
    assert!(!refused.exists());
    assert_eq!(fs::read(&source).unwrap(), before);
}
