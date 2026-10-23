use ivy_contracts::{IvyJson, JsString, core_schema::validate_host, hash, json};
use ivy_host_runtime::{
    IvyResult, accepted_configuration, filesystem, journal::HostJournal, node_path, target,
};
use ivy_store::rusqlite::Connection;
use std::{
    fs,
    path::{Path, PathBuf},
};
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("ivy-target-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn write(&self, path: &str, bytes: &[u8]) {
        let file = self.0.join(path);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, bytes).unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn path(path: &Path) -> IvyJson {
    IvyJson::String(node_path::from_native(
        &path.components().collect::<PathBuf>(),
    ))
}
fn native(value: &IvyJson) -> PathBuf {
    node_path::filesystem_path(value.as_js_string().unwrap())
}
fn operation(result: IvyResult<IvyJson>) -> IvyJson {
    match result {
        Ok(value) => IvyJson::object([("ok", true.into()), ("value", value)]),
        Err(error) => IvyJson::object([("ok", false.into()), ("error", error.to_wire())]),
    }
}
fn assert_json_eq(actual: &IvyJson, expected: &IvyJson) {
    assert_eq!(
        json::canonical(actual, usize::MAX).unwrap(),
        json::canonical(expected, usize::MAX).unwrap()
    );
}
fn revision(value: &IvyJson) {
    assert_eq!(
        uuid::Uuid::parse_str(&value.as_str().unwrap())
            .unwrap()
            .get_version_num(),
        4
    );
}
fn timestamp(value: &IvyJson) {
    chrono::DateTime::parse_from_rfc3339(&value.as_str().unwrap()).unwrap();
}
fn error_code(value: &IvyJson) -> String {
    value
        .required("operation")
        .unwrap()
        .required("error")
        .unwrap()
        .required("data")
        .unwrap()
        .required("code")
        .unwrap()
        .as_str()
        .unwrap()
}
fn published(value: &IvyJson) -> &IvyJson {
    assert_eq!(
        value.required("operation").unwrap().get("ok"),
        Some(&true.into()),
        "{value:?}"
    );
    value
        .required("operation")
        .unwrap()
        .required("value")
        .unwrap()
        .required("config")
        .unwrap()
}
struct Inputs {
    root: PathBuf,
    host: IvyJson,
    candidate: IvyJson,
    manifest: IvyJson,
    home: JsString,
}
fn inputs(fixture: &Fixture, name: &str, component: &str) -> Inputs {
    let root = fixture.0.join(name);
    fs::create_dir_all(root.join("artifact/native")).unwrap();
    fs::write(
        root.join("artifact/native/runtime.exe"),
        b"selected native bytes",
    )
    .unwrap();
    let settings = if component == "phone-bridge" {
        IvyJson::object([
            (
                "native",
                IvyJson::object([
                    ("executable", "native/runtime.exe".into()),
                    (
                        "executableHash",
                        format!("sha256:{}", "0".repeat(64)).into(),
                    ),
                    ("retained", "native option".into()),
                ]),
            ),
            ("retained", IvyJson::String(JsString(vec![97, 0xd800, 98]))),
        ])
    } else {
        IvyJson::object([("retained", "new settings".into())])
    };
    let host = IvyJson::object([
        ("schemaVersion", IvyJson::Number(1.0)),
        ("hostId", "fixture-host".into()),
        ("runtimeRoot", path(&root.join("runtime"))),
        ("artifactRoot", path(&root.join("artifacts"))),
        ("stagingRoot", path(&root.join("staging"))),
        ("publicBaseUrl", "http://127.0.0.1:39081/ivy".into()),
        (
            "executables",
            IvyJson::object([("node", path(&root.join("fixture-node.exe")))]),
        ),
        (
            "instances",
            IvyJson::Array(vec![IvyJson::object([
                ("instanceId", "logical-ü-😀".into()),
                ("serviceNodeId", "fixture-node".into()),
                ("componentId", component.into()),
                ("enabled", true.into()),
                ("engine", "process".into()),
                ("credential", "synthetic-current".into()),
                ("settings", settings),
            ])]),
        ),
    ]);
    let build = format!("sha256:{}", "a".repeat(64));
    let candidate = IvyJson::object([
        ("candidateId", build.clone().into()),
        ("componentId", component.into()),
        ("buildId", build.clone().into()),
        ("artifactRoot", path(&root.join("artifact"))),
        ("manifestPath", path(&root.join("component.json"))),
        ("createdAt", "2026-01-01T00:00:00.000Z".into()),
        (
            "platform",
            IvyJson::object([
                ("os", if cfg!(windows) { "win32" } else { "linux" }.into()),
                ("arch", "x64".into()),
                ("node", "v24.18.0".into()),
            ]),
        ),
    ]);
    let command = IvyJson::object([
        ("executable", "node".into()),
        ("args", IvyJson::Array(vec!["dist/main.js".into()])),
        ("timeoutMs", IvyJson::Number(10000.0)),
    ]);
    let manifest = IvyJson::object([
        ("schemaVersion", IvyJson::Number(1.0)),
        ("componentId", component.into()),
        ("kind", "service".into()),
        ("buildId", build.into()),
        ("version", "1.2.3".into()),
        ("connectsToHive", false.into()),
        (
            "requirements",
            IvyJson::object([
                ("node", ">=24.18.0 <25.0.0".into()),
                ("hiveProtocol", IvyJson::Null),
                ("contracts", IvyJson::Array(vec![])),
            ]),
        ),
        ("entrypoint", command.clone()),
        (
            "readiness",
            IvyJson::object([
                ("timeoutMs", IvyJson::Number(10000.0)),
                ("command", command),
            ]),
        ),
        (
            "shutdown",
            IvyJson::object([("timeoutMs", IvyJson::Number(5000.0))]),
        ),
        (
            "restart",
            IvyJson::object([
                ("policy", "never".into()),
                ("minimumDelayMs", IvyJson::Number(100.0)),
                ("maximumDelayMs", IvyJson::Number(1000.0)),
            ]),
        ),
    ]);
    Inputs {
        root,
        host,
        candidate,
        manifest,
        home: node_path::from_native(&fixture.0.join("explicit-home")),
    }
}
fn instance(host: &IvyJson) -> &IvyJson {
    &host.required("instances").unwrap().as_array().unwrap()[0]
}
fn set_instance(host: &mut IvyJson, key: &str, value: IvyJson) {
    let mut instance = instance(host).clone();
    instance.insert(key, value);
    host.insert("instances", IvyJson::Array(vec![instance]));
}
fn files(input: &Inputs, operation: IvyJson) -> IvyJson {
    let runtime = native(input.host.required("runtimeRoot").unwrap());
    let id = instance(&input.host)
        .required("instanceId")
        .unwrap()
        .as_js_string()
        .unwrap();
    let directory = node_path::filesystem_path(&node_path::join(
        node_path::Style::native(),
        &[
            node_path::from_native(&runtime),
            "instances".into(),
            id.clone(),
            "configurations".into(),
        ],
    ));
    let mut written = Vec::new();
    if directory.exists() {
        for entry in fs::read_dir(&directory).unwrap() {
            let entry = entry.unwrap();
            written.push(IvyJson::object([
                ("revision", entry.file_name().to_str().unwrap().into()),
                (
                    "config",
                    json::parse(
                        &fs::read_to_string(entry.path().join("config.json")).unwrap(),
                        usize::MAX,
                    )
                    .unwrap(),
                ),
            ]));
        }
    }
    let mut accepted = Vec::new();
    let directory = runtime.join("accepted-configurations");
    if directory.is_dir() {
        for entry in fs::read_dir(directory).unwrap() {
            let entry = entry.unwrap();
            accepted.push(IvyJson::object([
                (
                    "revision",
                    entry
                        .file_name()
                        .to_str()
                        .unwrap()
                        .strip_suffix(".instance.json")
                        .unwrap()
                        .into(),
                ),
                (
                    "config",
                    json::parse(&fs::read_to_string(entry.path()).unwrap(), usize::MAX).unwrap(),
                ),
            ]));
        }
    }
    IvyJson::object([
        ("operation", operation),
        ("written", IvyJson::Array(written)),
        ("accepted", IvyJson::Array(accepted)),
        ("selected", IvyJson::Null),
    ])
}
async fn materialize(input: &Inputs, enabled: bool, basis: Option<IvyJson>) -> IvyJson {
    let journal = HostJournal::open(input.host.clone()).await.unwrap();
    journal
        .save_candidate(input.candidate.clone(), input.manifest.clone())
        .await
        .unwrap();
    let id = instance(&input.host)
        .required("instanceId")
        .unwrap()
        .as_js_string()
        .unwrap()
        .clone();
    let candidate = input
        .candidate
        .required("candidateId")
        .unwrap()
        .as_str()
        .unwrap();
    let result = target::materialize_target(
        journal.clone(),
        id,
        candidate,
        enabled,
        basis,
        input.home.clone(),
    )
    .await
    .map(|value| IvyJson::object([("target", value.target), ("config", value.config)]));
    let selected: i64 = Connection::open(
        native(input.host.required("runtimeRoot").unwrap()).join("deployments.sqlite"),
    )
    .unwrap()
    .query_row("SELECT COUNT(*) FROM runtime_targets", [], |row| row.get(0))
    .unwrap();
    assert_eq!(selected, 0);
    journal.close().await.unwrap();
    let actual = files(input, operation(result));
    if actual.required("operation").unwrap().get("ok") == Some(&true.into()) {
        let value = actual
            .required("operation")
            .unwrap()
            .required("value")
            .unwrap();
        let target = value.required("target").unwrap();
        let config = value.required("config").unwrap();
        validate_host("RuntimeTarget", target).unwrap();
        validate_host("InstanceConfig", config).unwrap();
        revision(target.required("revision").unwrap());
        timestamp(target.required("requestedAt").unwrap());
        assert_eq!(
            target.get("desired"),
            Some(&if enabled { "running" } else { "stopped" }.into())
        );
        assert_eq!(
            target.get("candidateId"),
            input.candidate.get("candidateId")
        );
        assert_eq!(
            target.get("instanceId"),
            instance(&input.host).get("instanceId")
        );
        let written = actual.required("written").unwrap().as_array().unwrap();
        assert_eq!(written.len(), 1);
        assert_eq!(written[0].get("revision"), target.get("revision"));
        assert_json_eq(written[0].required("config").unwrap(), config);
        assert_json_eq(
            actual.required("written").unwrap(),
            actual.required("accepted").unwrap(),
        );
        let written_path = native(target.required("configPath").unwrap());
        assert_json_eq(
            &json::parse(&fs::read_to_string(written_path).unwrap(), usize::MAX).unwrap(),
            config,
        );
    }
    actual
}
#[tokio::test]
async fn immutable_targets_preserve_account_infrastructure_phone_settings_and_running_stopped_selection()
 {
    for (component, enabled) in [
        ("fixture", true),
        ("host-executor", false),
        ("service-manager", true),
        ("agent-manager", false),
        ("phone-bridge", true),
    ] {
        let fixture = Fixture::new();
        let input = inputs(&fixture, "actual", component);
        let result = materialize(&input, enabled, None).await;
        let config = published(&result);
        for key in ["hostId", "publicBaseUrl"] {
            assert_eq!(config.get(key), input.host.get(key));
        }
        for key in ["instanceId", "componentId", "serviceNodeId", "credential"] {
            assert_eq!(config.get(key), instance(&input.host).get(key));
        }
        for key in ["artifactRoot", "buildId"] {
            assert_eq!(config.get(key), input.candidate.get(key));
        }
        assert_eq!(config.get("version"), input.manifest.get("version"));
        for (key, directory) in [
            ("dataRoot", "data"),
            ("workRoot", "work"),
            ("logsRoot", "logs"),
        ] {
            assert_eq!(
                config.get(key),
                Some(&path(
                    &input
                        .root
                        .join("runtime/instances/logical-ü-😀")
                        .join(directory)
                ))
            );
        }
        let settings = config.required("settings").unwrap();
        assert_eq!(
            settings.get("retained"),
            instance(&input.host)
                .required("settings")
                .unwrap()
                .get("retained")
        );
        match component {
            "host-executor" | "service-manager" => assert_eq!(
                settings.get("hostConfigPath"),
                Some(&path(&input.root.join("runtime/host.json")))
            ),
            "agent-manager" => {
                let home = node_path::to_native(&input.home);
                for (key, suffix) in [
                    ("projectRoot", "projects"),
                    ("internalProjectRoot", ".ivy/codex-projects"),
                    ("skillsRoot", ".codex/skills"),
                ] {
                    assert_eq!(settings.get(key), Some(&path(&home.join(suffix))));
                }
            }
            "phone-bridge" => assert_eq!(
                settings.required("native").unwrap().get("executableHash"),
                Some(&hash::digest(b"selected native bytes").into())
            ),
            _ => assert_json_eq(
                settings,
                instance(&input.host).required("settings").unwrap(),
            ),
        }
    }
    for component in ["host-executor", "service-manager", "agent-manager"] {
        let fixture = Fixture::new();
        let mut input = inputs(&fixture, "actual", component);
        let settings = if component == "agent-manager" {
            IvyJson::object([
                (
                    "appServer",
                    IvyJson::object([("mode", "claude-adapter".into())]),
                ),
                ("codexHome", path(&input.root.join("claude-home"))),
            ])
        } else {
            IvyJson::object([(
                "hostConfigPath",
                path(&input.root.join("explicit-host.json")),
            )])
        };
        set_instance(&mut input.host, "settings", settings.clone());
        let result = materialize(&input, true, None).await;
        let actual = published(&result).required("settings").unwrap();
        if component == "agent-manager" {
            assert_eq!(actual.get("codexHome"), settings.get("codexHome"));
            assert_eq!(actual.get("appServer"), settings.get("appServer"));
            assert!(actual.get("skillsRoot").is_none());
            let home = node_path::to_native(&input.home);
            assert_eq!(
                actual.get("projectRoot"),
                Some(&path(&home.join("projects")))
            );
            assert_eq!(
                actual.get("internalProjectRoot"),
                Some(&path(&home.join(".ivy/codex-projects")))
            );
        } else {
            assert_json_eq(actual, &settings);
        }
    }
    for component in ["host-executor", "service-manager"] {
        let fixture = Fixture::new();
        let mut input = inputs(&fixture, "actual", component);
        let config_path = node_path::join(
            node_path::Style::native(),
            &[
                node_path::from_native(&input.root),
                JsString(vec![0xd800, 46, 106, 115, 111, 110]),
            ],
        );
        input
            .host
            .insert("configPath", IvyJson::String(config_path.clone()));
        let result = materialize(&input, true, None).await;
        assert_eq!(
            published(&result)
                .required("settings")
                .unwrap()
                .get("hostConfigPath"),
            Some(&IvyJson::String(config_path))
        );
    }
}
fn basis(input: &Inputs) -> IvyJson {
    IvyJson::object([
        ("schemaVersion", IvyJson::Number(1.0)),
        ("hostId", input.host.required("hostId").unwrap().clone()),
        (
            "instanceId",
            instance(&input.host)
                .required("instanceId")
                .unwrap()
                .clone(),
        ),
        ("serviceNodeId", "accepted-old-node".into()),
        (
            "componentId",
            instance(&input.host)
                .required("componentId")
                .unwrap()
                .clone(),
        ),
        ("publicBaseUrl", "http://127.0.0.1:39082/previous".into()),
        ("dataRoot", path(&input.root.join("old-data"))),
        ("workRoot", path(&input.root.join("old-work"))),
        ("logsRoot", path(&input.root.join("old-logs"))),
        ("artifactRoot", path(&input.root.join("old-artifact"))),
        ("buildId", format!("sha256:{}", "b".repeat(64)).into()),
        ("version", "0.9.0".into()),
        ("credential", "synthetic-accepted-old".into()),
        (
            "settings",
            instance(&input.host).required("settings").unwrap().clone(),
        ),
    ])
}
#[tokio::test]
async fn target_ids_preserve_utf16_and_refuse_nul_at_the_filesystem_boundary() {
    for id in [
        JsString(vec![114, 97, 119, 45, 0xd800]),
        JsString(vec![114, 97, 119, 45, 0xdfff]),
        JsString(vec![114, 97, 119, 0]),
    ] {
        let fixture = Fixture::new();
        let mut input = inputs(&fixture, "actual", "fixture");
        set_instance(&mut input.host, "instanceId", IvyJson::String(id.clone()));
        let result = materialize(&input, true, None).await;
        if id.0.contains(&0) {
            assert_eq!(
                result.required("operation").unwrap().get("ok"),
                Some(&false.into())
            );
            assert!(
                result
                    .required("written")
                    .unwrap()
                    .as_array()
                    .unwrap()
                    .is_empty()
            );
            assert!(
                result
                    .required("accepted")
                    .unwrap()
                    .as_array()
                    .unwrap()
                    .is_empty()
            );
        } else {
            assert_eq!(
                published(&result).get("instanceId"),
                Some(&IvyJson::String(id))
            );
        }
    }
}
#[tokio::test]
async fn rollback_targets_retain_the_complete_accepted_basis_and_rebind_only_selected_release_identity()
 {
    for component in ["fixture", "phone-bridge"] {
        for credential in [Some(IvyJson::from("synthetic-accepted-old")), None] {
            let fixture = Fixture::new();
            let input = inputs(&fixture, "actual", component);
            let mut previous = basis(&input);
            if credential.is_none() {
                previous.remove("credential");
            }
            let result = materialize(&input, false, Some(previous.clone())).await;
            let config = published(&result);
            for key in [
                "credential",
                "serviceNodeId",
                "publicBaseUrl",
                "dataRoot",
                "workRoot",
                "logsRoot",
            ] {
                assert_eq!(config.get(key), previous.get(key), "{key}");
            }
            assert_eq!(config.get("version"), Some(&"1.2.3".into()));
            assert_eq!(
                config.get("artifactRoot"),
                input.candidate.get("artifactRoot")
            );
            assert_eq!(config.get("buildId"), input.candidate.get("buildId"));
            let mut expected = previous;
            for key in ["version", "artifactRoot", "buildId"] {
                expected.insert(key, config.required(key).unwrap().clone());
            }
            if component == "phone-bridge" {
                let mut settings = expected.required("settings").unwrap().clone();
                let mut native = settings.required("native").unwrap().clone();
                native.insert(
                    "executableHash",
                    hash::digest(b"selected native bytes").into(),
                );
                settings.insert("native", native);
                expected.insert("settings", settings);
            }
            assert_json_eq(config, &expected);
        }
    }
}
#[tokio::test]
async fn mismatched_basis_is_rejected_before_output_and_snapshot_failure_keeps_the_partial_file() {
    for kind in ["hostId", "instanceId", "componentId", "snapshot-file"] {
        let fixture = Fixture::new();
        let input = inputs(&fixture, "actual", "fixture");
        let previous = if kind == "snapshot-file" {
            let root = native(input.host.required("runtimeRoot").unwrap());
            fs::create_dir_all(&root).unwrap();
            fs::write(root.join("accepted-configurations"), b"occupied").unwrap();
            None
        } else {
            let mut previous = basis(&input);
            previous.insert(kind, "wrong".into());
            Some(previous)
        };
        let result = materialize(&input, true, previous).await;
        assert_eq!(
            error_code(&result),
            if kind == "snapshot-file" {
                "internal_error"
            } else {
                "target_conflict"
            }
        );
        assert_eq!(
            result
                .required("written")
                .unwrap()
                .as_array()
                .unwrap()
                .len(),
            usize::from(kind == "snapshot-file")
        );
        assert!(
            result
                .required("accepted")
                .unwrap()
                .as_array()
                .unwrap()
                .is_empty()
        );
    }
    for previous in [
        false.into(),
        IvyJson::Number(0.0),
        "".into(),
        IvyJson::Array(vec![]),
    ] {
        let fixture = Fixture::new();
        let input = inputs(&fixture, "actual", "fixture");
        let result = materialize(&input, true, Some(previous)).await;
        assert_eq!(error_code(&result), "target_conflict");
        assert!(
            result
                .required("written")
                .unwrap()
                .as_array()
                .unwrap()
                .is_empty()
        );
        assert!(
            result
                .required("accepted")
                .unwrap()
                .as_array()
                .unwrap()
                .is_empty()
        );
    }
    let fixture = Fixture::new();
    let mut input = inputs(&fixture, "actual", "fixture");
    let mut configured = instance(&input.host).clone();
    configured.remove("credential");
    input
        .host
        .insert("instances", IvyJson::Array(vec![configured]));
    let result = materialize(&input, true, Some(IvyJson::Null)).await;
    assert!(published(&result).get("credential").is_none());
}
#[tokio::test]
async fn native_settings_resolve_and_hash_only_the_selected_artifact_and_preserve_raw_utf16() {
    let fixture = Fixture::new();
    fixture.write("candidate/nested/runtime.exe", b"native bytes");
    fixture.write("outside/escape.exe", b"outside bytes");
    filesystem::directory_link(
        &fixture.0.join("outside"),
        &fixture.0.join("candidate/escape"),
    )
    .unwrap();
    for (executable, expected) in [
        (IvyJson::from("nested/runtime.exe"), None),
        ("escape/escape.exe".into(), Some("target_conflict")),
        ("../outside/escape.exe".into(), Some("target_conflict")),
        ("nested".into(), Some("target_conflict")),
        ("missing".into(), Some("internal_error")),
        ("".into(), Some("invalid_arguments")),
        (
            path(&fixture.0.join("outside/escape.exe")),
            Some("invalid_arguments"),
        ),
        (IvyJson::Null, Some("invalid_arguments")),
    ] {
        let settings = IvyJson::object([
            (
                "native",
                IvyJson::object([
                    ("executable", executable),
                    ("executableHash", "old hash".into()),
                    ("raw", IvyJson::String(JsString(vec![0xd800]))),
                ]),
            ),
            ("raw", IvyJson::String(JsString(vec![0xdfff]))),
        ]);
        let result = target::candidate_bound_settings(
            "phone-bridge".into(),
            settings.clone(),
            fixture.0.join("candidate"),
        )
        .await;
        if let Some(code) = expected {
            assert_eq!(result.unwrap_err().code, code);
        } else {
            let mut expected = settings.clone();
            let mut native = expected.required("native").unwrap().clone();
            native.insert("executableHash", hash::digest(b"native bytes").into());
            expected.insert("native", native);
            assert_json_eq(&result.unwrap(), &expected);
        }
        assert_json_eq(
            &target::candidate_bound_settings(
                "fixture".into(),
                settings.clone(),
                fixture.0.join("absent"),
            )
            .await
            .unwrap(),
            &settings,
        );
    }
    assert_eq!(
        fs::read(fixture.0.join("outside/escape.exe")).unwrap(),
        b"outside bytes"
    );
}
#[tokio::test]
async fn stopped_targets_replace_only_launch_nonce_desire_and_time() {
    let input = IvyJson::object([
        ("instanceId", "fixture".into()),
        ("candidateId", "retained candidate".into()),
        ("configPath", "retained config".into()),
        ("revision", "previous launch".into()),
        ("desired", "running".into()),
        ("requestedAt", "previous time".into()),
        ("retained", IvyJson::String(JsString(vec![0xd800]))),
    ]);
    let actual = target::stopped_target(&input);
    revision(actual.required("revision").unwrap());
    timestamp(actual.required("requestedAt").unwrap());
    assert_eq!(actual.get("desired"), Some(&"stopped".into()));
    let mut retained = actual;
    for key in ["revision", "desired", "requestedAt"] {
        retained.insert(key, input.required(key).unwrap().clone());
    }
    assert_json_eq(&retained, &input);
    assert_eq!(input.get("revision"), Some(&"previous launch".into()));
}
#[tokio::test]
async fn admitted_materialization_finishes_both_files_after_its_waiter_is_dropped() {
    use std::{
        future::{Future, poll_fn},
        task::Poll,
        time::Duration,
    };
    let fixture = Fixture::new();
    let input = inputs(&fixture, "actual", "fixture");
    let journal = HostJournal::open(input.host.clone()).await.unwrap();
    journal
        .save_candidate(input.candidate.clone(), input.manifest.clone())
        .await
        .unwrap();
    let mut waiter = Box::pin(target::materialize_target(
        journal.clone(),
        instance(&input.host)
            .required("instanceId")
            .unwrap()
            .as_js_string()
            .unwrap()
            .clone(),
        input
            .candidate
            .required("candidateId")
            .unwrap()
            .as_str()
            .unwrap(),
        true,
        None,
        input.home.clone(),
    ));
    poll_fn(|cx| {
        assert!(matches!(waiter.as_mut().poll(cx), Poll::Pending));
        Poll::Ready(())
    })
    .await;
    drop(waiter);
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let directory =
                native(input.host.required("runtimeRoot").unwrap()).join("accepted-configurations");
            let ready = fs::read_dir(directory)
                .ok()
                .and_then(|mut rows| rows.next())
                .and_then(Result::ok)
                .and_then(|row| fs::read_to_string(row.path()).ok())
                .and_then(|text| json::parse(&text, usize::MAX).ok())
                .is_some();
            if ready {
                let observed = files(&input, operation(Ok(IvyJson::Null)));
                assert_eq!(observed.get("written"), observed.get("accepted"));
                let revision = observed.required("accepted").unwrap().as_array().unwrap()[0]
                    .required("revision")
                    .unwrap()
                    .as_str()
                    .unwrap();
                let accepted = accepted_configuration::accepted_instance_configuration_path(
                    &input.host,
                    &revision,
                )
                .unwrap();
                assert_eq!(
                    accepted_configuration::read_accepted_instance_configuration(
                        &input.host,
                        &accepted
                    )
                    .unwrap(),
                    observed.required("written").unwrap().as_array().unwrap()[0]
                        .required("config")
                        .unwrap()
                        .clone()
                );
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    journal.close().await.unwrap();
}

#[path = "host-service-storage.rs"]
mod service_storage_cases;
