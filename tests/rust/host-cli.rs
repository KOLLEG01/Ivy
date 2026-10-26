//! One command scenario exercises a fresh process for each durable boundary.
//! IVY_TEST_CLI selects the normal packaged entry for installed qualification.
#[path = "host-service-fixture.rs"]
#[allow(dead_code)]
mod fixture;
use fixture::{ServiceFixture, host_configuration};
use ivy_contracts::{IvyJson as J, core_schema::validate_host, hash::digest, json};
use ivy_host_runtime::{config, host_config, journal::HostJournal, layout, native_path};
use std::{
    path::{Path, PathBuf},
    process::Command,
};

struct Cli(PathBuf);
impl Cli {
    fn new(f: &ServiceFixture) -> Self {
        if let Some(path) = std::env::var_os("IVY_TEST_CLI") {
            return Self(path.into());
        }
        let platform = format!(
            "{}-{}",
            if cfg!(windows) { "win32" } else { "linux" },
            if cfg!(target_arch = "x86_64") {
                "x64"
            } else {
                "arm64"
            }
        );
        let directory = f.0.join("package/dist/native").join(platform);
        std::fs::create_dir_all(&directory).unwrap();
        let source = PathBuf::from(env!("CARGO_BIN_EXE_ivy"));
        let destination = directory.join(source.file_name().unwrap());
        std::fs::copy(&source, &destination).unwrap();
        if cfg!(windows) {
            std::fs::copy(
                source.with_file_name("ivy-job.exe"),
                directory.join("ivy-job.exe"),
            )
            .unwrap();
        }
        Self(destination)
    }
}
fn path(value: &Path) -> J {
    J::String(native_path::from_native(value))
}
fn equal(actual: &J, expected: &J) {
    assert_eq!(
        json::canonical(actual, usize::MAX).unwrap(),
        json::canonical(expected, usize::MAX).unwrap()
    );
}
impl Cli {
    fn run(&self, config: &Path, args: &[&str], expected: i32) -> J {
        let result = Command::new(&self.0)
            .args(args)
            .arg("--config")
            .arg(config)
            .arg("--json")
            .env("IVY_HOST_CONFIG", config.with_file_name("ignored.json"))
            .env_remove("IVY_NODE_EXECUTABLE")
            .output()
            .unwrap();
        let text = String::from_utf8(result.stdout).unwrap();
        assert_eq!(
            result.status.code(),
            Some(expected),
            "{args:?}: {text}\n{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let value = json::parse(&text, usize::MAX).unwrap();
        validate_host("CliOutput", &value).unwrap();
        value
    }
}
#[tokio::test]
async fn native_command_replays_durable_work_and_restores_offline_data() {
    let f = ServiceFixture::new();
    let cli = Cli::new(&f);
    let source = f.0.join("host.json");
    let mut host = host_configuration(&f);
    host.insert("executables", J::object([("native-host", path(&cli.0))]));
    config::atomic_json(&source, host.clone()).await.unwrap();
    assert_eq!(
        cli.run(&source, &["status"], 0).get("code"),
        Some(&"host_status".into())
    );
    let checked = host_config::host_config(source.clone()).await.unwrap();
    let journal = HostJournal::open(checked).await.unwrap();
    let id = digest("CLI retained app fixture");
    let candidate = J::object([
        ("candidateId", id.clone().into()),
        ("componentId", "fixture".into()),
        ("buildId", id.clone().into()),
        ("createdAt", "2026-01-01T00:00:00.000Z".into()),
        ("artifactRoot", path(&f.0.join("retained-app"))),
        ("manifestPath", path(&f.0.join("retained-app.json"))),
        (
            "platform",
            J::object([
                ("os", if cfg!(windows) { "win32" } else { "linux" }.into()),
                (
                    "arch",
                    if cfg!(target_arch = "x86_64") {
                        "x64"
                    } else {
                        "arm64"
                    }
                    .into(),
                ),
                ("node", "unavailable".into()),
            ]),
        ),
    ]);
    let manifest = J::object([
        ("schemaVersion", J::Number(1.0)),
        ("componentId", "fixture".into()),
        ("kind", "app".into()),
        ("version", "1.0.0".into()),
        ("buildId", id.clone().into()),
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
    journal.save_candidate(candidate, manifest).await.unwrap();
    journal
        .set_installed(J::object([
            ("instanceId", "fixture".into()),
            ("candidateId", id.clone().into()),
            ("buildId", id.into()),
            ("enabled", true.into()),
            ("installedAt", "2026-01-01T00:00:00.000Z".into()),
        ]))
        .await
        .unwrap();
    journal.close().await.unwrap();
    let command = [
        "disable",
        "--instance",
        "fixture",
        "--operation-id",
        "cli-original",
    ];
    let accepted = cli.run(&source, &command, 0);
    let deployment = accepted.required("deploymentId").unwrap().as_str().unwrap();
    equal(&accepted, &cli.run(&source, &command, 0));
    equal(
        &accepted,
        &cli.run(&source, &["status", "--deployment", &deployment], 0),
    );
    assert_eq!(
        cli.run(
            &source,
            &[
                "enable",
                "--instance",
                "fixture",
                "--operation-id",
                "cli-original"
            ],
            1
        )
        .get("code"),
        Some(&"mutation_conflict".into())
    );
    let environment = Command::new(&cli.0)
        .args(["status", "--json"])
        .env("IVY_HOST_CONFIG", &source)
        .env_remove("IVY_NODE_EXECUTABLE")
        .output()
        .unwrap();
    assert!(environment.status.success());
    assert_eq!(
        json::parse(
            std::str::from_utf8(&environment.stdout).unwrap(),
            usize::MAX
        )
        .unwrap()
        .get("code"),
        Some(&"host_status".into())
    );
    std::fs::write(f.0.join("state/retained.txt"), b"offline private bytes").unwrap();
    let source_bytes = std::fs::read(&source).unwrap();
    let backup = f.0.join("backup");
    let saved = cli.run(
        &source,
        &["backup", "--destination", backup.to_str().unwrap()],
        0,
    );
    assert_eq!(saved.get("code"), Some(&"backup_completed".into()));
    let saved_data = saved.required("data").unwrap();
    validate_host("BackupResult", saved_data).unwrap();
    assert_eq!(saved_data.get("hostId"), host.get("hostId"));
    equal(saved_data.required("directory").unwrap(), &path(&backup));
    let hash = saved
        .required("data")
        .unwrap()
        .required("manifestHash")
        .unwrap()
        .as_str()
        .unwrap();
    let original_database = std::fs::read(backup.join("state/deployments.sqlite")).unwrap();
    let target = f.0.join("target.json");
    for (key, directory) in [
        ("runtimeRoot", "restored-state"),
        ("artifactRoot", "restored-artifacts"),
        ("stagingRoot", "restored-staging"),
    ] {
        host.insert(key, path(&f.0.join(directory)));
    }
    host.insert("publicBaseUrl", "http://127.0.0.1:39082/ivy".into());
    let mut instances = host
        .required("instances")
        .unwrap()
        .as_array()
        .unwrap()
        .to_vec();
    for instance in &mut instances {
        instance.insert("enabled", false.into());
    }
    host.insert("instances", J::Array(instances));
    let context = host_config::HostContext::current().unwrap();
    let mut expected_config = layout::resolve_host_configuration(
        &host,
        &native_path::from_native(&target),
        &context.environment,
        context.paths.style,
        &context.account_home,
    )
    .unwrap();
    validate_host("HostConfig", &expected_config).unwrap();
    assert_eq!(expected_config.get("hostId"), saved_data.get("hostId"));
    assert_ne!(
        expected_config.get("publicBaseUrl"),
        Some(&"http://127.0.0.1:39081/ivy".into())
    );
    for key in ["runtimeRoot", "artifactRoot", "stagingRoot"] {
        assert!(
            !native_path::to_native(
                expected_config
                    .required(key)
                    .unwrap()
                    .as_js_string()
                    .unwrap()
            )
            .exists()
        );
    }
    config::atomic_json(&target, host).await.unwrap();
    let template_bytes = std::fs::read(&target).unwrap();
    let restored = cli.run(
        &target,
        &[
            "restore",
            "--backup",
            backup.to_str().unwrap(),
            "--backup-hash",
            &hash,
        ],
        0,
    );
    assert_eq!(restored.get("code"), Some(&"restore_completed".into()));
    let restored_data = restored.required("data").unwrap();
    validate_host("RestoreResult", restored_data).unwrap();
    assert_eq!(restored_data.get("backupId"), saved_data.get("backupId"));
    assert_eq!(
        restored_data.get("manifestHash"),
        saved_data.get("manifestHash")
    );
    let restored_path = layout::host_configuration_path(&expected_config).unwrap();
    equal(
        restored_data.required("configPath").unwrap(),
        &path(&restored_path),
    );
    expected_config.insert(
        "restoredFrom",
        J::object([
            ("backupId", saved_data.required("backupId").unwrap().clone()),
            (
                "manifestHash",
                saved_data.required("manifestHash").unwrap().clone(),
            ),
            (
                "restoreId",
                restored_data.required("restoreId").unwrap().clone(),
            ),
        ]),
    );
    let restored_value = config::json_file(restored_path.clone(), 1024 * 1024)
        .await
        .unwrap();
    validate_host("HostConfig", &restored_value).unwrap();
    equal(&restored_value, &expected_config);
    assert!(
        restored_value
            .required("instances")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .all(|instance| instance.get("enabled") == Some(&false.into()))
    );
    let restored_root = native_path::to_native(
        restored_value
            .required("runtimeRoot")
            .unwrap()
            .as_js_string()
            .unwrap(),
    );
    assert!(!restored_root.join("restore.json").exists());
    {
        let db = rusqlite::Connection::open_with_flags(
            restored_root.join("deployments.sqlite"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        assert_eq!(
            db.query_row("PRAGMA quick_check", [], |row| row.get::<_, String>(0))
                .unwrap(),
            "ok"
        );
        assert!(
            db.prepare("PRAGMA foreign_key_check")
                .unwrap()
                .query([])
                .unwrap()
                .next()
                .unwrap()
                .is_none()
        );
        for table in ["observations", "runtime_targets", "installed"] {
            assert_eq!(
                db.query_row(&format!("SELECT count(*) FROM {table}"), [], |row| row
                    .get::<_, i64>(0))
                    .unwrap(),
                0
            );
        }
        let uncertain = db
            .prepare("SELECT deployment_id FROM operations WHERE phase='needs_attention'")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(uncertain, vec![deployment.clone()]);
    }
    assert_eq!(std::fs::read(&source).unwrap(), source_bytes);
    assert_eq!(std::fs::read(&target).unwrap(), template_bytes);
    assert_eq!(
        restored.required("data").unwrap().get("state"),
        Some(&"disabled".into())
    );
    assert_eq!(
        std::fs::read(f.0.join("restored-state/retained.txt")).unwrap(),
        b"offline private bytes"
    );
    assert_eq!(
        std::fs::read(backup.join("state/deployments.sqlite")).unwrap(),
        original_database
    );
    equal(
        &cli.run(&source, &["status", "--deployment", &deployment], 0),
        &accepted,
    );
    let restored_config = restored_path;
    let status = cli.run(&restored_config, &["status"], 0);
    assert_eq!(
        status
            .required("data")
            .unwrap()
            .required("restoredFrom")
            .unwrap()
            .get("manifestHash"),
        Some(&hash.into())
    );
    let pending = cli.run(
        &restored_config,
        &["status", "--deployment", &deployment],
        3,
    );
    assert_eq!(
        pending.get("code"),
        Some(&"deployment_needs_attention".into())
    );
    std::fs::write(&target, "{").unwrap();
    assert_eq!(
        cli.run(&target, &["status"], 2)
            .required("data")
            .unwrap()
            .get("message"),
        Some(&"Configuration contains malformed JSON.".into())
    );
    assert_eq!(
        cli.run(
            &target,
            &[
                "restore",
                "--backup",
                backup.to_str().unwrap(),
                "--backup-hash",
                &digest("wrong backup")
            ],
            1
        )
        .get("code"),
        Some(&"backup_changed".into())
    );
    if std::env::var_os("IVY_TEST_CLI").is_some() {
        // The regular package must carry the real reset dependencies, including
        // plans for uninstalled instances and the checked public UI bytes.
        let preview_path = f.0.join("reset-preview.json");
        let mut preview = host_configuration(&f);
        preview.insert("executables", J::object([("native-host", path(&cli.0))]));
        for (key, directory) in [
            ("runtimeRoot", "reset/state"),
            ("artifactRoot", "reset/artifacts"),
            ("stagingRoot", "reset/staging"),
        ] {
            preview.insert(key, path(&f.0.join(directory)));
        }
        preview.insert(
            "instances",
            J::Array(vec![J::object([
                ("instanceId", "preview-host".into()),
                ("serviceNodeId", "fixture-host.executor".into()),
                ("componentId", "host-executor".into()),
                ("engine", "process".into()),
                ("enabled", false.into()),
                ("credential", "preview-credential".into()),
                (
                    "settings",
                    J::object([(
                        "runtimeReset",
                        J::object([
                            ("coordinatorHostId", "fixture-host".into()),
                            ("uiIds", J::Array(vec!["wiki-ui".into()])),
                        ]),
                    )]),
                ),
            ])]),
        );
        preview.insert(
            "deployment",
            J::object([
                ("identity", "cli-reset-fixture".into()),
                (
                    "hosts",
                    J::Array(vec![J::object([
                        ("hostId", "fixture-host".into()),
                        ("configPath", path(&preview_path)),
                    ])]),
                ),
            ]),
        );
        config::atomic_json(&preview_path, preview).await.unwrap();
        let local = cli.run(
            &preview_path,
            &["runtime-reset-host", "--phase", "preview"],
            0,
        );
        assert_eq!(
            local.required("data").unwrap().get("hostId"),
            Some(&"fixture-host".into())
        );
        let reset = cli.run(&preview_path, &["runtime-reset"], 0);
        let data = reset.required("data").unwrap();
        assert_eq!(data.get("mode"), Some(&"preview".into()));
        assert_eq!(
            data.required("uis").unwrap().as_array().unwrap()[0].get("uiId"),
            Some(&"wiki-ui".into())
        );
        assert_eq!(
            cli.run(
                &preview_path,
                &["runtime-reset", "--apply", "--confirm", "wrong-deployment"],
                2
            )
            .get("code"),
            Some(&"invalid_arguments".into())
        );
    }
}
