use super::*;
use crate::{journal::HostJournal, node_runtime::NodeRuntime, runtime_maintenance};
use ivy_contracts::{core_schema::validate_host, hash::hash_json, json};
use ivy_sdk::clock::parse_date;
use std::{
    fs,
    path::{Path, PathBuf},
    time::Instant,
};

const DATE: &str = "2026-10-20T14:00:00.000Z";
const RESET: &str = "fixture-reset";
const EPOCH: &str = "fixture-epoch";
struct FixedClock;
impl Clock for FixedClock {
    fn utc_millis(&self) -> i64 {
        parse_date(&DATE.into()) as i64
    }
    fn monotonic(&self) -> Instant {
        Instant::now()
    }
}
fn value(source: &str) -> J {
    json::parse(source, usize::MAX).unwrap()
}
fn path(path: &Path) -> J {
    J::String(node_path::from_native(path))
}
fn digest(c: char) -> J {
    format!("sha256:{}", c.to_string().repeat(64)).into()
}
fn equal(a: &J, b: &J) {
    assert_eq!(
        json::canonical(a, usize::MAX).unwrap(),
        json::canonical(b, usize::MAX).unwrap()
    );
}
#[derive(Debug, PartialEq, Eq)]
enum FileState {
    Missing,
    Directory,
    File(Vec<u8>),
}
fn state(path: &Path) -> FileState {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => FileState::Directory,
        Ok(_) => FileState::File(fs::read(path).unwrap()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => FileState::Missing,
        Err(error) => panic!("Fixture metadata: {error}"),
    }
}
struct Fixture {
    root: PathBuf,
    config: J,
    runtime: J,
    data_path: PathBuf,
    runtime_path: PathBuf,
    record: J,
    candidate: J,
    manifest: J,
    target: Option<J>,
    reset_id: JsString,
    epoch: JsString,
}
impl Fixture {
    async fn new(case: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("ivy-config-ü-空-😀-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        let mut instance = value(
            r#"{"instanceId":"fixture","serviceNodeId":"fixture-node","componentId":"fixture","engine":"process","enabled":true,"settings":{}}"#,
        );
        if case == "hive-unicode-target" {
            instance.insert("instanceId", "fixture-é🚀".into());
        }
        if case == "hive-surrogate-target" {
            let mut id = JsString::from("fixture-");
            id.0.push(0xd800);
            instance.insert("instanceId", J::String(id));
        }
        if case.starts_with("hive-") || ["disabled-hive", "process-hive"].contains(&case) {
            instance.insert("componentId", "hive".into());
            instance.insert(
                "engine",
                if case == "process-hive" {
                    "process"
                } else {
                    "docker"
                }
                .into(),
            );
        } else if case.starts_with("docker-") {
            instance.insert("engine", "docker".into());
        }
        if case == "disabled-config" {
            instance.insert("enabled", false.into());
        }
        let config = J::object([
            ("schemaVersion", J::Number(1.0)),
            ("hostId", "fixture-host".into()),
            ("ivyRoot", path(&root.join("installation"))),
            ("runtimeRoot", path(&root.join("state"))),
            ("artifactRoot", path(&root.join("artifacts"))),
            ("stagingRoot", path(&root.join("staging"))),
            ("publicBaseUrl", "https://example.invalid/ivy".into()),
            (
                "executables",
                J::object([(
                    "node",
                    path(&PathBuf::from(
                        std::env::var_os("IVY_RESTORE_NODE")
                            .expect("pinned optional workload executable"),
                    )),
                )]),
            ),
            ("instances", J::Array(vec![instance.clone()])),
        ]);
        // No workload needs Node; the public optional capability remains supported.
        validate_host("HostConfig", &config).unwrap();
        let mut data_path = root.join("runtime-data");
        let mut runtime_path = root.join("state/configs/配置-é-🚀.json");
        if case == "hive-opaque-runtime" {
            #[cfg(windows)]
            let name = {
                use std::{ffi::OsString, os::windows::ffi::OsStringExt};
                OsString::from_wide(&[0xd800, 46, 106, 115, 111, 110])
            };
            #[cfg(not(windows))]
            let name = std::ffi::OsString::from("配置-é-🚀.json");
            runtime_path.set_file_name(&name);
            data_path.push(name);
        }
        let mut runtime = value(
            r#"{"schemaVersion":1,"hostId":"fixture-host","publicBaseUrl":"https://example.invalid/ivy","version":"1.0.0","settings":{}}"#,
        );
        for key in ["instanceId", "serviceNodeId", "componentId"] {
            runtime.insert(key, instance.required(key).unwrap().clone());
        }
        runtime.insert("dataRoot", path(&data_path));
        runtime.insert("artifactRoot", path(&root.join("artifact")));
        runtime.insert("buildId", digest('a'));
        validate_host("InstanceConfig", &runtime).unwrap();
        let candidate = J::object([
            ("candidateId", digest('a')),
            (
                "componentId",
                instance.required("componentId").unwrap().clone(),
            ),
            ("artifactRoot", path(&root.join("artifact"))),
            ("manifestPath", path(&root.join("artifact/manifest.json"))),
            ("createdAt", DATE.into()),
            (
                "platform",
                J::object([
                    ("os", if cfg!(windows) { "win32" } else { "linux" }.into()),
                    (
                        "arch",
                        if cfg!(target_arch = "aarch64") {
                            "arm64"
                        } else {
                            "x64"
                        }
                        .into(),
                    ),
                    ("node", "24.18.0".into()),
                ]),
            ),
            ("buildId", digest('a')),
        ]);
        let mut manifest = value(
            r#"{"schemaVersion":1,"kind":"app","version":"1.0.0","connectsToHive":false,"requirements":{"node":">=24.18.0 <25.0.0","hiveProtocol":null,"contracts":[]},"shutdown":{"timeoutMs":1000},"app":{"appId":"fixture","dist":"dist/apps/fixture","entryPath":"index.html"}}"#,
        );
        manifest.insert(
            "componentId",
            instance.required("componentId").unwrap().clone(),
        );
        manifest.insert("buildId", digest('a'));
        validate_host("Candidate", &candidate).unwrap();
        validate_host("ReleaseManifest", &manifest).unwrap();
        fs::create_dir_all(runtime_path.parent().unwrap()).unwrap();
        if !case.ends_with("missing-runtime") {
            if case.ends_with("invalid-runtime") {
                fs::write(&runtime_path, b"{invalid").unwrap();
            } else {
                config::write_json(&runtime_path, &runtime).unwrap();
            }
        }
        fs::create_dir_all(root.join("artifact/dist")).unwrap();
        config::write_json(
            &root.join("artifact/dist/build-info.json"),
            &J::object([
                (
                    "buildId",
                    digest(if case.ends_with("build-mismatch") {
                        'b'
                    } else {
                        'a'
                    }),
                ),
                ("version", "1.0.0".into()),
            ]),
        )
        .unwrap();
        let journal = HostJournal::open(config.clone()).await.unwrap();
        journal
            .save_candidate(candidate.clone(), manifest.clone())
            .await
            .unwrap();
        let target = if (case.starts_with("hive-") || case.starts_with("docker-"))
            && !case.ends_with("missing-target")
        {
            Some(J::object([
                ("schemaVersion", J::Number(1.0)),
                (
                    "instanceId",
                    instance.required("instanceId").unwrap().clone(),
                ),
                ("revision", "fixture-launch".into()),
                ("candidateId", digest('a')),
                (
                    "desired",
                    if case.ends_with("stopped-target") {
                        "stopped"
                    } else {
                        "running"
                    }
                    .into(),
                ),
                ("configPath", path(&runtime_path)),
                ("requestedAt", DATE.into()),
            ]))
        } else {
            None
        };
        if case != "hive-surrogate-target" {
            if let Some(target) = &target {
                journal.restore_target(target.clone()).await.unwrap();
            }
        }
        journal.close().await.unwrap();
        if case == "hive-surrogate-target" {
            // Exercise a pre-existing v1 storage shape through its read path:
            // exact JSON identity alongside a UTF-8 SQLite TEXT lookup key.
            let db = ivy_store::rusqlite::Connection::open(root.join("state/deployments.sqlite"))
                .unwrap();
            db.execute(
                "INSERT INTO runtime_targets(instance_id,value) VALUES (?1,?2)",
                ivy_store::rusqlite::params![
                    "fixture-\u{fffd}",
                    json::canonical(target.as_ref().unwrap(), usize::MAX).unwrap()
                ],
            )
            .unwrap();
            db.close().unwrap();
        }
        let plan = J::object([
            ("schemaVersion", J::Number(1.0)),
            ("deploymentIdentity", "fixture-deployment".into()),
            ("hostId", "fixture-host".into()),
            ("configPath", path(&root.join("host.json"))),
            (
                "instances",
                J::Array(vec![J::object([
                    (
                        "instanceId",
                        instance.required("instanceId").unwrap().clone(),
                    ),
                    (
                        "componentId",
                        instance.required("componentId").unwrap().clone(),
                    ),
                    ("enabled", instance.required("enabled").unwrap().clone()),
                ])]),
            ),
            ("scopeRootIds", J::Array(vec![])),
            ("delete", J::Array(vec![])),
            ("preserve", J::Array(vec![])),
            ("completionMarkers", J::Array(vec![])),
        ]);
        let mut record = J::object([
            ("schemaVersion", J::Number(1.0)),
            ("deploymentIdentity", "fixture-deployment".into()),
            ("resetId", RESET.into()),
            ("runtimeEpoch", EPOCH.into()),
            ("hostId", "fixture-host".into()),
            ("phase", "complete".into()),
            ("updatedAt", "2026-01-01T00:00:00.000Z".into()),
            ("planHash", hash_json(&plan).unwrap().into()),
            ("plan", plan),
            (
                "saved",
                J::Array(vec![J::object([
                    (
                        "instanceId",
                        instance.required("instanceId").unwrap().clone(),
                    ),
                    ("enabled", true.into()),
                    ("candidate", candidate.clone()),
                    ("manifest", manifest.clone()),
                ])]),
            ),
            (
                "instructions",
                value(r#"[{"retained":"instruction authority"}]"#),
            ),
            ("publisherCredentialDigest", digest('d')),
            ("instructionsCaptured", true.into()),
            ("deploymentComplete", false.into()),
            (
                "retainedExtra",
                value(r#"{"originalOperationId":"fixture-original"}"#),
            ),
        ]);
        if case == "no-enabled" {
            record.insert("saved", J::Array(vec![]));
        }
        if ["disabled-hive", "docker-nonboolean-enabled"].contains(&case) {
            let mut saved = record
                .required("saved")
                .unwrap()
                .as_array()
                .unwrap()
                .to_vec();
            saved[0].insert(
                "enabled",
                if case == "disabled-hive" {
                    false.into()
                } else {
                    J::Array(vec![])
                },
            );
            record.insert("saved", J::Array(saved));
        }
        if [
            "prepared",
            "cleared",
            "released",
            "already-released",
            "released-bad-saved",
        ]
        .contains(&case)
        {
            record.insert(
                "phase",
                if case.contains("released") {
                    "released"
                } else {
                    case
                }
                .into(),
            );
        }
        if ["saved-null", "released-bad-saved"].contains(&case) {
            record.insert("saved", J::Null);
        }
        if case == "saved-entry-null" {
            record.insert("saved", J::Array(vec![J::Null]));
        }
        if case == "deployment-complete" {
            record.insert("deploymentComplete", true.into());
        }
        let fixture = Self {
            root,
            config,
            runtime,
            data_path,
            runtime_path,
            record,
            candidate,
            manifest,
            target,
            reset_id: if case == "wrong-reset" {
                "different"
            } else {
                RESET
            }
            .into(),
            epoch: if case == "wrong-epoch" {
                "different"
            } else {
                EPOCH
            }
            .into(),
        };
        fs::create_dir_all(fixture.marker().parent().unwrap()).unwrap();
        if case != "missing-record" {
            let bytes = match case {
                "null-record" => "null".into(),
                "malformed-record" => "{invalid".into(),
                _ => format!(
                    " \n{}\n ",
                    json::canonical(&fixture.record, usize::MAX).unwrap()
                ),
            };
            fs::write(fixture.record_path(), bytes).unwrap();
        }
        if ["hold-directory", "marker-directory"].contains(&case) {
            fs::create_dir(fixture.marker()).unwrap();
        } else if case != "missing-hold" {
            fs::write(fixture.marker(), b"retained hold\n").unwrap();
        }
        fixture
    }
    fn marker(&self) -> PathBuf {
        self.root.join("installation/reset/maintenance.json")
    }
    fn record_path(&self) -> PathBuf {
        self.root.join("installation/reset/current.json")
    }
    fn snapshot(&self) -> (FileState, FileState, FileState) {
        (
            state(&self.record_path()),
            state(&self.marker()),
            state(&self.data_path),
        )
    }
    async fn authority_unchanged(&self) {
        let journal = HostJournal::open(self.config.clone()).await.unwrap();
        equal(
            &journal
                .candidate(&digest('a').as_str().unwrap())
                .await
                .unwrap(),
            &self.candidate,
        );
        equal(
            &journal
                .manifest(&digest('a').as_str().unwrap())
                .await
                .unwrap(),
            &self.manifest,
        );
        let instance = &self
            .config
            .required("instances")
            .unwrap()
            .as_array()
            .unwrap()[0];
        let id = String::from_utf16_lossy(&field(instance, "instanceId").unwrap().0);
        let target = journal.target(&id).await.unwrap();
        match (&target, &self.target) {
            (Some(actual), Some(expected)) => equal(actual, expected),
            _ => assert!(target.is_none() && self.target.is_none()),
        }
        assert!(journal.installed(&id).await.unwrap().is_none());
        journal.close().await.unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
fn native() -> ResetHost {
    ResetHost::new(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.."),
        PathBuf::from(std::env::var_os("IVY_BACKUP_JOB").expect("actual Job helper")),
        Arc::new(FixedClock),
    )
    .unwrap()
}
async fn bootstrap(host: &ResetHost, fixture: &Fixture) -> IvyResult<J> {
    host.0
        .clone()
        .bootstrap(
            fixture.config.clone(),
            fixture.reset_id.clone(),
            fixture.epoch.clone(),
            None,
        )
        .await
}
async fn release(host: &ResetHost, fixture: &Fixture) -> IvyResult<J> {
    host.0
        .clone()
        .release(
            fixture.config.clone(),
            fixture.reset_id.clone(),
            fixture.epoch.clone(),
            None,
        )
        .await
}

#[tokio::test]
async fn bootstrap_identity_hold_and_saved_selection_preserve_retained_state() {
    let host = native();
    for case in [
        "no-hive",
        "no-enabled",
        "disabled-hive",
        "process-hive",
        "wrong-reset",
        "wrong-epoch",
        "prepared",
        "cleared",
        "released",
        "missing-hold",
        "hold-directory",
        "missing-record",
        "null-record",
        "malformed-record",
        "saved-null",
        "saved-entry-null",
    ] {
        let fixture = Fixture::new(case).await;
        let before = fixture.snapshot();
        let result = bootstrap(&host, &fixture).await;
        match case {
            "no-hive" | "no-enabled" | "disabled-hive" | "process-hive" | "hold-directory" => {
                equal(&result.unwrap(), &fixture.record)
            }
            "missing-record" | "null-record" | "malformed-record" | "saved-null"
            | "saved-entry-null" => {
                assert_eq!(result.unwrap_err().code, "internal_error", "{case}")
            }
            _ => assert_eq!(result.unwrap_err().code, "mutation_conflict", "{case}"),
        }
        assert_eq!(
            fixture.snapshot(),
            before,
            "{case}: bootstrap changed retained state"
        );
        fixture.authority_unchanged().await;
    }
}
#[tokio::test]
async fn bootstrap_uses_real_journal_configuration_and_native_docker_rejection() {
    let host = native();
    for (case, code, loaded) in [
        ("hive-missing-target", "storage_invalid", false),
        ("hive-stopped-target", "storage_invalid", false),
        ("hive-missing-runtime", "internal_error", false),
        ("hive-build-mismatch", "build_mismatch", false),
        ("hive-unsupported", "unsupported_runtime", true),
        ("hive-opaque-runtime", "unsupported_runtime", true),
    ] {
        let fixture = Fixture::new(case).await;
        let before = (state(&fixture.record_path()), state(&fixture.marker()));
        assert_eq!(
            bootstrap(&host, &fixture).await.unwrap_err().code,
            code,
            "{case}"
        );
        assert_eq!(
            (state(&fixture.record_path()), state(&fixture.marker())),
            before,
            "{case}"
        );
        assert_eq!(
            fixture.data_path.is_dir(),
            loaded,
            "{case}: configuration admission effects"
        );
        if loaded {
            equal(
                &config::json_file(&fixture.runtime_path, config::CONFIGURATION_BYTES)
                    .await
                    .unwrap(),
                &fixture.runtime,
            );
        }
        fixture.authority_unchanged().await;
    }
}
#[cfg(windows)]
#[tokio::test]
async fn release_idempotence_and_ordinary_process_selection_preserve_original_bytes() {
    let host = native();
    for case in [
        "no-enabled",
        "ordinary-process",
        "disabled-config",
        "disabled-hive",
        "already-released",
        "released-bad-saved",
        "wrong-reset",
        "wrong-epoch",
        "prepared",
        "cleared",
        "missing-record",
        "null-record",
        "malformed-record",
        "saved-null",
        "saved-entry-null",
        "deployment-complete",
        "missing-hold",
        "docker-nonboolean-enabled",
    ] {
        let fixture = Fixture::new(case).await;
        let before = fixture.snapshot();
        let result = release(&host, &fixture).await;
        if [
            "wrong-reset",
            "wrong-epoch",
            "prepared",
            "cleared",
            "missing-record",
            "null-record",
            "malformed-record",
            "saved-null",
            "saved-entry-null",
        ]
        .contains(&case)
        {
            let code = if ["wrong-reset", "wrong-epoch", "prepared", "cleared"].contains(&case) {
                "mutation_conflict"
            } else {
                "internal_error"
            };
            assert_eq!(result.unwrap_err().code, code, "{case}");
            assert_eq!(fixture.snapshot(), before, "{case}");
        } else if ["already-released", "released-bad-saved"].contains(&case) {
            equal(&result.unwrap(), &fixture.record);
            assert_eq!(
                fixture.snapshot(),
                before,
                "{case}: replay rewrote retained bytes"
            );
        } else {
            let mut expected = fixture.record.clone();
            expected.insert("phase", "released".into());
            expected.insert("deploymentComplete", false.into());
            expected.insert("updatedAt", DATE.into());
            equal(&result.unwrap(), &expected);
            equal(
                &config::json_file(fixture.record_path(), config::CONFIGURATION_BYTES)
                    .await
                    .unwrap(),
                &expected,
            );
            assert_eq!(state(&fixture.marker()), FileState::Missing);
            assert_eq!(state(&fixture.data_path), FileState::Missing);
        }
        fixture.authority_unchanged().await;
    }
}
#[cfg(windows)]
#[tokio::test]
async fn release_errors_recreate_the_original_hold_even_when_marker_recovery_fails() {
    let host = native();
    for (case, code) in [
        ("marker-directory", "internal_error"),
        ("docker-missing-target", "storage_invalid"),
        ("docker-stopped-target", "storage_invalid"),
        ("docker-invalid-runtime", "internal_error"),
        ("docker-unsupported", "unsupported_runtime"),
        ("hive-missing-target", "storage_invalid"),
        ("hive-stopped-target", "storage_invalid"),
        ("hive-missing-runtime", "internal_error"),
        ("hive-build-mismatch", "build_mismatch"),
        ("hive-unsupported", "unsupported_runtime"),
        ("hive-opaque-runtime", "unsupported_runtime"),
    ] {
        let fixture = Fixture::new(case).await;
        assert_eq!(
            release(&host, &fixture).await.unwrap_err().code,
            code,
            "{case}"
        );
        let mut expected = fixture.record.clone();
        expected.insert("updatedAt", DATE.into());
        equal(
            &config::json_file(fixture.record_path(), config::CONFIGURATION_BYTES)
                .await
                .unwrap(),
            &expected,
        );
        if case == "marker-directory" {
            assert_eq!(state(&fixture.marker()), FileState::Directory);
        } else {
            equal(
                &config::json_file(fixture.marker(), config::CONFIGURATION_BYTES)
                    .await
                    .unwrap(),
                &J::object([
                    ("schemaVersion", J::Number(1.0)),
                    ("deploymentIdentity", "fixture-deployment".into()),
                    ("resetId", RESET.into()),
                    ("runtimeEpoch", EPOCH.into()),
                    ("hostId", "fixture-host".into()),
                    ("createdAt", DATE.into()),
                ]),
            );
        }
        let hive_marker = runtime_maintenance::hive_reset_bootstrap_marker(&fixture.data_path);
        if ["hive-unsupported", "hive-opaque-runtime"].contains(&case) {
            equal(
                &config::json_file(&hive_marker, config::CONFIGURATION_BYTES)
                    .await
                    .unwrap(),
                &J::object([
                    ("schemaVersion", J::Number(1.0)),
                    ("resetId", RESET.into()),
                    ("runtimeEpoch", EPOCH.into()),
                    ("phase", "bootstrap".into()),
                    ("publisherCredentialDigest", digest('d')),
                ]),
            );
        } else {
            assert_eq!(state(&hive_marker), FileState::Missing);
        }
        fixture.authority_unchanged().await;
        // Recovery's final stop stage must release each actual owner lock even
        // when marker recreation failed.
        for member in ["executor", "owners/fixture"] {
            let lock = crate::journal::ExecutorLock::acquire(
                fixture.root.join("state").join(member),
                false,
            )
            .await
            .unwrap();
            lock.close().await.unwrap();
        }
    }
}
#[cfg(windows)]
#[tokio::test]
async fn release_reprepare_complete_release_confirm_preserves_one_identity() {
    let host = native();
    let fixture = Fixture::new("ordinary-process").await;
    let first = release(&host, &fixture).await.unwrap();
    assert_eq!(first.get("phase"), Some(&"released".into()));
    let held = prepare_reset_hold(
        fixture.config.clone(),
        fixture.record.required("plan").unwrap().clone(),
        RESET.into(),
        EPOCH.into(),
        field(&fixture.record, "publisherCredentialDigest").unwrap(),
        Arc::new(FixedClock),
    )
    .await
    .unwrap();
    assert_eq!(held.get("phase"), Some(&"cleared".into()));
    host.0.clone().stop(fixture.config.clone()).await.unwrap();
    let completed = complete_reset_record(
        fixture.config.clone(),
        RESET.into(),
        EPOCH.into(),
        Arc::new(FixedClock),
    )
    .await
    .unwrap();
    assert_eq!(completed.get("phase"), Some(&"complete".into()));
    let released = release(&host, &fixture).await.unwrap();
    assert_eq!(released.get("phase"), Some(&"released".into()));
    let confirmed = confirm_reset_record(
        fixture.config.clone(),
        RESET.into(),
        EPOCH.into(),
        Arc::new(FixedClock),
    )
    .await
    .unwrap();
    let mut expected = fixture.record.clone();
    expected.insert("phase", "released".into());
    expected.insert("updatedAt", DATE.into());
    expected.insert("deploymentComplete", true.into());
    equal(&confirmed, &expected);
    assert_eq!(state(&fixture.marker()), FileState::Missing);
    let before = state(&fixture.record_path());
    equal(
        &confirm_reset_record(
            fixture.config.clone(),
            RESET.into(),
            EPOCH.into(),
            Arc::new(FixedClock),
        )
        .await
        .unwrap(),
        &confirmed,
    );
    assert_eq!(state(&fixture.record_path()), before);
    fixture.authority_unchanged().await;
}
#[tokio::test]
async fn new_full_entrypoints_use_actual_administrator_admission_before_writes() {
    let host = native();
    let administrator = host.require_administrator().await.is_ok();
    for bootstrap in [true, false] {
        let fixture = Fixture::new("no-enabled").await;
        let before = fixture.snapshot();
        let result = if bootstrap {
            host.bootstrap(
                fixture.config.clone(),
                RESET.into(),
                EPOCH.into(),
                None::<NodeRuntime>,
            )
            .await
        } else {
            host.release(
                fixture.config.clone(),
                RESET.into(),
                EPOCH.into(),
                None::<NodeRuntime>,
            )
            .await
        };
        if !administrator {
            assert_eq!(result.unwrap_err().code, "access_denied");
            assert_eq!(fixture.snapshot(), before);
        } else if bootstrap {
            equal(&result.unwrap(), &fixture.record);
            assert_eq!(fixture.snapshot(), before);
        } else {
            let mut expected = fixture.record.clone();
            expected.insert("phase", "released".into());
            expected.insert("deploymentComplete", false.into());
            expected.insert("updatedAt", DATE.into());
            equal(&result.unwrap(), &expected);
            assert_eq!(state(&fixture.marker()), FileState::Missing);
        }
        fixture.authority_unchanged().await;
    }
}
#[tokio::test]
async fn journal_lookup_preserves_utf16_identity_through_the_existing_sqlite_boundary() {
    let host = native();
    for case in [
        "hive-unsupported",
        "hive-unicode-target",
        "hive-surrogate-target",
    ] {
        let fixture = Fixture::new(case).await;
        let before = (state(&fixture.record_path()), state(&fixture.marker()));
        assert_eq!(
            bootstrap(&host, &fixture).await.unwrap_err().code,
            "unsupported_runtime",
            "{case}"
        );
        assert_eq!(
            (state(&fixture.record_path()), state(&fixture.marker())),
            before
        );
        fixture.authority_unchanged().await;
    }
}
