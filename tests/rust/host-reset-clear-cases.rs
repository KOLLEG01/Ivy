use super::*;
#[allow(dead_code)]
#[path = "host-service-fixture.rs"]
mod fixture;
use fixture::{ServiceFixture, host_configuration};
use ivy_contracts::{
    hash::{digest, hash_json},
    json::{canonical, parse},
};
use ivy_hive::store::Store;
use ivy_host_runtime::host_config::HostContext;
use std::{
    fs,
    sync::atomic::{AtomicUsize, Ordering},
    time::Instant,
};

const DATE: &str = "2026-01-02T03:04:05.006Z";
const MILLIS: i64 = 1_767_323_045_006;
struct Fixed;
impl Clock for Fixed {
    fn utc_millis(&self) -> i64 {
        MILLIS
    }
    fn monotonic(&self) -> Instant {
        Instant::now()
    }
}
struct FailThird(AtomicUsize);
impl Clock for FailThird {
    fn utc_millis(&self) -> i64 {
        if self.0.fetch_add(1, Ordering::SeqCst) == 2 {
            i64::MAX
        } else {
            MILLIS
        }
    }
    fn monotonic(&self) -> Instant {
        Instant::now()
    }
}
fn text(path: impl AsRef<std::path::Path>) -> J {
    J::String(node_path::from_native(path.as_ref()))
}
fn encoded(value: &J) -> String {
    canonical(value, usize::MAX).unwrap()
}
fn assert_json(actual: &J, expected: &J) {
    assert_eq!(encoded(actual), encoded(expected));
}
fn storage() -> Arc<StorageContext> {
    let account = HostContext::current().unwrap();
    Arc::new(StorageContext {
        paths: account.paths,
        account_home: account.account_home,
        compare: Arc::new(|a, b| a.0.cmp(&b.0)),
    }) // Explicit fixture order only.
}
fn owner(f: &ServiceFixture, clock: Arc<dyn Clock>) -> ResetClear {
    ResetClear::new(
        f.0.join("unused-package"),
        f.0.join("unused-job"),
        storage(),
        clock,
    )
    .unwrap()
}
fn host(f: &ServiceFixture) -> J {
    assert!(
        f.0.canonicalize()
            .unwrap()
            .starts_with(std::env::temp_dir().canonicalize().unwrap())
    );
    let mut host = host_configuration(f);
    host.insert("servicesRoot", text(f.0.join("services")));
    let mut instances = host
        .required("instances")
        .unwrap()
        .as_array()
        .unwrap()
        .to_vec();
    let mut hive = instances[0].clone();
    hive.insert("instanceId", "hive-fixture".into());
    hive.insert("componentId", "hive".into());
    instances.push(hive);
    host.insert("instances", J::Array(instances));
    host
}
fn input() -> J {
    parse(r###"{"op":"rebootstrap","epoch":"epoch-selected","values":[{"family":{"key":"agent/instructions","owner":"{ \"kind\" : \"hive\" }","mediaType":"text/markdown","objectMode":"retain"},"contract":{"version":"1.2.3","definition":"{ \"raw\" : \"definition\", \"line\":\"\\u0061\" }"},"object":{"id":"kept","parentId":null,"ownerObjectId":null,"name":"Instruction kept","path":"/Instruction kept","position":7,"icon":"🦉","depth":1,"currentRevision":4,"createdAt":"created raw","updatedAt":"updated raw"},"revision":{"contractVersion":"1.2.3","encoding":"text","content":"  # Résumé\r\nkeep\u0000raw 🦉  ","contentHash":"raw-content-hash","byteLength":31,"createdAt":"revision raw","referencesJson":"{}"}},{"family":{"key":"agent/instructions","owner":"{ \"kind\" : \"hive\" }","mediaType":"text/markdown","objectMode":"retain"},"contract":{"version":"1.2.3","definition":"{ \"raw\" : \"definition\", \"line\":\"\\u0061\" }"},"object":{"id":"second","parentId":null,"ownerObjectId":null,"name":"Instruction second","path":"/Instruction second","position":7,"icon":"🦉","depth":1,"currentRevision":4,"createdAt":"created raw","updatedAt":"updated raw"},"revision":{"contractVersion":"1.2.3","encoding":"text","content":"  # Résumé\r\nkeep\u0000raw 🦉  ","contentHash":"raw-content-hash","byteLength":31,"createdAt":"revision raw","referencesJson":"{}"}}],"ids":["scope-b","scope-a","scope-a"],"definitions":[{"key":"service/explicit","version":"1.0.0","owner":{"kind":"service","serviceName":"example"},"mediaType":"application/json","retention":{"objects":{"mode":"retain"},"revisions":{"mode":"current"}},"specMarkdown":"Explicit reset bootstrap.","jsonSchema":{"type":"object"}}],"at":"2026-01-02T03:04:05.006Z"}"###, usize::MAX).unwrap()
}
fn hive_path(f: &ServiceFixture) -> PathBuf {
    f.0.join("services/hive/data/hive.sqlite")
}
fn entry(f: &ServiceFixture, area: &str, member: &str, relative: &str) -> J {
    J::object([
        ("instanceId", "hive-fixture".into()),
        ("componentId", "hive".into()),
        ("area", area.into()),
        ("path", member.into()),
        ("absolutePath", text(f.0.join(relative))),
    ])
}
async fn prepared(f: &ServiceFixture, config: &J, with_saved: bool) -> J {
    let data = input();
    fs::create_dir_all(hive_path(f).parent().unwrap()).unwrap();
    hive_reset_storage()
        .rebootstrap(ResetBootstrap {
            path: hive_path(f),
            epoch: "old-epoch".into(),
            instructions: data.required("values").unwrap().clone(),
            scope_roots: vec![],
            service_contracts: vec![],
            now: Arc::new(|| Ok(DATE.into())),
        })
        .await
        .unwrap();
    let work = f.0.join("services/hive/work");
    fs::create_dir_all(&work).unwrap();
    fs::write(work.join("clear.bin"), b"discard").unwrap();
    let keep = f.0.join("services/hive/data/whatsapp");
    fs::create_dir_all(&keep).unwrap();
    fs::write(keep.join("opaque.bin"), b"\0exact-auth\xff").unwrap();
    let mut preserve = entry(f, "data", "whatsapp", "services/hive/data/whatsapp");
    preserve.insert(
        "checksum",
        preserved_reset_tree_hash(field(&preserve, "absolutePath").unwrap(), storage())
            .await
            .unwrap(),
    );
    let plan = J::object([
        ("scopeRootIds", data.required("ids").unwrap().clone()),
        (
            "delete",
            J::Array(vec![
                entry(f, "data", "hive.sqlite", "services/hive/data/hive.sqlite"),
                entry(
                    f,
                    "data",
                    "hive.sqlite-wal",
                    "services/hive/data/hive.sqlite-wal",
                ),
                entry(
                    f,
                    "data",
                    "hive.sqlite-shm",
                    "services/hive/data/hive.sqlite-shm",
                ),
                entry(f, "work", ".", "services/hive/work"),
            ]),
        ),
        (
            "completionMarkers",
            J::Array(vec![J::object([(
                "absolutePath",
                text(f.0.join("services/hive/data/reset-completed.json")),
            )])]),
        ),
        ("preserve", J::Array(vec![preserve])),
    ]);
    let saved = if with_saved {
        let hash: J = format!("sha256:{}", "a".repeat(64)).into();
        let candidate = J::object([
            ("candidateId", hash.clone()),
            ("componentId", "fixture".into()),
            ("artifactRoot", text(f.0.join("artifact"))),
            ("manifestPath", "manifest.json".into()),
            ("createdAt", DATE.into()),
            ("buildId", hash.clone()),
            (
                "platform",
                J::object([
                    ("os", "win32".into()),
                    ("arch", "x64".into()),
                    ("node", "24.18.0".into()),
                ]),
            ),
        ]);
        let manifest = J::object([
            ("schemaVersion", J::Number(1.0)),
            ("componentId", "fixture".into()),
            ("kind", "app".into()),
            ("version", "1.0.0".into()),
            ("buildId", hash),
            ("connectsToHive", false.into()),
            (
                "requirements",
                J::object([
                    ("node", ">=24.18.0 <25.0.0".into()),
                    ("hiveProtocol", J::Null),
                    ("contracts", J::Array(vec![])),
                ]),
            ),
            (
                "app",
                J::object([
                    ("appId", "fixture".into()),
                    ("dist", "dist/apps/fixture".into()),
                    ("entryPath", "index.html".into()),
                ]),
            ),
        ]);
        vec![J::object([
            ("instanceId", "fixture".into()),
            ("candidate", candidate),
            ("manifest", manifest),
            ("enabled", true.into()),
        ])]
    } else {
        vec![]
    };
    let record = J::object([
        ("schemaVersion", J::Number(1.0)),
        ("deploymentIdentity", "fixture-install".into()),
        ("resetId", "reset".into()),
        ("runtimeEpoch", "new-epoch".into()),
        ("hostId", config.required("hostId").unwrap().clone()),
        ("phase", "prepared".into()),
        ("updatedAt", DATE.into()),
        ("planHash", hash_json(&plan).unwrap().into()),
        ("plan", plan),
        ("saved", J::Array(saved)),
        ("instructions", J::Array(vec![])),
        ("instructionsCaptured", false.into()),
        ("deploymentComplete", false.into()),
        (
            "publisherCredentialDigest",
            format!("sha256:{}", "b".repeat(64)).into(),
        ),
    ]);
    config::atomic_json(reset_record_path(config).unwrap(), record.clone())
        .await
        .unwrap();
    config::atomic_json(
        runtime_maintenance::runtime_reset_marker(config).unwrap(),
        J::object([("held", true.into())]),
    )
    .await
    .unwrap();
    record
}
async fn record(config: &J) -> J {
    config::json_file(
        reset_record_path(config).unwrap(),
        config::CONFIGURATION_BYTES,
    )
    .await
    .unwrap()
}
fn db_closed(f: &ServiceFixture) {
    let db = Store::open(&hive_path(f)).unwrap();
    db.close().unwrap();
}

async fn check_preserved_tree_hashes(f: &ServiceFixture) {
    let root = f.0.join("preserved-tree 中 🚀");
    fs::create_dir_all(root.join("a-dir")).unwrap();
    fs::create_dir(root.join("empty")).unwrap();
    let mut files: Vec<(PathBuf, &[u8])> = vec![
        ("a-dir/leaf.bin".into(), b"abc"),
        ("z.bin".into(), b"\0\xff"),
        ("é.txt".into(), b"accent"),
        ("中.bin".into(), b"middle"),
        ("🚀.bin".into(), b"supplementary"),
    ];
    #[cfg(windows)]
    {
        let mut opaque = JsString::from("opaque-");
        opaque.0.push(0xd800);
        opaque.0.extend(JsString::from(".bin").0);
        files.insert(
            1,
            (node_path::node_filesystem_path(&opaque).unwrap(), b"opaque"),
        );
    }
    for (name, bytes) in &files {
        fs::write(root.join(name), bytes).unwrap();
    }
    let row = |path: JsString, bytes: &[u8]| {
        J::object([("path", J::String(path)), ("hash", digest(bytes).into())])
    };
    let rows: Vec<J> = files
        .iter()
        .map(|(name, bytes)| {
            let path = if name == std::path::Path::new("a-dir/leaf.bin") {
                JsString::from("a-dir/leaf.bin")
            } else {
                node_path::from_native(name)
            };
            row(path, bytes)
        })
        .collect();
    let expected: J = hash_json(&J::Array(rows.clone())).unwrap().into();
    assert_json(
        &preserved_reset_tree_hash(node_path::from_native(&root), storage())
            .await
            .unwrap(),
        &expected,
    );
    assert_json(
        &preserved_reset_tree_hash(node_path::from_native(&root.join("a-dir/..")), storage())
            .await
            .unwrap(),
        &expected,
    );
    let account = HostContext::current().unwrap();
    let reverse = Arc::new(StorageContext {
        paths: account.paths,
        account_home: account.account_home,
        compare: Arc::new(|a, b| b.0.cmp(&a.0)),
    });
    let mut reversed = rows;
    reversed.reverse();
    assert_json(
        &preserved_reset_tree_hash(node_path::from_native(&root), reverse)
            .await
            .unwrap(),
        &hash_json(&J::Array(reversed)).unwrap().into(),
    );
    assert_json(
        &preserved_reset_tree_hash(
            node_path::from_native(&root.join("a-dir/leaf.bin")),
            storage(),
        )
        .await
        .unwrap(),
        &hash_json(&J::Array(vec![row("".into(), b"abc")]))
            .unwrap()
            .into(),
    );
    assert_json(
        &preserved_reset_tree_hash(node_path::from_native(&root.join("empty")), storage())
            .await
            .unwrap(),
        &hash_json(&J::Array(vec![])).unwrap().into(),
    );
    assert_eq!(
        preserved_reset_tree_hash(node_path::from_native(&root.join("absent")), storage())
            .await
            .unwrap(),
        J::Null,
    );
    let root_link = f.0.join("preserved-root-link");
    filesystem::directory_link(&root, &root_link).unwrap();
    let error = preserved_reset_tree_hash(node_path::from_native(&root_link), storage())
        .await
        .unwrap_err();
    assert_eq!(error.code, "target_conflict");
    assert_eq!(
        error.message,
        "Preserved reset paths cannot traverse links."
    );
    let with_link = f.0.join("preserved-child-link");
    fs::create_dir(&with_link).unwrap();
    filesystem::directory_link(&root, &with_link.join("child")).unwrap();
    let error = preserved_reset_tree_hash(node_path::from_native(&with_link), storage())
        .await
        .unwrap_err();
    assert_eq!(error.code, "target_conflict");
    assert_eq!(error.message, "Preserved reset paths cannot contain links.");
    #[cfg(not(windows))]
    {
        use std::os::unix::ffi::OsStringExt;
        let invalid = f.0.join("unrepresentable-tree");
        fs::create_dir(&invalid).unwrap();
        fs::write(
            invalid.join(std::ffi::OsString::from_vec(vec![0xff])),
            b"raw",
        )
        .unwrap();
        assert_eq!(
            preserved_reset_tree_hash(node_path::from_native(&invalid), storage())
                .await
                .unwrap_err()
                .code,
            "internal_error",
        );
    }
    for (name, bytes) in files {
        assert_eq!(fs::read(root.join(name)).unwrap(), bytes);
    }
}

#[tokio::test]
async fn clear_captures_raw_hive_before_delete_and_restores_saved_target_under_hold() {
    let f = ServiceFixture::new();
    check_preserved_tree_hashes(&f).await;
    let config = host(&f);
    let original = prepared(&f, &config, true).await;
    let data = input();
    let actual = owner(&f, Arc::new(Fixed))
        .clear_admitted(
            config.clone(),
            "reset".into(),
            "new-epoch".into(),
            data.required("definitions")
                .unwrap()
                .as_array()
                .unwrap()
                .to_vec(),
        )
        .await
        .unwrap();
    assert_eq!(actual.required("phase").unwrap(), &J::from("cleared"));
    assert_json(
        actual.required("instructions").unwrap(),
        data.required("values").unwrap(),
    );
    assert_json(
        actual.required("plan").unwrap(),
        original.required("plan").unwrap(),
    );
    assert_json(&record(&config).await, &actual);
    assert!(
        runtime_maintenance::runtime_reset_marker(&config)
            .unwrap()
            .is_file()
    );
    assert!(f.0.join("services/hive/work").is_dir());
    assert!(!f.0.join("services/hive/work/clear.bin").exists());
    assert_eq!(
        fs::read(f.0.join("services/hive/data/whatsapp/opaque.bin")).unwrap(),
        b"\0exact-auth\xff"
    );
    assert_json(
        &hive_reset_storage()
            .saved_instructions(hive_path(&f))
            .await
            .unwrap(),
        data.required("values").unwrap(),
    );
    let store = Store::open(&hive_path(&f)).unwrap();
    assert_eq!(
        store.metadata("runtime_epoch").unwrap().unwrap(),
        "new-epoch"
    );
    assert_eq!(
        store
            .connection()
            .query_row::<i64, _, _>(
                "SELECT count(*) FROM objects WHERE id IN ('scope-a','scope-b')",
                [],
                |r| r.get(0)
            )
            .unwrap(),
        2
    );
    assert_eq!(
        store
            .connection()
            .query_row::<i64, _, _>(
                "SELECT count(*) FROM contracts WHERE key='service/explicit'",
                [],
                |r| r.get(0)
            )
            .unwrap(),
        1
    );
    store.close().unwrap();
    let journal = HostJournal::open(config.clone()).await.unwrap();
    let target = journal.target("fixture").await.unwrap().unwrap();
    assert_eq!(target.required("desired").unwrap(), &J::from("running"));
    let launch = config::json_file(
        native(&field(&target, "configPath").unwrap()).unwrap(),
        config::CONFIGURATION_BYTES,
    )
    .await
    .unwrap();
    assert_eq!(launch.required("instanceId").unwrap(), &J::from("fixture"));
    assert_eq!(
        journal
            .installed("fixture")
            .await
            .unwrap()
            .unwrap()
            .required("enabled")
            .unwrap(),
        &J::Bool(true)
    );
    journal.close().await.unwrap();
    db_closed(&f);
}

#[tokio::test]
async fn interrupted_hive_bootstrap_retains_capture_and_retries_without_recapturing() {
    let f = ServiceFixture::new();
    let config = host(&f);
    prepared(&f, &config, false).await;
    let data = input();
    assert_eq!(
        owner(&f, Arc::new(FailThird(AtomicUsize::new(0))))
            .clear_admitted(config.clone(), "reset".into(), "new-epoch".into(), vec![])
            .await
            .unwrap_err()
            .code,
        "internal_error"
    );
    let held = record(&config).await;
    assert_eq!(held.required("phase").unwrap(), &J::from("prepared"));
    assert_eq!(
        held.required("instructionsCaptured").unwrap(),
        &J::Bool(true)
    );
    assert_json(
        held.required("instructions").unwrap(),
        data.required("values").unwrap(),
    );
    assert!(!f.0.join("services/hive/work/clear.bin").exists());
    let store = Store::open(&hive_path(&f)).unwrap();
    assert_eq!(
        store.metadata("runtime_epoch").unwrap().unwrap(),
        "new-epoch"
    );
    store.close().unwrap();
    // Recreate only the disposable database through the actual owner. Revisions
    // are immutable; no UPDATE or owner-invariant bypass is used as a fixture.
    filesystem::remove_force(hive_path(&f)).await.unwrap();
    hive_reset_storage()
        .rebootstrap(ResetBootstrap {
            path: hive_path(&f),
            epoch: "later-partial".into(),
            instructions: J::Array(vec![]),
            scope_roots: vec![],
            service_contracts: vec![],
            now: Arc::new(|| Ok(DATE.into())),
        })
        .await
        .unwrap();
    assert_ne!(
        encoded(
            &hive_reset_storage()
                .saved_instructions(hive_path(&f))
                .await
                .unwrap()
        ),
        encoded(data.required("values").unwrap())
    );
    let done = owner(&f, Arc::new(Fixed))
        .clear_admitted(
            config.clone(),
            "reset".into(),
            "new-epoch".into(),
            data.required("definitions")
                .unwrap()
                .as_array()
                .unwrap()
                .to_vec(),
        )
        .await
        .unwrap();
    assert_json(
        done.required("instructions").unwrap(),
        data.required("values").unwrap(),
    );
    assert_json(
        &hive_reset_storage()
            .saved_instructions(hive_path(&f))
            .await
            .unwrap(),
        data.required("values").unwrap(),
    );
    db_closed(&f);
}

#[tokio::test]
async fn changed_deletion_path_refuses_after_capture_without_deleting_outside_data() {
    let f = ServiceFixture::new();
    let config = host(&f);
    let mut initial = prepared(&f, &config, false).await;
    let outside = f.0.join("protected");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("sentinel"), b"keep").unwrap();
    let link = f.0.join("services/hive/data/changed");
    filesystem::directory_link(&outside, &link).unwrap();
    let mut plan = initial.required("plan").unwrap().clone();
    plan.insert(
        "delete",
        J::Array(vec![entry(
            &f,
            "data",
            "changed",
            "services/hive/data/changed",
        )]),
    );
    initial.insert("plan", plan);
    config::atomic_json(reset_record_path(&config).unwrap(), initial)
        .await
        .unwrap();
    let err = owner(&f, Arc::new(Fixed))
        .clear_admitted(config.clone(), "reset".into(), "new-epoch".into(), vec![])
        .await
        .unwrap_err();
    assert_eq!(err.code, "target_conflict");
    assert_eq!(fs::read(outside.join("sentinel")).unwrap(), b"keep");
    assert_eq!(
        record(&config)
            .await
            .required("instructionsCaptured")
            .unwrap(),
        &J::Bool(true)
    );
    assert!(f.0.join("services/hive/work/clear.bin").exists());
    db_closed(&f);
}

#[tokio::test]
async fn identity_and_cleared_replay_are_effect_free_and_preserve_failure_keeps_hold() {
    let f = ServiceFixture::new();
    let config = host(&f);
    let mut initial = prepared(&f, &config, false).await;
    let bytes = fs::read(reset_record_path(&config).unwrap()).unwrap();
    assert_eq!(
        owner(&f, Arc::new(Fixed))
            .clear_admitted(config.clone(), "other".into(), "new-epoch".into(), vec![])
            .await
            .unwrap_err()
            .code,
        "mutation_conflict"
    );
    assert_eq!(
        fs::read(reset_record_path(&config).unwrap()).unwrap(),
        bytes
    );
    initial.insert("phase", "cleared".into());
    config::atomic_json(reset_record_path(&config).unwrap(), initial.clone())
        .await
        .unwrap();
    let bytes = fs::read(reset_record_path(&config).unwrap()).unwrap();
    assert_json(
        &owner(&f, Arc::new(Fixed))
            .clear_admitted(config.clone(), "reset".into(), "new-epoch".into(), vec![])
            .await
            .unwrap(),
        &initial,
    );
    assert_eq!(
        fs::read(reset_record_path(&config).unwrap()).unwrap(),
        bytes
    );
    assert!(f.0.join("services/hive/work/clear.bin").exists());
    initial.insert("phase", "prepared".into());
    config::atomic_json(reset_record_path(&config).unwrap(), initial)
        .await
        .unwrap();
    fs::write(
        f.0.join("services/hive/data/whatsapp/opaque.bin"),
        b"changed-after-preview",
    )
    .unwrap();
    let err = owner(&f, Arc::new(Fixed))
        .clear_admitted(config.clone(), "reset".into(), "new-epoch".into(), vec![])
        .await
        .unwrap_err();
    assert_eq!(err.code, "configuration_changed");
    assert_eq!(
        record(&config).await.required("phase").unwrap(),
        &J::from("prepared")
    );
    assert!(
        runtime_maintenance::runtime_reset_marker(&config)
            .unwrap()
            .is_file()
    );
    assert!(!f.0.join("services/hive/work/clear.bin").exists());
    let journal = HostJournal::open(config).await.unwrap();
    journal.close().await.unwrap();
    db_closed(&f);
}
