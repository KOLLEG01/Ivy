use super::*;
use ivy_contracts::{core_schema::validate_host, json::canonical};
use ivy_sdk::service::{ConnectionOptions, ServiceConnection};
use ivy_secretary::journal::{Journal, Limits};
use std::{
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::Duration,
};

struct Process(Child);
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
struct Directory(PathBuf);
impl Drop for Directory {
    fn drop(&mut self) {
        let actual = std::fs::canonicalize(&self.0).unwrap();
        assert_eq!(
            actual.parent(),
            Some(
                std::fs::canonicalize(std::env::temp_dir())
                    .unwrap()
                    .as_path()
            )
        );
        assert!(
            actual
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("secretary-startup-")
        );
        let _ = std::fs::remove_dir_all(actual);
    }
}
fn hidden(command: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
}
fn text_path(path: &Path) -> IvyJson {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        IvyJson::String(ivy_contracts::JsString(
            path.as_os_str().encode_wide().collect(),
        ))
    }
    #[cfg(not(windows))]
    {
        path.to_string_lossy().as_ref().into()
    }
}
fn write(path: &Path, value: &IvyJson) {
    std::fs::write(path, canonical(value, 4 * 1024 * 1024).unwrap()).unwrap();
}
fn read(path: &Path) -> Option<IvyJson> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| parse(&text, 4 * 1024 * 1024).ok())
}
async fn trace(connection: &ServiceConnection) -> Vec<IvyJson> {
    connection
        .request(
            "fixture.trace",
            IvyJson::object(Vec::<(String, IvyJson)>::new()),
            RequestOptions::default(),
        )
        .await
        .unwrap()
        .as_array()
        .unwrap()
        .to_vec()
}
async fn until(mut ready: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(12), async {
        while !ready() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("Production lifecycle boundary did not settle");
}
fn health_projection(value: &IvyJson) -> IvyJson {
    IvyJson::object(
        [
            "schemaVersion",
            "instanceId",
            "componentId",
            "buildId",
            "ready",
            "generation",
            "details",
            "launchId",
        ]
        .map(|name| (name, value.required(name).unwrap().clone())),
    )
}

#[tokio::test]
async fn original_and_rust_service_process_registration_health_control_and_shutdown() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let mut fixture = Command::new("node");
    fixture
        .args([
            "tests/secretary-rust/generation-fixture.mjs",
            "--service",
            "--package-health",
        ])
        .current_dir(&root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    hidden(&mut fixture);
    let mut fixture = Process(fixture.spawn().unwrap());
    let mut line = String::new();
    BufReader::new(fixture.0.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let fixture_data = parse(&line, 1024 * 1024).unwrap();
    let address = format!(
        "http://127.0.0.1:{}",
        fixture_data.required("port").unwrap().as_f64().unwrap() as u16
    );
    let observer = ServiceConnection::connect(
        &address,
        "fixture",
        ConnectionOptions::new("fixture.observer".into()),
        RequestOptions::default().cancellation,
    )
    .await
    .unwrap();
    let directory =
        Directory(std::env::temp_dir().join(format!("secretary-startup-{}", uuid::Uuid::new_v4())));
    let artifact = directory.0.join("artifact");
    std::fs::create_dir_all(artifact.join("dist")).unwrap();
    std::fs::create_dir(artifact.join("dist/instructions")).unwrap();
    write(
        &artifact.join("dist/instructions/secretary-notification-policy.json"),
        fixture_data.required("notificationPolicy").unwrap(),
    );
    let build = IvyJson::object([
        ("version", "fixture".into()),
        ("buildId", format!("sha256:{}", "a".repeat(64)).into()),
    ]);
    write(&artifact.join("dist/build-info.json"), &build);
    let node = fixture_data.required("node").unwrap().as_str().unwrap();
    let mut package = Command::new(&node);
    package
        .arg("tests/secretary-rust/package-fixture.mjs")
        .arg(&artifact)
        .arg(env!("CARGO_BIN_EXE_ivy-secretary"))
        .arg(
            std::env::var_os("IVY_SECRETARY_HEALTH_BINARY")
                .expect("Actual Host health prerequisite is required"),
        )
        .arg(
            std::env::var_os("IVY_SECRETARY_JOB_HELPER")
                .expect("Qualified Host Job helper is required"),
        )
        .current_dir(&root);
    hidden(&mut package);
    let output = package.output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let package = parse(std::str::from_utf8(&output.stdout).unwrap(), 1024 * 1024).unwrap();
    assert_eq!(package.get("missingMatrixRefused"), Some(&true.into()));
    let mut results = vec![];
    for mode in ["original", "rust", "packaged"] {
        let reference = mode == "original";
        let packaged = mode == "packaged";
        observer
            .request(
                "fixture.reset",
                IvyJson::object(Vec::<(String, IvyJson)>::new()),
                RequestOptions::default(),
            )
            .await
            .unwrap();
        let state = directory.0.join(mode);
        let data = state.join("data");
        std::fs::create_dir_all(&data).unwrap();
        {
            let journal = Journal::open(&data.join("secretary.sqlite"), Limits::default()).unwrap();
            journal.db.execute_batch("PRAGMA user_version=37;CREATE TABLE retained_fixture(value BLOB);INSERT INTO retained_fixture VALUES(x'00ff00d8');").unwrap();
        }
        let config = IvyJson::object([
            ("schemaVersion", IvyJson::Number(1.0)),
            ("hostId", "fixture-host".into()),
            ("instanceId", "fixture.secretary".into()),
            ("componentId", "secretary".into()),
            ("serviceNodeId", "fixture.secretary".into()),
            ("credential", "fixture".into()),
            ("publicBaseUrl", address.clone().into()),
            ("dataRoot", text_path(&data)),
            ("artifactRoot", text_path(&artifact)),
            ("version", build.required("version").unwrap().clone()),
            ("buildId", build.required("buildId").unwrap().clone()),
            (
                "settings",
                fixture_data.required("settings").unwrap().clone(),
            ),
        ]);
        let path = state.join("instance.json");
        write(&path, &config);
        let node = fixture_data.required("node").unwrap().as_str().unwrap();
        if !reference {
            let mut import = if packaged {
                let mut command = Command::new(&node);
                command.arg(package.required("main").unwrap().as_str().unwrap());
                command
            } else {
                Command::new(env!("CARGO_BIN_EXE_ivy-secretary"))
            };
            import
                .args([
                    "import",
                    "--operation-id",
                    "fixture-epoch:1800000000000:startup-import",
                    "--config",
                ])
                .arg(&path)
                .current_dir(&root)
                .env("IVY_NODE_EXECUTABLE", &node);
            if packaged {
                import.env("IVY_NODE_EXECUTABLE", "ignored-before-dispatch.exe");
            }
            hidden(&mut import);
            let output = import.output().unwrap();
            assert!(
                output.status.success(),
                "Explicit retained-data import: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            let result = parse(
                std::str::from_utf8(&output.stdout).unwrap(),
                16 * 1024 * 1024,
            )
            .unwrap();
            assert_eq!(result.get("complete"), Some(&true.into()));
            let imported = trace(&observer).await;
            assert!(
                imported.iter().all(|row| !matches!(
                    row.get("method").and_then(|v| v.as_str().ok()).as_deref(),
                    Some("objects.write" | "tools.call")
                )),
                "This empty retained fixture must not initialize domain workers or providers"
            );
            observer
                .request(
                    "fixture.reset",
                    IvyJson::object(Vec::<(String, IvyJson)>::new()),
                    RequestOptions::default(),
                )
                .await
                .unwrap();
        }
        let mut command = if reference {
            let mut command = Command::new(&node);
            command.arg(
                PathBuf::from(
                    fixture_data
                        .required("oracleRoot")
                        .unwrap()
                        .as_str()
                        .unwrap(),
                )
                .join("services/secretary/src/main.js"),
            );
            command
        } else if packaged {
            let mut command = Command::new(&node);
            command.arg(package.required("main").unwrap().as_str().unwrap());
            command
        } else {
            Command::new(env!("CARGO_BIN_EXE_ivy-secretary"))
        };
        let stderr = std::fs::File::create(state.join("stderr.log")).unwrap();
        command
            .arg("--config")
            .arg(&path)
            .current_dir(&root)
            .env("IVY_NODE_EXECUTABLE", &node)
            .env("IVY_LAUNCH_ID", "fixture-launch")
            .stderr(Stdio::from(stderr))
            .stdout(Stdio::null())
            .stdin(Stdio::null());
        if packaged {
            command.env("IVY_NODE_EXECUTABLE", "ignored-before-dispatch.exe");
        }
        hidden(&mut command);
        let mut child = Process(command.spawn().unwrap());
        let health_path = data.join("health.json");
        until(|| {
            assert!(
                child.0.try_wait().unwrap().is_none(),
                "Service exited during startup: {}",
                std::fs::read_to_string(state.join("stderr.log")).unwrap()
            );
            read(&health_path).is_some_and(|value| value.get("ready") == Some(&true.into()))
        })
        .await;
        let health = read(&health_path).unwrap();
        validate_host("Health", &health).unwrap();
        assert_eq!(health.get("generation"), Some(&IvyJson::Number(7.0)));
        if packaged {
            assert_ne!(
                health.get("pid").and_then(IvyJson::as_f64),
                Some(child.0.id() as f64)
            );
        } else {
            assert_eq!(
                health.get("pid").and_then(IvyJson::as_f64),
                Some(child.0.id() as f64)
            );
        }
        let run_health = || {
            let mut command = Command::new(&node);
            let selected = if packaged {
                PathBuf::from(package.required("health").unwrap().as_str().unwrap())
            } else {
                PathBuf::from(
                    fixture_data
                        .required("oracleRoot")
                        .unwrap()
                        .as_str()
                        .unwrap(),
                )
                .join("packages/host-runtime/src/health.js")
            };
            command
                .arg(selected)
                .arg("--config")
                .arg(&path)
                .current_dir(&artifact)
                .env("IVY_NODE_EXECUTABLE", "ignored-before-health-dispatch.exe");
            hidden(&mut command);
            let output = command.output().unwrap();
            let value = parse(std::str::from_utf8(&output.stdout).unwrap(), 1024).unwrap();
            (output.status.success(), value)
        };
        let (healthy, value) = run_health();
        assert!(healthy, "{mode}: {value:?}");
        assert_eq!(
            value,
            IvyJson::object([
                ("ok", true.into()),
                ("buildId", build.required("buildId").unwrap().clone()),
                ("generation", IvyJson::Number(7.0))
            ])
        );
        observer
            .request(
                "fixture.healthNode",
                IvyJson::object([("buildId", format!("sha256:{}", "b".repeat(64)).into())]),
                RequestOptions::default(),
            )
            .await
            .unwrap();
        let (healthy, value) = run_health();
        assert!(!healthy);
        assert_eq!(value.get("code"), Some(&"service_not_ready".into()));
        observer
            .request(
                "fixture.healthNode",
                IvyJson::object(Vec::<(String, IvyJson)>::new()),
                RequestOptions::default(),
            )
            .await
            .unwrap();
        let mut control = IvyJson::object([
            ("schemaVersion", IvyJson::Number(1.0)),
            ("instanceId", "fixture.secretary".into()),
            ("launchId", "fixture-launch".into()),
            ("bootId", "foreign-boot".into()),
            ("action", "shutdown".into()),
            ("requestedAt", "2027-01-15T08:00:00.000Z".into()),
        ]);
        write(&data.join("control.json"), &control);
        tokio::time::sleep(Duration::from_millis(650)).await;
        assert!(
            child.0.try_wait().unwrap().is_none(),
            "Foreign control cannot stop this boot"
        );
        write(&data.join("control.json"), &IvyJson::Null);
        tokio::time::sleep(Duration::from_millis(650)).await;
        assert!(
            child.0.try_wait().unwrap().is_none(),
            "Invalid control observation is ignored"
        );
        control.insert("bootId", health.required("bootId").unwrap().clone());
        write(&data.join("control.json"), &control);
        until(|| child.0.try_wait().unwrap().is_some()).await;
        assert!(
            child.0.wait().unwrap().success(),
            "{}",
            std::fs::read_to_string(state.join("stderr.log")).unwrap()
        );
        let stopped = read(&health_path).unwrap();
        validate_host("Health", &stopped).unwrap();
        assert_eq!(stopped.get("ready"), Some(&false.into()));
        assert_eq!(stopped.get("details"), Some(&"Secretary stopped.".into()));
        assert_eq!(stopped.get("generation"), Some(&IvyJson::Null));
        let (healthy, value) = run_health();
        assert!(!healthy);
        assert_eq!(value.get("code"), Some(&"service_not_ready".into()));
        let calls = trace(&observer).await;
        let selected = |method: &str| {
            calls
                .iter()
                .filter(|row| row.get("method") == Some(&method.into()))
                .map(|row| row.required("params").unwrap().clone())
                .collect::<Vec<_>>()
        };
        let heartbeats = selected("service.heartbeat");
        assert!(
            heartbeats
                .iter()
                .any(|value| value.get("ready") == Some(&true.into())),
            "Hive observed ready only after reconciliation"
        );
        assert!(
            calls.iter().all(|row| row
                .get("method")
                .and_then(|m| m.as_str().ok())
                .is_none_or(|method| !method.starts_with("triage."))),
            "Dormant triage stays disabled"
        );
        results.push(IvyJson::object([
            ("identity", IvyJson::Array(selected("service.connect"))),
            ("registry", IvyJson::Array(selected("registry.sync"))),
            ("readyHealth", health_projection(&health)),
            ("stoppedHealth", health_projection(&stopped)),
        ]));
        let journal = Journal::open(&data.join("secretary.sqlite"), Limits::default()).unwrap();
        assert_eq!(
            journal
                .db
                .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            37
        );
        assert_eq!(
            journal
                .db
                .query_row("SELECT value FROM retained_fixture", [], |row| row
                    .get::<_, Vec<u8>>(0))
                .unwrap(),
            vec![0, 255, 0, 216]
        );
    }
    assert_eq!(
        canonical(&results[1], 4 * 1024 * 1024).unwrap(),
        canonical(&results[0], 4 * 1024 * 1024).unwrap()
    );
    assert_eq!(
        canonical(&results[2], 4 * 1024 * 1024).unwrap(),
        canonical(&results[0], 4 * 1024 * 1024).unwrap()
    );
    #[cfg(windows)]
    {
        // A valid old-layout helper cannot rescue a missing admitted payload.
        // Exercise the actual Node dispatcher and Rust entrypoint, not only the
        // selector called by the in-process preflight adapter.
        let helper = package.required("job").unwrap().as_str().unwrap();
        std::fs::copy(&helper, artifact.join("dist/native/ivy-job.exe")).unwrap();
        std::fs::remove_file(helper).unwrap();
        observer
            .request(
                "fixture.reset",
                IvyJson::object(Vec::<(String, IvyJson)>::new()),
                RequestOptions::default(),
            )
            .await
            .unwrap();
        let state = directory.0.join("packaged");
        let stderr = state.join("missing-helper.log");
        let mut command = Command::new(&node);
        command
            .arg(package.required("main").unwrap().as_str().unwrap())
            .arg("--config")
            .arg(state.join("instance.json"))
            .current_dir(&artifact)
            .stdout(Stdio::null())
            .stderr(std::fs::File::create(&stderr).unwrap());
        hidden(&mut command);
        let mut child = Process(command.spawn().unwrap());
        until(|| child.0.try_wait().unwrap().is_some()).await;
        assert_eq!(child.0.wait().unwrap().code(), Some(1));
        let error = read(&stderr).expect("Structured native startup failure");
        assert_eq!(error.get("code"), Some(&"artifact_missing".into()));
        assert!(
            trace(&observer).await.is_empty(),
            "Missing payload must fail before service connection"
        );
    }
    println!(
        "v1 Secretary launch: original/direct Rust/real legacy dispatch import,registry,ready+registration-bound health,foreign/invalid control,shutdown and retained SQLite bytes agree; incomplete advertised matrix refuses before staging; missing current-target helper fails before connection despite valid legacy helper"
    );
    observer.close();
}
