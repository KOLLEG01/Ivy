use ivy_contracts::{IvyJson, JsString};
use ivy_host_runtime::{config, node_runtime::NodeRuntime};
use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

pub(crate) struct ServiceFixture(pub(crate) PathBuf);
impl ServiceFixture {
    pub(crate) fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "ivy-config-{}-{:x}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NEXT.fetch_add(1, Ordering::Relaxed),
        ));
        fs::create_dir_all(root.join("artifact/dist")).unwrap();
        Self(root)
    }
    pub(crate) fn value(&self) -> IvyJson {
        IvyJson::object([
            ("schemaVersion", IvyJson::Number(1.0)),
            ("hostId", "host".into()),
            ("instanceId", "renamed-service".into()),
            ("serviceNodeId", "host.service".into()),
            ("componentId", "service".into()),
            ("publicBaseUrl", "https://example.invalid/ivy".into()),
            (
                "dataRoot",
                self.0.join("data").to_string_lossy().to_string().into(),
            ),
            (
                "artifactRoot",
                self.0.join("artifact").to_string_lossy().to_string().into(),
            ),
            (
                "workRoot",
                self.0.join("work").to_string_lossy().to_string().into(),
            ),
            (
                "logsRoot",
                self.0.join("logs").to_string_lossy().to_string().into(),
            ),
            ("buildId", format!("sha256:{}", "a".repeat(64)).into()),
            ("version", "1.0.0".into()),
            ("credential", "fixture-credential".into()),
            (
                "settings",
                IvyJson::object([("text", IvyJson::String(JsString(vec![0xd800])))]),
            ),
        ])
    }
    pub(crate) async fn save(&self, value: IvyJson) {
        config::atomic_json(self.0.join("config.json"), value)
            .await
            .unwrap();
    }
    pub(crate) async fn build(&self, value: &IvyJson) {
        config::atomic_json(
            self.0.join("artifact/dist/build-info.json"),
            IvyJson::object([
                ("buildId", value.required("buildId").unwrap().clone()),
                ("version", value.required("version").unwrap().clone()),
            ]),
        )
        .await
        .unwrap();
    }
}
impl Drop for ServiceFixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
pub(crate) fn fixture_node(version: &str) -> NodeRuntime {
    NodeRuntime {
        executable: PathBuf::from("unused-fixture-node"),
        version: version.into(),
    }
}

pub(crate) fn host_configuration(fixture: &ServiceFixture) -> IvyJson {
    IvyJson::object([
        ("schemaVersion", IvyJson::Number(1.0)),
        ("hostId", "fixture-host".into()),
        (
            "runtimeRoot",
            fixture.0.join("state").to_string_lossy().to_string().into(),
        ),
        (
            "artifactRoot",
            fixture
                .0
                .join("artifacts")
                .to_string_lossy()
                .to_string()
                .into(),
        ),
        (
            "stagingRoot",
            fixture
                .0
                .join("staging")
                .to_string_lossy()
                .to_string()
                .into(),
        ),
        ("publicBaseUrl", "http://127.0.0.1:39081/ivy".into()),
        (
            "executables",
            IvyJson::object([("node", "fixture-node".into())]),
        ),
        (
            "instances",
            IvyJson::Array(vec![IvyJson::object([
                ("instanceId", "fixture".into()),
                ("serviceNodeId", "fixture-node".into()),
                ("componentId", "fixture".into()),
                ("engine", "process".into()),
                ("enabled", true.into()),
                ("settings", IvyJson::object(Vec::<(String, IvyJson)>::new())),
            ])]),
        ),
    ])
}
