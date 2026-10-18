use ivy_host_runtime::paths;
use std::{fs, path::PathBuf};

#[path = "host-journal-cases.rs"]
mod journal_cases;
#[path = "host-artifact-cases.rs"]
mod artifact_cases;
#[path = "host-source-cases.rs"]
mod source_cases;
#[path = "host-action-cases.rs"]
mod action_cases;
#[path = "host-preparation-cases.rs"]
mod preparation_cases;
#[path = "host-storage-space-cases.rs"]
mod storage_space_cases;
#[path = "host-node-path-cases.rs"]
mod node_path_cases;
#[path = "host-layout-cases.rs"]
mod layout_cases;
#[path = "host-startup-cases.rs"]
mod host_startup_cases;

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
    let expected = paths::realpath(&root).unwrap().join("missing/leaf");
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

fn assert_json_eq(actual: &IvyJson, expected: &IvyJson) {
    assert_eq!(ivy_contracts::json::canonical(actual, usize::MAX).unwrap(), ivy_contracts::json::canonical(expected, usize::MAX).unwrap());
}
fn assert_json_eq_with_context(actual: &IvyJson, expected: &IvyJson, context: &IvyJson) {
    assert_eq!(ivy_contracts::json::canonical(actual, usize::MAX).unwrap(), ivy_contracts::json::canonical(expected, usize::MAX).unwrap(), "{context:?}");
}
fn assert_optional_json_eq(actual: Option<IvyJson>, expected: Option<IvyJson>) {
    assert_eq!(actual.as_ref().map(|value| ivy_contracts::json::canonical(value, usize::MAX).unwrap()), expected.as_ref().map(|value| ivy_contracts::json::canonical(value, usize::MAX).unwrap()));
}

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

fn compare_config_oracle(input: IvyJson, result: ivy_contracts::IvyResult<IvyJson>) {
    use std::{io::Write, process::{Command, Stdio}};
    let mut child = Command::new("node")
        .args(["--experimental-transform-types", "--disable-warning=ExperimentalWarning", "tests/fixtures/host-config-oracle.mjs"])
        .current_dir(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.."))
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    child.stdin.take().unwrap().write_all(ivy_contracts::json::stringify(&input, usize::MAX).unwrap().as_bytes()).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let expected = ivy_contracts::json::parse(std::str::from_utf8(&output.stdout).unwrap(), usize::MAX).unwrap();
    let actual = match result {
        Ok(value) => IvyJson::object([("ok", true.into()), ("value", value)]),
        Err(error) => IvyJson::object([("ok", false.into()), ("error", error.to_wire())]),
    };
    assert_eq!(ivy_contracts::json::canonical(&actual, usize::MAX).unwrap(), ivy_contracts::json::canonical(&expected, usize::MAX).unwrap(), "{input:?}");
}

fn stop_fence() -> IvyJson {
    IvyJson::object([
        ("schemaVersion", IvyJson::Number(1.0)), ("owner", "native".into()),
        ("ownerId", "native-owner".into()), ("processId", IvyJson::Number(123.0)),
        ("observedAt", "2026-01-01T00:00:00.000Z".into()), ("code", "outcome_unknown".into()),
    ])
}

fn host_configuration(fixture: &ServiceFixture) -> IvyJson {
    IvyJson::object([
        ("schemaVersion", IvyJson::Number(1.0)), ("hostId", "fixture-host".into()),
        ("runtimeRoot", fixture.0.join("state").to_string_lossy().to_string().into()),
        ("artifactRoot", fixture.0.join("artifacts").to_string_lossy().to_string().into()),
        ("stagingRoot", fixture.0.join("staging").to_string_lossy().to_string().into()),
        ("publicBaseUrl", "http://127.0.0.1:39081/ivy".into()),
        ("executables", IvyJson::object([("node", "fixture-node".into())])),
        ("instances", IvyJson::Array(vec![IvyJson::object([
            ("instanceId", "fixture".into()), ("serviceNodeId", "fixture-node".into()), ("componentId", "fixture".into()),
            ("engine", "process".into()), ("enabled", true.into()), ("settings", IvyJson::object(Vec::<(String, IvyJson)>::new())),
        ])])),
    ])
}
fn accepted_oracle(operation: &str, args: Vec<IvyJson>, result: ivy_contracts::IvyResult<IvyJson>) {
    compare_config_oracle(IvyJson::object([("kind", "accepted".into()), ("operation", operation.into()), ("args", IvyJson::Array(args))]), result);
}

#[test]
fn accepted_configuration_preserves_installation_and_revision_boundaries() {
    use ivy_host_runtime::{accepted_configuration::*, layout::*};
    let fixture = ServiceFixture::new(); let anchor = host_configuration(&fixture);
    for revision in ["launch-1".to_string(), format!("sha256:{}", "a".repeat(64)), "".into(), "../escape".into(), "a".repeat(129), "é".into(), format!("sha256:{}", "A".repeat(64))] {
        accepted_oracle("acceptedInstanceConfigurationPath", vec![anchor.clone(), revision.clone().into()],
            accepted_instance_configuration_path(&anchor, &revision).map(|p| p.to_string_lossy().to_string().into()));
    }
    for mode in ["same", "host", "roots", "node", "identity", "component", "storage", "removed", "settings", "restored", "modern"] {
        let mut next = anchor.clone();
        let mut instances = next.required("instances").unwrap().as_array().unwrap().to_vec();
        match mode {
            "host" => next.insert("hostId", "other".into()),
            "roots" => next.insert("runtimeRoot", fixture.0.join("other").to_string_lossy().to_string().into()),
            "node" => next.insert("executables", IvyJson::object([("node", "different-node".into())])),
            "identity" => instances[0].insert("serviceNodeId", "other".into()),
            "component" => instances[0].insert("componentId", "other".into()),
            "storage" => instances[0].insert("paths", IvyJson::object([("data", "other/data".into()), ("work", "other/work".into()), ("logs", "other/logs".into())])),
            "removed" => instances.clear(),
            "settings" => instances[0].insert("settings", IvyJson::object([("changed", true.into())])),
            "restored" => next.insert("restoredFrom", IvyJson::object([("backupId", "changed".into())])),
            "modern" => next.insert("servicesRoot", fixture.0.join("services").to_string_lossy().to_string().into()),
            _ => (),
        }
        next.insert("instances", IvyJson::Array(instances));
        accepted_oracle("sameInstallation", vec![anchor.clone(), next.clone()], same_installation(&anchor, &next).map(|_| IvyJson::Null));
        accepted_oracle("installationRecord", vec![next.clone()], installation_record(&next).map(Into::into));
    }
    for modern in [false, true] {
        let mut source = anchor.clone(); if modern { source.insert("servicesRoot", fixture.0.join("services").to_string_lossy().to_string().into()); }
        for id in ["fixture", "missing"] {
            compare_config_oracle(IvyJson::object([("kind", "layout".into()), ("operation", "servicePaths".into()), ("args", IvyJson::Array(vec![source.clone(), id.into()]))]), service_paths(&source, &id.into()));
        }
    }
    let mut source = anchor.clone();
    source.insert("runtimeRoot", fixture.0.join("state/../runtime").to_string_lossy().to_string().into());
    compare_config_oracle(IvyJson::object([("kind", "layout".into()), ("operation", "servicePaths".into()), ("args", IvyJson::Array(vec![source.clone(), "fixture".into()]))]), service_paths(&source, &"fixture".into()));
    compare_config_oracle(IvyJson::object([("kind", "layout".into()), ("operation", "hostConfigurationPath".into()), ("args", IvyJson::Array(vec![source.clone()]))]), host_configuration_path(&source).map(|p| p.to_string_lossy().to_string().into()));
}

#[test]
fn accepted_configuration_files_remain_private_immutable_and_direct() {
    use ivy_host_runtime::accepted_configuration::*;
    let fixture = ServiceFixture::new(); let anchor = host_configuration(&fixture);
    let saved = save_accepted_configuration(&anchor).unwrap();
    let expected = format!("{}\n", ivy_contracts::json::canonical(&anchor, usize::MAX).unwrap());
    assert_eq!(fs::read_to_string(&saved).unwrap(), expected);
    accepted_oracle("readAcceptedConfiguration", vec![anchor.clone(), saved.to_string_lossy().to_string().into()], read_accepted_configuration(&anchor, &saved));
    let source = fixture.0.join("state/launch.json"); fs::write(&source, ivy_contracts::json::stringify(&fixture.value(), usize::MAX).unwrap()).unwrap();
    let target = save_accepted_instance_configuration(&anchor, &source, "launch-1").unwrap();
    assert!(save_accepted_instance_configuration(&anchor, &source, "launch-1").is_err());
    fs::write(&source, "null").unwrap();
    accepted_oracle("readAcceptedInstanceConfiguration", vec![anchor.clone(), target.to_string_lossy().to_string().into()], read_accepted_instance_configuration(&anchor, &target));
    for mode in ["missing", "directory", "oversize", "invalid", "outside", "nested", "changed"] {
        let file = match mode {
            "outside" => fixture.0.join("outside.json"),
            "nested" => saved.parent().unwrap().join("nested/test.json"),
            _ => saved.parent().unwrap().join(format!("{mode}.json")),
        };
        if mode == "directory" { fs::create_dir(&file).unwrap(); }
        else if mode != "missing" {
            fs::create_dir_all(file.parent().unwrap()).unwrap();
            let content = if mode == "oversize" { " ".repeat(1024 * 1024 + 1) }
                else if mode == "invalid" { "{".into() }
                else if mode == "changed" { let mut value = anchor.clone(); value.insert("hostId", "other".into()); ivy_contracts::json::canonical(&value, usize::MAX).unwrap() }
                else { expected.clone() };
            fs::write(&file, content).unwrap();
        }
        accepted_oracle("readAcceptedConfiguration", vec![anchor.clone(), file.to_string_lossy().to_string().into()], read_accepted_configuration(&anchor, &file));
    }
    #[cfg(unix)] {
        use std::os::unix::fs::{PermissionsExt, symlink};
        assert_eq!(fs::metadata(&saved).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(fs::metadata(saved.parent().unwrap()).unwrap().permissions().mode() & 0o777, 0o700);
        let link = saved.parent().unwrap().join("link.json"); symlink(&saved, &link).unwrap();
        assert_eq!(read_accepted_configuration(&anchor, &link).unwrap_err().code, "configuration_changed");
    }
}

struct FixedHealthClock(i64);
impl ivy_sdk::clock::Clock for FixedHealthClock {
    fn utc_millis(&self) -> i64 { self.0 }
    fn monotonic(&self) -> std::time::Instant { std::time::Instant::now() }
}
fn hive_settings(host: &str, port: u16) -> IvyJson {
    IvyJson::object([
        ("listenHost", host.into()), ("listenPort", IvyJson::Number(port as f64)),
        ("credentials", IvyJson::Array(vec![IvyJson::object([("principalId", "fixture".into()), ("token", "fixture-credential".into())])])),
        ("backup", IvyJson::object([("directory", "unused".into()), ("intervalHours", IvyJson::Number(24.0)), ("retain", IvyJson::Number(7.0))])),
    ])
}

#[test]
fn local_hive_endpoint_matches_node_url_and_docker_mapping_rules() {
    use ivy_host_runtime::health::local_hive_endpoint;
    for host in ["0.0.0.0", "::", "::1", "127.0.0.1", "localhost", "[::1]", "bad host"] {
        let config = IvyJson::object([("publicBaseUrl", "https://example.invalid:9443/prefix?query=1#fragment".into()), ("settings", hive_settings(host, 39081))]);
        compare_config_oracle(IvyJson::object([("kind", "endpoint".into()), ("config", config.clone())]), local_hive_endpoint(&config, None).map(|url| url.to_string().into()));
    }
    let config = IvyJson::object([("publicBaseUrl", "https://example.invalid/ivy".into()), ("settings", hive_settings("0.0.0.0", 39081))]);
    for hosts in [vec![], vec!["127.0.0.1"], vec!["::"], vec!["127.0.0.1", "::1"], vec!["192.0.2.1"]] {
        let docker = IvyJson::object([("ports", IvyJson::Array(hosts.iter().map(|host| IvyJson::object([
            ("host", (*host).into()), ("port", IvyJson::Number(80.0)), ("containerPort", IvyJson::Number(39081.0)),
        ])).collect()))]);
        compare_config_oracle(IvyJson::object([("kind", "endpoint".into()), ("config", config.clone()), ("docker", docker.clone())]), local_hive_endpoint(&config, Some(&docker)).map(|url| url.to_string().into()));
    }
}

#[tokio::test]
async fn health_readiness_matches_node_time_and_identity_boundaries() {
    use ivy_host_runtime::health::check_health_with_clock;
    let fixture = ServiceFixture::new();
    let config = config::InstanceConfig::from_json(fixture.value()).unwrap();
    let writer = HealthFile::with_launch_id(config.clone(), Some("launch".into()));
    writer.write(true, "ready".into(), Some(7)).await.unwrap();
    let path = config.data_root().join("health.json");
    let baseline = config::json_file(&path, 4096).await.unwrap();
    let now = 1_767_225_600_000_i64;
    for case in 0..13 {
        let mut value = baseline.clone();
        let offset = match case { 1 => -14999, 2 => -15000, 3 => 1000, 4 => 1001, _ => 0 };
        value.insert("observedAt", chrono::DateTime::from_timestamp_millis(now + offset).unwrap().to_rfc3339_opts(chrono::SecondsFormat::Millis, true).into());
        match case {
            5 => { value.insert("observedAt", "invalid".into()); },
            6 => { value.insert("observedAt", "Thu, 01 Jan 2026 00:00:00 GMT".into()); },
            7 => { value.insert("ready", false.into()); },
            8 => { value.insert("instanceId", "different".into()); },
            9 => { value.insert("buildId", format!("sha256:{}", "b".repeat(64)).into()); },
            10 => { value.insert("pid", IvyJson::Number(999.0)); value.insert("bootId", "other".into()); },
            11 => { value.insert("componentId", "other".into()); },
            12 => { value = IvyJson::Null; }, _ => (),
        }
        config::atomic_json(&path, value).await.unwrap();
        compare_config_oracle(IvyJson::object([("kind", "health".into()), ("config", config.json().clone()), ("now", IvyJson::Number(now as f64)), ("verifyRegistration", false.into())]),
            check_health_with_clock(&config, None, false, &FixedHealthClock(now)).await);
    }
}

// Two real requests (Rust and original Node) share one isolated response fixture.
fn health_rpc_fixture(result: IvyJson, expected_method: &'static str) -> (String, std::thread::JoinHandle<()>) {
    use std::{io::{Read, Write}, time::{Duration, Instant}};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let endpoint = format!("http://{}/base", listener.local_addr().unwrap());
    let thread = std::thread::spawn(move || {
        for _ in 0..2 {
            let deadline = Instant::now() + Duration::from_secs(15);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < deadline => std::thread::sleep(Duration::from_millis(5)),
                    Err(error) => panic!("health fixture accept: {error}"),
                }
            };
            stream.set_nonblocking(false).unwrap();
            stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            let mut bytes = Vec::new(); let header_end;
            loop { let mut b = [0]; stream.read_exact(&mut b).unwrap(); bytes.push(b[0]); if bytes.ends_with(b"\r\n\r\n") { header_end = bytes.len(); break; } assert!(bytes.len() < 16384); }
            let headers = String::from_utf8(bytes.clone()).unwrap().to_ascii_lowercase();
            assert!(headers.starts_with("post /base/api/v1/rpc "));
            assert!(headers.contains("authorization: bearer fixture-credential\r\n"));
            let length: usize = headers.lines().find_map(|l| l.strip_prefix("content-length:")).unwrap().trim().parse().unwrap();
            assert!(length < 16384); bytes.resize(header_end + length, 0); stream.read_exact(&mut bytes[header_end..]).unwrap();
            let request = ivy_contracts::json::parse(std::str::from_utf8(&bytes[header_end..]).unwrap(), 16384).unwrap();
            assert_eq!(request.get("method"), Some(&IvyJson::from(expected_method)));
            if expected_method == "serviceNodes.get" { assert_eq!(request.required("params").unwrap().get("serviceNodeId"), Some(&IvyJson::from("host.service"))); }
            let body = ivy_contracts::json::stringify(&IvyJson::object([("jsonrpc", "2.0".into()), ("id", request.required("id").unwrap().clone()), ("result", result.clone())]), 16384).unwrap();
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}", body.len()).unwrap();
        }
    });
    (endpoint, thread)
}

#[tokio::test]
async fn health_rpc_matches_node_registration_and_hive_identity_checks() {
    use ivy_host_runtime::health::check_health_with_clock;
    let fixture = ServiceFixture::new(); let now = 1_767_225_600_000_i64;
    for hive in [false, true] {
        for failure in ["none", "ready", "build", "connected", "synced"] {
            if hive && ["connected", "synced"].contains(&failure) { continue; }
            let mut source = fixture.value();
            let build = source.required("buildId").unwrap().clone();
            let mut response = if hive {
                IvyJson::object([("callerPrincipalId", "fixture".into()), ("version", "1.0".into()), ("buildId", build.clone()),
                    ("hiveProtocol", IvyJson::Number(1.0)), ("runtimeEpoch", "epoch".into()), ("ready", true.into()),
                    ("serverTime", "2026-01-01T00:00:00.000Z".into()), ("storageFormat", IvyJson::Number(4.0)), ("publicBaseUrl", "https://example.invalid".into())])
            } else {
                let mut v = IvyJson::object([("serviceNodeId", "host.service".into()), ("hostId", "host".into()), ("serviceName", "service".into()),
                    ("instanceMode", "singleton".into()), ("principalId", "fixture".into()), ("version", "1.0".into()), ("buildId", build.clone()), ("hiveProtocol", IvyJson::Number(1.0)),
                    ("connected", true.into()), ("synced", true.into()), ("ready", true.into()), ("stale", false.into()), ("desiredEnabled", true.into())]);
                for key in ["nativeVersion", "lastContactAt", "lastSuccessfulSyncAt", "lastFailedSyncAt", "lastObservationAt", "diagnostic"] { v.insert(key, IvyJson::Null); } v
            };
            if failure == "build" { response.insert("buildId", format!("sha256:{}", "b".repeat(64)).into()); }
            else if failure != "none" { response.insert(failure, false.into()); }
            let (endpoint, server) = health_rpc_fixture(response, if hive { "system.status" } else { "serviceNodes.get" });
            source.insert("publicBaseUrl", endpoint.clone().into());
            if hive { source.insert("componentId", "hive".into()); source.insert("settings", hive_settings("127.0.0.1", 39081)); }
            let config = config::InstanceConfig::from_json(source.clone()).unwrap();
            HealthFile::new(config.clone()).write(true, "".into(), None).await.unwrap();
            let path = config.data_root().join("health.json"); let mut value = config::json_file(&path, 4096).await.unwrap();
            value.insert("observedAt", "2026-01-01T00:00:00.000Z".into()); config::atomic_json(&path, value).await.unwrap();
            let result = check_health_with_clock(&config, Some(&endpoint), true, &FixedHealthClock(now)).await;
            compare_config_oracle(IvyJson::object([("kind", "health".into()), ("config", source), ("now", IvyJson::Number(now as f64)), ("verifyRegistration", true.into()), ("endpoint", endpoint.into())]), result);
            server.join().unwrap();
        }
    }
}

#[tokio::test]
async fn native_health_cli_preserves_config_precedence_and_json_exit_contract() {
    let fixture = ServiceFixture::new(); let mut source = fixture.value(); source.remove("credential");
    fixture.save(source.clone()).await; fixture.build(&source).await;
    let config = config::InstanceConfig::from_json(source.clone()).unwrap();
    HealthFile::new(config).write(true, "ready".into(), Some(7)).await.unwrap();
    let node = std::process::Command::new("node").args(["-p", "process.execPath"]).output().unwrap();
    let node = String::from_utf8(node.stdout).unwrap().trim().to_string();
    for mode in ["environment", "argument", "invalid", "build"] {
        if mode == "build" { let mut value = source.clone(); value.insert("buildId", format!("sha256:{}", "b".repeat(64)).into()); fixture.build(&value).await; }
        let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_ivy-health"));
        command.env("IVY_NODE_EXECUTABLE", &node).env_remove("IVY_HIVE_HEALTH_URL").env("IVY_INSTANCE_CONFIG", fixture.0.join("config.json"));
        if mode == "argument" { command.env("IVY_INSTANCE_CONFIG", "relative").arg("--config").arg(fixture.0.join("config.json")); }
        if mode == "invalid" { command.arg("--config").arg("relative"); }
        let result = command.output().unwrap();
        assert!(result.stderr.is_empty());
        let output = ivy_contracts::json::parse(std::str::from_utf8(&result.stdout).unwrap(), 1024).unwrap();
        if ["environment", "argument"].contains(&mode) {
            assert!(result.status.success(), "{output:?}");
            assert_eq!(output.get("buildId"), source.get("buildId")); assert_eq!(output.get("generation"), Some(&IvyJson::Number(7.0)));
        } else {
            assert_eq!(result.status.code(), Some(1));
            assert_eq!(output.get("ok"), Some(&IvyJson::Bool(false)));
            assert_eq!(output.get("code"), Some(&IvyJson::from(if mode == "build" { "build_mismatch" } else { "invalid_arguments" })));
        }
    }
}

#[tokio::test]
async fn stop_fence_matches_node_observation_creation_and_exact_clear() {
    use ivy_host_runtime::process_fence::*;
    let fixture = ServiceFixture::new();
    let canonical = ivy_contracts::json::canonical(&stop_fence(), usize::MAX).unwrap();
    let states = [None, Some(canonical.clone()), Some(format!("\u{feff}\t{canonical}\r\n")),
        Some(format!("\u{85}{canonical}")), Some("null".into()), Some("{".into()),
        Some(" ".repeat(4097)), Some(canonical.replace("native-owner", "different-owner"))];
    for state in states {
        for operation in ["processStopFence", "requireClearProcessStopFence", "recordProcessStopFence", "clearProcessStopFence"] {
            let rust = fixture.0.join("rust"); let node = fixture.0.join("node");
            for root in [&rust, &node] {
                fs::create_dir_all(root).unwrap();
                let path = root.join("process-stop-fence.json");
                if path.exists() { fs::remove_file(&path).unwrap(); }
                if let Some(content) = &state { fs::write(path, content).unwrap(); }
            }
            let result = match operation {
                "processStopFence" => process_stop_fence(&rust).await.map(|v| v.unwrap_or(IvyJson::Null)),
                "requireClearProcessStopFence" => require_clear_process_stop_fence(&rust).await.map(|_| IvyJson::Null),
                "recordProcessStopFence" => record_process_stop_fence(&rust, stop_fence()).await.map(Into::into),
                _ => clear_process_stop_fence(&rust, stop_fence()).await.map(Into::into),
            };
            compare_config_oracle(IvyJson::object([("kind", "fence".into()), ("operation", operation.into()),
                ("path", node.to_string_lossy().to_string().into()), ("fence", stop_fence())]), result);
            assert_eq!(fs::read(rust.join("process-stop-fence.json")).ok(), fs::read(node.join("process-stop-fence.json")).ok());
        }
    }
}

#[tokio::test]
async fn stop_fence_exclusive_writer_and_invalid_marker_fail_closed() {
    use ivy_host_runtime::process_fence::*;
    let fixture = ServiceFixture::new(); let root = fixture.0.join("fenced");
    let mut writers = Vec::new();
    for _ in 0..16 { let root = root.clone(); writers.push(tokio::spawn(async move { record_process_stop_fence(root, stop_fence()).await.unwrap() })); }
    let mut winners = 0; for writer in writers { if writer.await.unwrap() { winners += 1; } }
    assert_eq!(winners, 1);
    let error = require_clear_process_stop_fence(&root).await.unwrap_err();
    assert_eq!(error.code, "outcome_unknown"); assert_eq!(error.outcome, ivy_contracts::Outcome::Unknown);
    assert!(clear_process_stop_fence(&root, stop_fence()).await.unwrap());
    let path = root.join("process-stop-fence.json"); fs::create_dir(&path).unwrap();
    assert_eq!(process_stop_fence(&root).await.unwrap_err().code, "outcome_unknown");
    compare_config_oracle(IvyJson::object([("kind", "fence".into()), ("operation", "recordProcessStopFence".into()),
        ("path", root.to_string_lossy().to_string().into()), ("fence", stop_fence())]),
        record_process_stop_fence(&root, stop_fence()).await.map(Into::into));
    assert!(clear_process_stop_fence(&root, stop_fence()).await.is_err());
    fs::remove_dir(&path).unwrap();
    assert!(record_process_stop_fence(&root, IvyJson::Null).await.is_err());
    assert!(!path.exists());
}

#[tokio::test]
async fn service_configuration_matches_original_node_file_and_identity_boundaries() {
    let fixture = ServiceFixture::new();
    let path = fixture.0.join("config.json");
    let path_json = IvyJson::from(path.to_string_lossy().to_string());
    for bytes in [b"null".as_slice(), b"[1,true]", b"{bad", &[b'"', 0xff, b'"'], b"\xef\xbb\xbf{}", b"{\"__proto__\":{\"x\":1},\"text\":\"\\ud800\"}"] {
        fs::write(&path, bytes).unwrap();
        for limit in [2, 1024] {
            compare_config_oracle(IvyJson::object([("kind", "file".into()), ("path", path_json.clone()), ("limit", IvyJson::Number(limit as f64))]), config::json_file(&path, limit).await);
        }
    }
    let valid = fixture.value(); fixture.build(&valid).await;
    for mode in ["valid", "unknown-property", "root-overlap", "build-mismatch", "null-build"] {
        let mut value = valid.clone();
        match mode {
            "unknown-property" => value.insert("extra", true.into()),
            "root-overlap" => value.insert("dataRoot", value.required("artifactRoot").unwrap().clone()),
            "build-mismatch" => value.insert("version", "wrong".into()),
            "null-build" => config::atomic_json(fixture.0.join("artifact/dist/build-info.json"), IvyJson::Null).await.unwrap(),
            _ => (),
        }
        fixture.save(value).await;
        compare_config_oracle(IvyJson::object([("kind", "instance".into()), ("path", path_json.clone())]), config::instance_config(&path, &fixture_node("24.18.0")).await.map(|value| value.json().clone()));
    }
    for args in [vec![], vec!["--config".to_string()], vec!["--config".into(), "relative".into()], vec!["--config".into(), path.to_string_lossy().to_string(), "--config".into(), "relative".into()]] {
        let native = args.iter().map(Into::into).collect::<Vec<_>>();
        compare_config_oracle(IvyJson::object([("kind", "path".into()), ("args", IvyJson::Array(args.iter().cloned().map(Into::into).collect())), ("environment", path_json.clone())]), config::configuration_path(&native, Some(path.clone().into_os_string())).map(|p| p.to_string_lossy().to_string().into()));
    }
    #[cfg(windows)]
    for input in ["\\relative-to-drive\\config.json", "/relative-to-drive/config.json", "C:relative.json", "\\\\server\\share\\config.json"] {
        compare_config_oracle(IvyJson::object([("kind", "path".into()), ("args", IvyJson::Array(vec!["--config".into(), input.into()])), ("environment", IvyJson::Null)]), config::configuration_path(&["--config".into(), input.into()], None).map(|p| p.to_string_lossy().to_string().into()));
    }
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
    assert_eq!(ivy_contracts::json::canonical(actual.json(), usize::MAX).unwrap(), ivy_contracts::json::canonical(&value, usize::MAX).unwrap());
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
    assert_eq!(ivy_contracts::json::canonical(&health.control().await.unwrap().unwrap(), usize::MAX).unwrap(), ivy_contracts::json::canonical(&control, usize::MAX).unwrap());
    let replacement = HealthFile::with_launch_id(config, Some("launch-1".into()));
    assert!(replacement.control().await.unwrap().is_none());
    fs::write(&path, "{}").unwrap();
    assert!(health.control().await.is_err());
}

#[cfg(windows)]
#[tokio::test]
async fn cancelled_health_waiter_cannot_release_the_active_file_writer() {
    use std::{os::windows::fs::OpenOptionsExt, sync::Arc, time::Duration};
    let fixture = ServiceFixture::new();
    let health = Arc::new(HealthFile::with_launch_id(config::InstanceConfig::from_json(fixture.value()).unwrap(), Some("launch".into())));
    health.write(false, "initial".into(), None).await.unwrap();
    let path = fixture.0.join("data/health.json");
    let held = fs::OpenOptions::new().read(true).share_mode(1).open(&path).unwrap();
    let writer = health.clone();
    let first = tokio::spawn(async move { writer.write(true, "cancelled waiter".into(), None).await });
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if fs::read_dir(path.parent().unwrap()).unwrap().any(|entry| entry.unwrap().file_name().to_string_lossy().ends_with(".tmp")) { break; }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }).await.expect("first write entered durable replacement");
    first.abort(); let _ = first.await;
    let writer = health.clone();
    let mut second = tokio::spawn(async move { writer.write(false, "last".into(), None).await });
    assert!(tokio::time::timeout(Duration::from_millis(50), &mut second).await.is_err());
    drop(held);
    second.await.unwrap().unwrap();
    let value = config::json_file(&path, config::CONFIGURATION_BYTES).await.unwrap();
    assert_eq!(value.get("details"), Some(&IvyJson::from("last")));
    assert_eq!(value.get("ready"), Some(&IvyJson::Bool(false)));
    assert!(!fs::read_dir(path.parent().unwrap()).unwrap().any(|entry| entry.unwrap().file_name().to_string_lossy().ends_with(".tmp")));
}
