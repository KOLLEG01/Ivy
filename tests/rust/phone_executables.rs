#![cfg(all(windows, target_pointer_width = "64"))]
//! Real executable/pipe/SDK ownership, using loopback only. No calls, account,
//! provider, audio device, UI automation or USB input is exercised.
use ivy_contracts::{
    IvyJson,
    hash::{digest, tool_definition_hash},
    json::{parse, stringify},
};
use ivy_host_runtime::{
    config::{atomic_json, json_file, native_instance_config},
    node_path,
};
use ivy_phone_bridge::{service_registry::phone_registry, service_runtime::PhoneService};
use ivy_sdk::rpc::RpcClient;
use std::{
    future::Future,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, Lines},
    process::{Child, ChildStdin, ChildStdout, Command},
};

fn object<const N: usize>(value: [(&str, IvyJson); N]) -> IvyJson {
    IvyJson::object(value)
}
fn id() -> String {
    uuid::Uuid::new_v4().to_string()
}
async fn bounded<T>(work: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(120), work)
        .await
        .expect("Phone executable did not settle")
}
struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("ivy-phone-executable-{}", id()));
        std::fs::create_dir(&root).unwrap();
        Self(root)
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        assert!(
            self.0.starts_with(std::env::temp_dir())
                && self
                    .0
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("ivy-phone-executable-")
        );
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
struct Native {
    child: Child,
    input: Option<ChildStdin>,
    output: Lines<BufReader<ChildStdout>>,
    epoch: String,
    sequence: u64,
}
impl Native {
    fn start(root: &Path, epoch: String) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_ivy-phone-native"))
            .creation_flags(0x0800_0000)
            .args(["--epoch", &epoch, "--owner-root"])
            .arg(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        Self {
            input: child.stdin.take(),
            output: BufReader::new(child.stdout.take().unwrap()).lines(),
            child,
            epoch,
            sequence: 0,
        }
    }
    async fn send(&mut self, method: &str, params: IvyJson, effect: bool) {
        self.sequence += 1;
        let value = object([
            ("version", 1.0.into()),
            ("epoch", self.epoch.clone().into()),
            ("requestId", (self.sequence as f64).into()),
            (
                "operationId",
                if effect { id().into() } else { IvyJson::Null },
            ),
            ("method", method.into()),
            ("params", params),
        ]);
        self.input
            .as_mut()
            .unwrap()
            .write_all((stringify(&value, 1024 * 1024).unwrap() + "\n").as_bytes())
            .await
            .unwrap();
    }
    async fn request(&mut self, method: &str, params: IvyJson, effect: bool) -> IvyJson {
        self.send(method, params, effect).await;
        let line = bounded(self.output.next_line())
            .await
            .unwrap()
            .unwrap_or_else(|| {
                panic!(
                    "Native pipe ended before {method} reply (request {})",
                    self.sequence
                )
            });
        let reply = parse(&line, 1024 * 1024).unwrap();
        assert_eq!(reply.get("requestId"), Some(&(self.sequence as f64).into()));
        assert_eq!(reply.get("ok"), Some(&true.into()), "{line}");
        reply.get("result").unwrap().clone()
    }
}
fn binding() -> IvyJson {
    object([
        ("address", "127.0.0.1".into()),
        ("port", 0.0.into()),
        ("transport", "udp".into()),
    ])
}
fn codecs() -> IvyJson {
    object([
        ("preferences", IvyJson::Array(vec!["PCMA".into()])),
        ("packetMs", 20.0.into()),
    ])
}

#[tokio::test]
async fn native_stdio_owns_its_lease_and_configures_without_devices() {
    let root = Root::new();
    let epoch = id();
    let mut native = Native::start(&root.0, epoch.clone());
    assert_eq!(
        native
            .request("status", object([]), false)
            .await
            .get("configured"),
        Some(&false.into())
    );
    let mut duplicate = Native::start(&root.0, epoch.clone());
    assert_eq!(
        bounded(duplicate.child.wait()).await.unwrap().code(),
        Some(1)
    );
    let configured = native
        .request(
            "configure",
            object([
                ("binding", binding()),
                ("codecs", codecs()),
                ("incomingPeers", IvyJson::Array(vec![])),
                ("registration", IvyJson::Null),
                ("password", IvyJson::Null),
            ]),
            true,
        )
        .await;
    assert_eq!(configured.get("configured"), Some(&true.into()));
    let status = native.request("status", object([]), false).await;
    assert_eq!(status.get("configured"), Some(&true.into()));
    assert!(status.get("call").unwrap().is_null());
    native.input.take();
    assert!(bounded(native.child.wait()).await.unwrap().success());
    let mut replacement = Native::start(&root.0, epoch);
    assert_eq!(
        replacement
            .request("status", object([]), false)
            .await
            .get("configured"),
        Some(&false.into())
    );
    // Shutdown cancels pipe writers as well as admission. Its reply may be lost;
    // only positive process exit and a released lease prove completion.
    replacement.send("shutdown", object([]), true).await;
    assert!(bounded(replacement.child.wait()).await.unwrap().success());
}

struct Fixture {
    root: Root,
    config: PathBuf,
}
impl Fixture {
    async fn new(url: &str) -> Self {
        let root = Root::new();
        let artifact = root.0.join("artifact");
        let native = artifact.join("dist/native/phone/Ivy.PhoneRuntime.exe");
        std::fs::create_dir_all(native.parent().unwrap()).unwrap();
        std::fs::copy(env!("CARGO_BIN_EXE_ivy-phone-native"), &native).unwrap();
        let executable = std::env::current_exe().unwrap();
        let job = executable
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("ivy-job.exe");
        assert!(
            job.is_file(),
            "Build the registered ivy-job prerequisite before this integration target."
        );
        std::fs::copy(job, artifact.join("dist/native/ivy-job.exe")).unwrap();
        let hash = digest(&std::fs::read(&native).unwrap());
        let settings = object([
            (
                "native",
                object([
                    (
                        "executable",
                        "dist/native/phone/Ivy.PhoneRuntime.exe".into(),
                    ),
                    ("executableHash", hash.into()),
                ]),
            ),
            ("binding", binding()),
            ("codecs", codecs()),
            ("registration", IvyJson::Null),
            ("credentialsPath", IvyJson::Null),
            (
                "policy",
                object([
                    ("recipients", IvyJson::Array(vec![])),
                    ("incoming", IvyJson::Array(vec![])),
                ]),
            ),
            ("ringSeconds", 30.0.into()),
            ("pollMs", 250.0.into()),
            ("incomingPrincipalId", IvyJson::Null),
            ("codexVoice", IvyJson::Null),
            ("incomingRoute", "windows".into()),
            ("outgoingRoute", "windows".into()),
            (
                "audio",
                object([
                    ("captureEndpointId", "fixture-capture".into()),
                    ("renderEndpointId", "fixture-render".into()),
                    ("sourceMode", "system_excluding_runtime".into()),
                    ("captureBufferMs", 20.0.into()),
                    ("renderLatencyMs", 20.0.into()),
                    ("queueMs", 40.0.into()),
                ]),
            ),
        ]);
        let build = format!("sha256:{}", "a".repeat(64));
        let value = object([
            ("schemaVersion", 1.0.into()),
            ("hostId", "fixture".into()),
            ("instanceId", "phone-fixture".into()),
            ("serviceNodeId", "fixture.phone".into()),
            ("componentId", "phone-bridge".into()),
            ("publicBaseUrl", url.into()),
            (
                "dataRoot",
                node_path::from_native(&root.0.join("data")).into(),
            ),
            ("artifactRoot", node_path::from_native(&artifact).into()),
            (
                "workRoot",
                node_path::from_native(&root.0.join("work")).into(),
            ),
            (
                "logsRoot",
                node_path::from_native(&root.0.join("logs")).into(),
            ),
            ("buildId", build.clone().into()),
            ("version", "1.0.0".into()),
            ("credential", "isolated-fixture".into()),
            ("settings", settings),
        ]);
        atomic_json(
            artifact.join("dist/build-info.json"),
            object([("buildId", build.into()), ("version", "1.0.0".into())]),
        )
        .await
        .unwrap();
        let config = root.0.join("config.json");
        atomic_json(&config, value).await.unwrap();
        Self { root, config }
    }
    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_ivy-phone-bridge"));
        command
            .creation_flags(0x0800_0000)
            .arg("--config")
            .arg(&self.config)
            .env("IVY_LAUNCH_ID", id())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        command
    }
    async fn health(&self) -> IvyJson {
        json_file(self.root.0.join("data/health.json"), 65536)
            .await
            .unwrap()
    }
    async fn ready(&self, child: &mut Child, previous: Option<&IvyJson>) -> IvyJson {
        bounded(async {
            loop {
                if let Some(status) = child.try_wait().unwrap() {
                    let mut stderr = String::new();
                    child
                        .stderr
                        .take()
                        .unwrap()
                        .take(4096)
                        .read_to_string(&mut stderr)
                        .await
                        .unwrap();
                    panic!("Service exited during startup ({status}): {stderr}");
                }
                if let Ok(value) = json_file(self.root.0.join("data/health.json"), 65536).await {
                    if value.get("ready") == Some(&true.into())
                        && previous.is_none_or(|old| old.get("bootId") != value.get("bootId"))
                    {
                        return value;
                    }
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
    }
    async fn until_health(&self, ready: bool, details: Option<&str>) -> IvyJson {
        bounded(async {
            loop {
                if let Ok(value) = json_file(self.root.0.join("data/health.json"), 65536).await {
                    if value.get("ready") == Some(&ready.into())
                        && details.is_none_or(|text| value.get("details") == Some(&text.into()))
                    {
                        return value;
                    }
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
    }
    async fn control(&self, health: &IvyJson, boot: IvyJson) {
        let value = object([
            ("schemaVersion", 1.0.into()),
            ("instanceId", health.get("instanceId").unwrap().clone()),
            ("launchId", health.get("launchId").unwrap().clone()),
            ("bootId", boot),
            ("action", "shutdown".into()),
            ("requestedAt", "2026-10-23T00:00:00.000Z".into()),
        ]);
        ivy_contracts::core_schema::validate_host("RuntimeControl", &value).unwrap();
        atomic_json(self.root.0.join("data/control.json"), value)
            .await
            .unwrap();
    }
}
fn offline_url() -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    format!("http://{}/ivy/", listener.local_addr().unwrap())
}
#[tokio::test]
async fn service_entry_ignores_stale_control_and_restarts_after_confirmed_stop() {
    let fixture = Fixture::new(&offline_url()).await;
    let mut service = fixture.command().spawn().unwrap();
    let health = fixture.ready(&mut service, None).await;
    fixture.control(&health, id().into()).await;
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert!(service.try_wait().unwrap().is_none());
    fixture
        .control(&health, health.get("bootId").unwrap().clone())
        .await;
    assert!(bounded(service.wait()).await.unwrap().success());
    assert_eq!(
        fixture.health().await.get("details"),
        Some(&"PhoneBridge stopped.".into())
    );
    let mut replacement = fixture.command().spawn().unwrap();
    let next = fixture.ready(&mut replacement, Some(&health)).await;
    fixture
        .control(&next, next.get("bootId").unwrap().clone())
        .await;
    assert!(bounded(replacement.wait()).await.unwrap().success());
    assert_eq!(fixture.health().await.get("ready"), Some(&false.into()));
}

#[tokio::test]
async fn service_observer_loss_retains_stop_and_releases_its_journal() {
    let fixture = Fixture::new(&offline_url()).await;
    let config = native_instance_config(&fixture.config).await.unwrap();
    let first = bounded(PhoneService::start(config.clone())).await.unwrap();
    fixture.until_health(true, None).await;
    let journal = first.journal().clone();
    let closing = first.close();
    drop(closing); // Observation does not own accepted cleanup.
    bounded(first.wait_closed()).await.unwrap();
    assert!(journal.status().await.is_err());
    assert_eq!(fixture.health().await.get("ready"), Some(&false.into()));
    drop(first);
    let replacement = bounded(PhoneService::start(config)).await.unwrap();
    drop(replacement); // The final handle revokes admission and retains cleanup.
    fixture
        .until_health(false, Some("PhoneBridge stopped."))
        .await;
}

#[tokio::test]
async fn service_registers_all_tools_and_preserves_native_owner_across_hive_reconnect() {
    let project = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let mut peer = Command::new("node")
        .creation_flags(0x0800_0000)
        .arg(project.join("tests/fixtures/phone-service-peer.mjs"))
        .current_dir(&project)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let url = bounded(
        BufReader::new(peer.stdout.take().unwrap())
            .lines()
            .next_line(),
    )
    .await
    .unwrap()
    .unwrap();
    let fixture = Fixture::new(&url).await;
    let service = bounded(PhoneService::start(
        native_instance_config(&fixture.config).await.unwrap(),
    ))
    .await
    .unwrap();
    let sdk = service.service().unwrap();
    let connection = bounded(sdk.wait_ready(None)).await.unwrap();
    let epoch = service.journal().epoch().await.unwrap();
    let info = connection
        .request("fixture.info", object([]), Default::default())
        .await
        .unwrap();
    assert_eq!(
        info.get("identity").unwrap().get("serviceName"),
        Some(&"phone-bridge".into())
    );
    assert_eq!(info.get("tools").unwrap().as_array().unwrap().len(), 23);
    assert_eq!(
        info.get("heartbeat").unwrap().get("ready"),
        Some(&true.into())
    );
    let registry = phone_registry().unwrap();
    let definition = registry.get("namespaces").unwrap().as_array().unwrap()[0]
        .get("tools")
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .find(|value| value.get("name") == Some(&"status".into()))
        .unwrap();
    let mut invocation = object([
        ("qualifiedName", "phone.status".into()),
        ("arguments", object([])),
        (
            "definitionHash",
            tool_definition_hash(definition).unwrap().into(),
        ),
    ]);
    let result = connection
        .request("fixture.invoke", invocation.clone(), Default::default())
        .await
        .unwrap();
    assert!(result.get("result").is_some(), "{result:?}");
    invocation.insert("generation", ((connection.generation() + 1) as f64).into());
    let refused = connection
        .request("fixture.invoke", invocation, Default::default())
        .await
        .unwrap();
    assert!(refused.get("error").is_some());
    let codecs = registry.get("namespaces").unwrap().as_array().unwrap()[0]
        .get("tools")
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .find(|value| value.get("name") == Some(&"codecTest".into()))
        .unwrap();
    let diagnostic = connection
        .request(
            "fixture.invoke",
            object([
                ("qualifiedName", "phone.codecTest".into()),
                ("arguments", object([])),
                (
                    "definitionHash",
                    tool_definition_hash(codecs).unwrap().into(),
                ),
            ]),
            Default::default(),
        )
        .await
        .unwrap();
    let rows = diagnostic
        .get("result")
        .unwrap()
        .get("codecs")
        .unwrap()
        .as_array()
        .unwrap();
    assert_eq!(rows.len(), 5);
    for row in rows {
        if row.get("name") == Some(&"EVS".into()) {
            assert_eq!(row.get("error"), Some(&"codec_library_missing".into()));
        } else {
            assert_eq!(row.get("passed"), Some(&true.into()), "{row:?}");
        }
    }
    let old = connection.generation();
    connection
        .request("fixture.disconnect", object([]), Default::default())
        .await
        .unwrap();
    bounded(async {
        loop {
            if sdk.connection().is_ok_and(|c| c.generation() > old) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await;
    assert_eq!(service.journal().epoch().await.unwrap(), epoch);
    bounded(service.close().wait()).await.unwrap();
    assert_eq!(fixture.health().await.get("ready"), Some(&false.into()));
    peer.stdin.take();
    assert!(bounded(peer.wait()).await.unwrap().success());
}

#[tokio::test]
async fn failed_start_releases_ownership_before_a_corrected_start() {
    let fixture = Fixture::new(&offline_url()).await;
    let valid = json_file(&fixture.config, 65536).await.unwrap();
    // Offline configuration tools must not start the service or acquire its lifetime.
    for allowed in [true, false] {
        let mut settings = valid.get("settings").unwrap().clone();
        if !allowed {
            settings.remove("audio");
        }
        let mut command = Command::new(env!("CARGO_BIN_EXE_ivy-phone-bridge"));
        let mut child = command
            .arg("--validate-settings")
            .current_dir(&fixture.root.0)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .creation_flags(0x0800_0000)
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let mut input = child.stdin.take().unwrap();
        input
            .write_all(stringify(&settings, 65536).unwrap().as_bytes())
            .await
            .unwrap();
        drop(input);
        let output = bounded(child.wait_with_output()).await.unwrap();
        assert_eq!(
            output.status.success(),
            allowed,
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stdout.is_empty());
        assert!(!fixture.root.0.join("data/phone-native.lock").exists());
    }
    let mut bad = valid.clone();
    let mut settings = bad.get("settings").unwrap().clone();
    let mut native = settings.get("native").unwrap().clone();
    native.insert(
        "executableHash",
        format!("sha256:{}", "0".repeat(64)).into(),
    );
    settings.insert("native", native);
    bad.insert("settings", settings);
    atomic_json(&fixture.config, bad).await.unwrap();
    let error = bounded(PhoneService::start(
        native_instance_config(&fixture.config).await.unwrap(),
    ))
    .await
    .err()
    .unwrap();
    assert_eq!(error.code, "phone_build_mismatch");
    assert_eq!(fixture.health().await.get("ready"), Some(&false.into()));
    atomic_json(&fixture.config, valid).await.unwrap();
    let replacement = bounded(PhoneService::start(
        native_instance_config(&fixture.config).await.unwrap(),
    ))
    .await
    .unwrap();
    bounded(replacement.close().wait()).await.unwrap();
}

#[tokio::test]
async fn cancelled_start_observer_retires_the_late_native_owner() {
    let fixture = Fixture::new(&offline_url()).await;
    let config = native_instance_config(&fixture.config).await.unwrap();
    let mut start = Box::pin(PhoneService::start(config.clone()));
    std::future::poll_fn(|context| {
        assert!(start.as_mut().poll(context).is_pending());
        std::task::Poll::Ready(())
    })
    .await;
    drop(start);
    fixture
        .until_health(false, Some("PhoneBridge stopped."))
        .await;
    let replacement = bounded(PhoneService::start(config)).await.unwrap();
    bounded(replacement.close().wait()).await.unwrap();
}
