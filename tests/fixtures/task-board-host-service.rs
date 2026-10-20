use crate::frozen_contracts::ContractsBinding;
use ivy_contracts::{
    IvyJson as Json, JsString,
    hash::hash_json,
    json::{parse, stringify},
    limits::MANAGEMENT_FRAME_BYTES,
};
use ivy_host_runtime::{config::InstanceConfig, health::HealthFile, node_runtime::NodeRuntime};
use ivy_task_board::host_service;
use std::{
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, OnceLock},
};

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
    assert_eq!(rows.len(), 9);
    for row in rows {
        let path = fixture.prepare(row);
        let registry = Arc::new(|| {
            Box::pin(async { Ok(Json::object([] as [(&str, Json); 0])) })
                as ivy_sdk::service::ServiceFuture<Json>
        });
        let prepared =
            host_service::prepare(path, &fixture.node, &ContractsBinding, registry, vec![]).await;
        let actual = result(prepared.map(|p| {
            assert!(!p.service.client.ready());
            assert!(!p.health.boot_id().is_empty());
            assert_eq!(
                hash_json(p.config.json()).unwrap(),
                hash_json(row.required("config").unwrap()).unwrap()
            );
            Json::from(hash_json(p.config.json()).unwrap())
        }));
        assert_eq!(
            actual,
            row.required("result").unwrap().clone(),
            "{}",
            row.required("name").unwrap().as_str().unwrap()
        );
        assert_eq!(
            effects(row.required("config").unwrap()),
            row.required("effects").unwrap().clone()
        );
    }
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
