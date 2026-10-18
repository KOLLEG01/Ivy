#[path = "../../packages/host-runtime/rust/src/paths.rs"]
mod paths;
use std::{fs, path::PathBuf};

#[test]
fn containment_normalizes_parents_and_rejects_sibling_prefixes() {
    let root = std::env::temp_dir().join("ivy-path-test");
    assert!(paths::inside(&root, &root.join("a/../b")).unwrap());
    assert!(paths::inside(&root, &root).unwrap());
    assert!(!paths::inside(&root, &root.join("../other")).unwrap());
    assert!(!paths::inside(&root, &PathBuf::from(format!("{}-sibling", root.display()))).unwrap());
}

#[test]
fn future_path_uses_the_existing_ancestor_without_creating_directories() {
    let root = std::env::temp_dir().join(format!("ivy-future-{}", std::process::id()));
    fs::create_dir(&root).unwrap();
    let expected = fs::canonicalize(&root).unwrap().join("missing/leaf");
    assert_eq!(
        paths::resolved_future_path(&root.join("missing/leaf")).unwrap(),
        expected
    );
    assert!(!root.join("missing").exists());
    fs::remove_dir(&root).unwrap();
}

#[cfg(windows)]
#[test]
fn windows_containment_ignores_case() {
    assert!(
        paths::inside(
            std::path::Path::new("C:\\Local\\Owned"),
            std::path::Path::new("c:\\local\\owned\\child")
        )
        .unwrap()
    );
}

use ivy_contracts::{IvyJson, JsString};
use ivy_host_runtime::{config, health::HealthFile, node_runtime::NodeRuntime};

struct ServiceFixture(PathBuf);
impl ServiceFixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("ivy-config-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(root.join("artifact/dist")).unwrap();
        Self(root)
    }
    fn value(&self) -> IvyJson {
        IvyJson::object([
            ("schemaVersion", IvyJson::Number(1.0)),
            ("hostId", "host".into()), ("instanceId", "renamed-service".into()),
            ("serviceNodeId", "host.service".into()), ("componentId", "service".into()),
            ("publicBaseUrl", "https://example.invalid/ivy".into()),
            ("dataRoot", self.0.join("data").to_string_lossy().to_string().into()),
            ("artifactRoot", self.0.join("artifact").to_string_lossy().to_string().into()),
            ("workRoot", self.0.join("work").to_string_lossy().to_string().into()),
            ("logsRoot", self.0.join("logs").to_string_lossy().to_string().into()),
            ("buildId", format!("sha256:{}", "a".repeat(64)).into()),
            ("version", "1.0.0".into()), ("credential", "fixture-credential".into()),
            ("settings", IvyJson::object([("text", IvyJson::String(JsString(vec![0xd800])))])),
        ])
    }
    async fn save(&self, value: IvyJson) {
        config::atomic_json(self.0.join("config.json"), value).await.unwrap();
    }
    async fn build(&self, value: &IvyJson) {
        config::atomic_json(self.0.join("artifact/dist/build-info.json"), IvyJson::object([
            ("buildId", value.required("buildId").unwrap().clone()),
            ("version", value.required("version").unwrap().clone()),
        ])).await.unwrap();
    }
}
impl Drop for ServiceFixture { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); } }
fn fixture_node(version: &str) -> NodeRuntime {
    NodeRuntime { executable: PathBuf::from("unused-fixture-node"), version: version.into() }
}

#[tokio::test]
async fn instance_configuration_checks_build_before_creating_runtime_roots() {
    let fixture = ServiceFixture::new();
    let value = fixture.value();
    fixture.save(value.clone()).await;
    let mut wrong_build = value.clone(); wrong_build.insert("version", "2.0.0".into());
    fixture.build(&wrong_build).await;
    let path = fixture.0.join("config.json");
    let result = config::instance_config(&path, &fixture_node("24.18.0")).await;
    assert_eq!(result.err().unwrap().code, "build_mismatch");
    assert!(!fixture.0.join("data").exists());
    fixture.build(&value).await;
    for version in ["22.0.0", "24.17.9", "25.0.0"] {
        assert_eq!(config::instance_config(&path, &fixture_node(version)).await.err().unwrap().code, "unsupported_runtime");
        assert!(!fixture.0.join("data").exists());
    }
    let actual = config::instance_config(&path, &fixture_node("24.18.0")).await.unwrap();
    assert_eq!(actual.json(), &value);
    for root in ["data", "work", "logs"] { assert!(fixture.0.join(root).is_dir()); }
    for root in [fixture.0.join("artifact"), fixture.0.join("artifact/nested"), fixture.0.clone(), PathBuf::from("relative")] {
        let mut invalid = value.clone(); invalid.insert("dataRoot", root.to_string_lossy().to_string().into());
        fixture.save(invalid).await;
        assert_eq!(config::instance_config(&path, &fixture_node("24.18.0")).await.err().unwrap().code, "invalid_arguments");
    }
    for key in ["workRoot", "logsRoot"] {
        let mut invalid = value.clone(); invalid.insert(key, fixture.0.join("artifact/nested").to_string_lossy().to_string().into());
        fixture.save(invalid).await;
        assert_eq!(config::instance_config(&path, &fixture_node("24.18.0")).await.err().unwrap().code, "invalid_arguments");
    }
}

#[tokio::test]
async fn configuration_selection_and_file_boundary_preserve_original_rules() {
    let fixture = ServiceFixture::new(); let path = fixture.0.join("config.json");
    let env = Some(path.clone().into_os_string());
    assert_eq!(config::configuration_path(&[], env.clone()).unwrap(), path);
    assert!(config::configuration_path(&["--config".into()], env.clone()).is_err());
    assert!(config::configuration_path(&["--config".into(), "relative".into()], env.clone()).is_err());
    assert_eq!(config::configuration_path(&["--config".into(), path.clone().into_os_string(), "--config".into(), "relative".into()], None).unwrap(), path);
    fs::write(&path, [b'"', 0xff, b'"']).unwrap();
    assert_eq!(config::json_file(&path, 3).await.unwrap(), IvyJson::from("\u{fffd}"));
    assert_eq!(config::json_file(&path, 2).await.unwrap_err().code, "invalid_arguments");
    fs::write(&path, "{bad").unwrap();
    assert_eq!(config::json_file(&path, 100).await.unwrap_err().code, "internal_error");
    assert_eq!(config::json_file(&fixture.0, 100).await.unwrap_err().code, "invalid_arguments");
    let mut value = fixture.value(); value.insert("unexpected", true.into());
    assert!(config::InstanceConfig::from_json(value).is_err());
}

#[tokio::test]
async fn health_control_requires_all_three_identities_and_preserves_utf16_slice() {
    let fixture = ServiceFixture::new();
    let config = config::InstanceConfig::from_json(fixture.value()).unwrap();
    let health = HealthFile::with_launch_id(config.clone(), Some("launch-1".into()));
    assert!(health.control().await.unwrap().is_none());
    let details = JsString(vec![0xd800; 4097]);
    health.write(true, details, Some(7)).await.unwrap();
    let value = config::json_file(fixture.0.join("data/health.json"), 1024 * 1024).await.unwrap();
    assert_eq!(value.required("details").unwrap().as_js_string().unwrap().0, vec![0xd800;4096]);
    assert_eq!(value.get("generation"), Some(&IvyJson::Number(7.0)));
    let before = value.clone();
    health.write(false, "draining".into(), None).await.unwrap();
    let value = config::json_file(fixture.0.join("data/health.json"), 1024 * 1024).await.unwrap();
    for key in ["bootId", "launchId", "startedAt", "pid"] { assert_eq!(value.get(key), before.get(key)); }
    let control = IvyJson::object([
        ("schemaVersion", IvyJson::Number(1.0)), ("instanceId", "renamed-service".into()),
        ("launchId", "launch-1".into()), ("bootId", health.boot_id().into()),
        ("action", "shutdown".into()), ("requestedAt", "2026-10-18T00:00:00.000Z".into()),
    ]);
    let path = fixture.0.join("data/control.json");
    for key in ["instanceId", "launchId", "bootId"] {
        let mut wrong = control.clone(); wrong.insert(key, "wrong".into());
        config::atomic_json(&path, wrong).await.unwrap();
        assert!(health.control().await.unwrap().is_none());
    }
    config::atomic_json(&path, control.clone()).await.unwrap();
    assert_eq!(health.control().await.unwrap(), Some(control));
    let replacement = HealthFile::with_launch_id(config, Some("launch-1".into()));
    assert!(replacement.control().await.unwrap().is_none());
    fs::write(&path, "{}").unwrap();
    assert!(health.control().await.is_err());
}
