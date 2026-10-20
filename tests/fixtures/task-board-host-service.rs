use crate::frozen_contracts::ContractsBinding;
use ivy_contracts::{
    IvyJson as Json, IvyResult, JsString,
    hash::hash_json,
    json::{parse, stringify},
    limits::MANAGEMENT_FRAME_BYTES,
};
use ivy_host_runtime::{config::InstanceConfig, health::HealthFile, node_runtime::NodeRuntime};
use ivy_task_board::host_service;
use ivy_task_board::journal_store::Contracts;
use std::{
    cell::Cell,
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, OnceLock},
};

#[derive(Default)]
struct StartupContracts {
    calls: Cell<u64>,
    refused: Cell<bool>,
}
impl Contracts for StartupContracts {
    fn version(&self, key: &str) -> IvyResult<String> {
        ContractsBinding.version(key)
    }
    fn local(&self, key: &str) -> IvyResult<bool> {
        ContractsBinding.local(key)
    }
    fn readable_version(&self, key: &str, version: &JsString) -> IvyResult<bool> {
        ContractsBinding.readable_version(key, version)
    }
    fn validate_record(&self, key: &str, version: &JsString, value: &Json) -> IvyResult<()> {
        ContractsBinding.validate_record(key, version, value)
    }
    fn validate_definition(&self, name: &str, value: &Json) -> IvyResult<()> {
        assert_eq!(name, "Settings");
        self.calls.set(self.calls.get() + 1);
        let result = ContractsBinding.validate_definition(name, value);
        self.refused.set(result.is_err());
        result
    }
}

struct Fixture {
    root: PathBuf,
    node: NodeRuntime,
    oracle: Json,
}
impl Fixture {
    async fn new() -> Self {
        let root = std::env::temp_dir().join(format!("task-board-host-{}", uuid::Uuid::new_v4()));
        let distribution = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
        static NODE: OnceLock<PathBuf> = OnceLock::new();
        let executable = NODE.get_or_init(|| {
            let output = Command::new("node")
                .args(["-p", "process.execPath"])
                .output()
                .unwrap();
            assert!(output.status.success());
            PathBuf::from(String::from_utf8(output.stdout).unwrap().trim())
        });
        let node = NodeRuntime::discover(Some(executable)).await.unwrap();
        let output = Command::new(&node.executable)
            .args([
                "--experimental-transform-types",
                "--disable-warning=ExperimentalWarning",
                "tests/fixtures/task-board-host-service-oracle.mjs",
            ])
            .arg(&root)
            .env("IVY_LAUNCH_ID", "fixture-launch")
            .current_dir(distribution)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let oracle = parse(
            std::str::from_utf8(&output.stdout).unwrap(),
            MANAGEMENT_FRAME_BYTES,
        )
        .unwrap();
        Self { root, node, oracle }
    }
    fn prepare(&self, row: &Json) -> PathBuf {
        let path = PathBuf::from(row.required("path").unwrap().as_str().unwrap());
        assert!(path.starts_with(&self.root));
        let config = row.required("config").unwrap();
        let artifact = PathBuf::from(config.required("artifactRoot").unwrap().as_str().unwrap());
        assert!(artifact.starts_with(&self.root));
        std::fs::create_dir_all(artifact.join("dist")).unwrap();
        if row.required("build").unwrap() != &Json::Null {
            write(
                &artifact.join("dist/build-info.json"),
                row.required("build").unwrap(),
            );
        }
        write(&path, config);
        path
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        assert!(
            self.root
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("task-board-host-")
        );
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
fn write(path: &Path, value: &Json) {
    std::fs::write(path, stringify(value, MANAGEMENT_FRAME_BYTES).unwrap()).unwrap();
}
fn result(value: ivy_contracts::IvyResult<Json>) -> Json {
    match value {
        Ok(v) => Json::object([("ok", v)]),
        Err(e) => Json::object([("error", Json::String(e.code_js()))]),
    }
}
fn effects(config: &Json) -> Json {
    let exists = |key: &str| Path::new(&config.required(key).unwrap().as_str().unwrap()).exists();
    Json::object([
        ("data", exists("dataRoot").into()),
        ("work", exists("workRoot").into()),
        ("logs", exists("logsRoot").into()),
        (
            "health",
            PathBuf::from(config.required("dataRoot").unwrap().as_str().unwrap())
                .join("health.json")
                .exists()
                .into(),
        ),
    ])
}

#[tokio::test(flavor = "current_thread")]
async fn instance_configuration_and_taskboard_service_admission_match_original_before_connecting() {
    let fixture = Fixture::new().await;
    let rows = fixture.oracle.required("rows").unwrap().as_array().unwrap();
    assert_eq!(rows.len(), 28);
    let mut mismatches = vec![];
    for row in rows {
        let path = fixture.prepare(row);
        let registry = Arc::new(|| {
            Box::pin(async { Ok(Json::object([] as [(&str, Json); 0])) })
                as ivy_sdk::service::ServiceFuture<Json>
        });
        let contracts = StartupContracts::default();
        let prepared =
            host_service::prepare(path, &fixture.node, &contracts, registry, vec![]).await;
        let actual = match prepared.map(|p| {
            assert!(!p.service.client.ready());
            assert!(!p.health.boot_id().is_empty());
            assert_eq!(
                hash_json(p.config.json()).unwrap(),
                hash_json(row.required("config").unwrap()).unwrap()
            );
            Json::from(hash_json(p.config.json()).unwrap())
        }) {
            Ok(value) => Json::object([("ok", value)]),
            Err(e) => {
                let config = row.required("config").unwrap();
                let identity = config.get("componentId") != Some(&Json::from("task-board"))
                    || config.get("credential").is_none();
                let phase = if contracts.calls.get() == 0 {
                    "host"
                } else if contracts.refused.get() {
                    "settings"
                } else if identity {
                    "identity"
                } else {
                    "transport"
                };
                Json::object([
                    ("error", Json::String(e.code_js())),
                    ("phase", phase.into()),
                    (
                        "message",
                        if phase == "settings" {
                            "$SETTINGS".into()
                        } else {
                            Json::String(e.message_js())
                        },
                    ),
                    ("outcome", e.outcome.as_str().into()),
                ])
            }
        };
        if hash_json(&actual).unwrap() != hash_json(row.required("result").unwrap()).unwrap()
            || row.required("settingsCalls").unwrap() != &Json::Number(contracts.calls.get() as f64)
            || effects(row.required("config").unwrap()) != row.required("effects").unwrap().clone()
        {
            mismatches.push(Json::object([
                ("name", row.required("name").unwrap().clone()),
                ("actual", actual),
                ("expected", row.required("result").unwrap().clone()),
                ("settingsCalls", Json::Number(contracts.calls.get() as f64)),
                (
                    "expectedSettingsCalls",
                    row.required("settingsCalls").unwrap().clone(),
                ),
            ]));
        }
    }
    assert!(
        mismatches.is_empty(),
        "Startup mismatches: {}",
        stringify(&Json::Array(mismatches), MANAGEMENT_FRAME_BYTES).unwrap()
    );
}

#[tokio::test(flavor = "current_thread")]
async fn supplied_health_file_preserves_taskboard_identity_utf16_and_exact_control_authority() {
    let fixture = Fixture::new().await;
    let oracle = fixture.oracle.required("health").unwrap();
    let config = InstanceConfig::from_json(oracle.required("config").unwrap().clone()).unwrap();
    let health = HealthFile::with_launch_id(config.clone(), Some("fixture-launch".into()));
    let details = JsString(
        "x".repeat(4095)
            .encode_utf16()
            .chain([0xd83d, 0xde00])
            .collect(),
    );
    health.write(true, details, Some(7)).await.unwrap();
    let mut written =
        ivy_host_runtime::config::json_file(config.data_root().join("health.json"), 1024 * 1024)
            .await
            .unwrap();
    assert_eq!(
        written.required("pid").unwrap(),
        &Json::Number(std::process::id() as f64)
    );
    assert_eq!(
        written.required("bootId").unwrap(),
        &Json::from(health.boot_id())
    );
    for key in ["startedAt", "observedAt"] {
        assert!(!written.required(key).unwrap().as_str().unwrap().is_empty());
    }
    for key in ["pid", "bootId", "startedAt", "observedAt"] {
        written.remove(key);
    }
    assert_eq!(written, oracle.required("written").unwrap().clone());
    let rows = oracle.required("rows").unwrap().as_array().unwrap();
    assert_eq!(rows.len(), 7);
    for row in rows {
        if let Some(value) = row.get("value") {
            let mut value = value.clone();
            if value.get("bootId") == Some(&Json::from("$BOOT")) {
                value.insert("bootId", health.boot_id().into());
            }
            write(&config.data_root().join("control.json"), &value);
        }
        let actual = result(health.control().await.map(|v| {
            let mut v = v.unwrap_or(Json::Null);
            if v.get("bootId") == Some(&Json::from(health.boot_id())) {
                v.insert("bootId", "$BOOT".into());
            }
            v
        }));
        assert_eq!(
            actual,
            row.required("result").unwrap().clone(),
            "{}",
            row.required("name").unwrap().as_str().unwrap()
        );
    }
}

#[test]
fn base_urls_match_original_constructor_and_policy_error_boundaries() {
    use ivy_contracts::{IvyJson, json::parse};
    use ivy_sdk::rpc::base_url;
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let output = std::process::Command::new("node")
        .args([
            "--experimental-transform-types",
            "--disable-warning=ExperimentalWarning",
            "tests/fixtures/sdk-base-url-oracle.mjs",
        ])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rows: Vec<serde_json::Value> = serde_json::from_slice(&output.stdout).unwrap();
    assert!(rows.len() > 700);
    for row in rows {
        let input = parse(row["input"].as_str().unwrap(), 100_000).unwrap();
        let expected = parse(row["result"].as_str().unwrap(), 100_000).unwrap();
        let value = String::from_utf16_lossy(&input.as_js_string().unwrap().0);
        let actual = match base_url(&value) {
            Ok(url) => IvyJson::object([("ok", true.into()), ("value", url.as_str().into())]),
            Err(error) => IvyJson::object([("ok", false.into()), ("error", error.to_wire())]),
        };
        assert_eq!(actual, expected, "URL input: {}", row["input"]);
    }
}
