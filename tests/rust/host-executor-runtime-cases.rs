use super::{ServiceFixture, assert_json_eq, host_configuration};
use futures_util::future::BoxFuture;
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString, core_schema::validate_host, json};
use ivy_host_runtime::{
    config,
    docker::{DockerAdapter, DockerCommand, NativeDockerHost},
    executor::{
        ActivationRuntime, ExecutorFuture, ExecutorOptions, NativeActivationRuntime,
        NativeOfflineHiveInspector, OfflineHiveInspector, OfflineInspection, OperationContext,
    },
    journal::HostJournal,
    node_path,
    node_runtime::NodeRuntime,
    portable_http::PortableHttp,
    prepare::PreparationRuntime,
    process::ProcessResult,
};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use tokio_util::sync::CancellationToken;
fn v(s: &str) -> IvyJson {
    json::parse(s, usize::MAX).unwrap()
}
#[path = "host-executor-container-cases.rs"]
mod container_cases;
#[path = "host-executor-inspection-cases.rs"]
mod inspection_cases;
fn path(p: &Path) -> IvyJson {
    IvyJson::String(node_path::from_native(p))
}
fn native(v: &IvyJson) -> PathBuf {
    node_path::to_native(v.as_js_string().unwrap())
}
fn hash() -> IvyJson {
    format!("sha256:{}", "a".repeat(64)).into()
}
fn nested(v: &mut IvyJson, key: &str, field: &str, next: IvyJson) {
    let mut child = v.required(key).unwrap().clone();
    child.insert(field, next);
    v.insert(key, child);
}
fn wire(result: IvyResult<()>) -> IvyJson {
    match result {
        Ok(()) => v(r#"{"ok":true}"#),
        Err(error) => IvyJson::object([("ok", false.into()), ("error", error.to_wire())]),
    }
}
fn oracle(input: IvyJson) -> IvyJson {
    let mut child = Command::new("node")
        .args([
            "--experimental-transform-types",
            "--disable-warning=ExperimentalWarning",
            "tests/fixtures/host-executor-runtime-oracle.mjs",
        ])
        .current_dir(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.."))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(json::stringify(&input, usize::MAX).unwrap().as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    json::parse(std::str::from_utf8(&out.stdout).unwrap(), usize::MAX).unwrap()
}
static CORPUS: Mutex<()> = Mutex::new(());
fn pair(case: &str, actual: &IvyJson, expected: &IvyJson) {
    if let Some(file) = std::env::var_os("IVY_HOST_EXECUTOR_RUNTIME_CORPUS") {
        let _guard = CORPUS.lock().unwrap();
        writeln!(
            fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(file)
                .unwrap(),
            "{}",
            json::stringify(
                &IvyJson::object([
                    ("case", case.into()),
                    ("actual", actual.clone()),
                    ("expected", expected.clone())
                ]),
                usize::MAX
            )
            .unwrap()
        )
        .unwrap();
    }
    assert_json_eq(actual, expected);
}
#[derive(Default)]
struct Inspector {
    calls: Mutex<Vec<IvyJson>>,
    result: Mutex<Option<IvyResult<IvyJson>>>,
}
impl OfflineHiveInspector for Inspector {
    fn inspect(&self, r: OfflineInspection) -> ExecutorFuture<'_, IvyJson> {
        Box::pin(async move {
            let config =
                config::json_file(r.configuration_path.clone(), config::CONFIGURATION_BYTES)
                    .await?;
            self.calls.lock().unwrap().push(IvyJson::object([
                ("config", config),
                ("contracts", r.contracts),
                ("artifactRoot", path(&r.artifact_root)),
            ]));
            self.result.lock().unwrap().clone().unwrap_or_else(|| {
                Err(IvyError::new(
                    "inspector_unavailable",
                    "No test offline inspector response.",
                ))
            })
        })
    }
}
fn node() -> PathBuf {
    PathBuf::from(
        String::from_utf8(
            Command::new("node")
                .args(["-p", "process.execPath"])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap()
        .trim(),
    )
}
fn preparation() -> PreparationRuntime {
    PreparationRuntime {
        node: Some(NodeRuntime {
            executable: node(),
            version: "24.18.0".into(),
        }),
        compare: Arc::new(|a, b| a.0.cmp(&b.0)),
        imports: Some(Arc::new(|_, _| panic!("compatibility must not parse source"))),
        http: PortableHttp::new(reqwest::Client::builder().no_proxy()).unwrap(),
    }
}
struct Fixture {
    root: ServiceFixture,
    host: IvyJson,
    candidate: IvyJson,
    manifest: IvyJson,
    journal: HostJournal,
    context: OperationContext,
    inspector: Arc<Inspector>,
    runtime: NativeActivationRuntime,
}
impl Fixture {
    async fn new(
        component: &str,
        edit: impl FnOnce(&mut IvyJson, &mut IvyJson, &mut IvyJson),
    ) -> Self {
        let root = ServiceFixture::new();
        let mut host = host_configuration(&root);
        host.insert("executables", IvyJson::object([("node", path(&node()))]));
        let mut instance = host.required("instances").unwrap().as_array().unwrap()[0].clone();
        instance.insert("componentId", component.into());
        instance.insert("credential", "fixture-credential".into());
        host.insert("instances", IvyJson::Array(vec![instance]));
        let artifact = native(host.required("artifactRoot").unwrap()).join("release");
        fs::create_dir_all(artifact.join("dist")).unwrap();
        fs::write(artifact.join("dist/entry.mjs"), "").unwrap();
        fs::write(
            artifact.join("dist/ready.mjs"),
            "process.stdout.write('ready');",
        )
        .unwrap();
        let mut candidate = super::journal_cases::candidate();
        candidate.insert("candidateId", hash());
        candidate.insert("buildId", hash());
        candidate.insert("componentId", component.into());
        candidate.insert("artifactRoot", path(&artifact));
        candidate.insert("manifestPath", path(&artifact.join("manifest.json")));
        let mut manifest = super::journal_cases::manifest();
        manifest.insert("componentId", component.into());
        manifest.insert("kind", "native".into());
        manifest.remove("app");
        manifest.insert("buildId", hash());
        manifest.insert(
            "entrypoint",
            v(r#"{"executable":"node","args":["dist/entry.mjs"],"timeoutMs":5000}"#),
        );
        manifest.insert("readiness",v(r#"{"timeoutMs":1000,"command":{"executable":"node","args":["dist/ready.mjs"],"timeoutMs":5000}}"#));
        manifest.insert("shutdown", v(r#"{"timeoutMs":1000}"#));
        manifest.insert(
            "restart",
            v(r#"{"policy":"never","minimumDelayMs":100,"maximumDelayMs":100}"#),
        );
        manifest.insert(
            "requirements",
            v(r#"{"node":">=24.18.0 <25.0.0","hiveProtocol":null,"contracts":[]}"#),
        );
        edit(&mut host, &mut candidate, &mut manifest);
        validate_host("ReleaseManifest", &manifest).unwrap();
        config::atomic_json(
            artifact.join("dist/build-info.json"),
            IvyJson::object([
                ("buildId", candidate.required("buildId").unwrap().clone()),
                ("version", manifest.required("version").unwrap().clone()),
            ]),
        )
        .await
        .unwrap();
        config::atomic_json(artifact.join("manifest.json"), manifest.clone())
            .await
            .unwrap();
        let journal = HostJournal::open(host.clone()).await.unwrap();
        journal
            .save_candidate(candidate.clone(), manifest.clone())
            .await
            .unwrap();
        let entry = journal
            .accept(
                IvyJson::object([
                    ("action", "deploy".into()),
                    ("instanceId", "fixture".into()),
                    ("candidateId", hash()),
                    ("operationId", "runtime-fixture".into()),
                ]),
                None,
            )
            .await
            .unwrap();
        let context = OperationContext::open(
            &journal,
            &entry,
            ExecutorOptions {
                own_instance_id: Some("independent".into()),
                account_home: node_path::from_native(&root.0),
                build_id: None,
            },
            CancellationToken::new(),
            Arc::new(ivy_sdk::clock::SystemClock),
        )
        .await
        .unwrap();
        let inspector = Arc::new(Inspector::default());
        let runtime = NativeActivationRuntime::new(
            root.0.clone(),
            preparation(),
            PathBuf::from(env!("CARGO_BIN_EXE_ivy-job")),
            inspector.clone(),
        )
        .unwrap();
        Self {
            root,
            host,
            candidate,
            manifest,
            journal,
            context,
            inspector,
            runtime,
        }
    }
    fn input(&self) -> IvyJson {
        IvyJson::object([
            ("config", self.host.clone()),
            ("candidateId", hash()),
            ("bootstrapRoot", path(&self.root.0)),
        ])
    }
    async fn close(self) {
        self.runtime.dispose().await.unwrap();
        self.context.journal.close().await.unwrap();
        self.journal.close().await.unwrap();
    }
}

#[tokio::test]
async fn native_admission_matches_original_runtime_artifact_command_and_journal_guards() {
    for case in [
        "ready",
        "unsupported-os",
        "unsupported-arch",
        "unsupported-node",
        "stamp-mismatch",
        "missing-entry",
        "bad-cwd",
        "script-command",
        "executor-format",
    ] {
        let fixture = Fixture::new(
            if case == "executor-format" {
                "host-executor"
            } else {
                "fixture"
            },
            |_, _, manifest| match case {
                "unsupported-os" => nested(
                    manifest,
                    "requirements",
                    "os",
                    IvyJson::Array(vec![if cfg!(windows) {
                        "linux".into()
                    } else {
                        "win32".into()
                    }]),
                ),
                "unsupported-arch" => nested(
                    manifest,
                    "requirements",
                    "arch",
                    IvyJson::Array(vec![if cfg!(target_arch = "x86_64") {
                        "arm64".into()
                    } else {
                        "x64".into()
                    }]),
                ),
                "unsupported-node" => {
                    nested(manifest, "requirements", "node", ">=25.0.0 <26.0.0".into())
                }
                "bad-cwd" => nested(manifest, "entrypoint", "cwd", "../../outside".into()),
                "script-command" => nested(manifest, "entrypoint", "executable", ".CMD".into()),
                "executor-format" => {
                    manifest.insert(
                        "storage",
                        v(r#"{"minReadableFormat":2,"maxReadableFormat":2,"writeFormat":2}"#),
                    );
                }
                _ => {}
            },
        )
        .await;
        let artifact = native(fixture.candidate.required("artifactRoot").unwrap());
        if case == "stamp-mismatch" {
            config::atomic_json(
                artifact.join("dist/build-info.json"),
                v(r#"{"buildId":"different","version":"1.0.0"}"#),
            )
            .await
            .unwrap();
        }
        if case == "missing-entry" {
            fs::remove_file(artifact.join("dist/entry.mjs")).unwrap();
        }
        if case == "script-command" {
            fs::write(artifact.join(".CMD"), "").unwrap();
        }
        let result = wire(
            fixture
                .runtime
                .compatibility(&fixture.context, &hash().as_str().unwrap(), None)
                .await,
        );
        let expected = oracle(fixture.input());
        pair(case, &result, &expected);
        assert!(fixture.journal.target("fixture").await.unwrap().is_none());
        fixture.close().await;
    }
}

struct Rpc {
    endpoint: String,
    requests: Arc<Mutex<Vec<IvyJson>>>,
    stopped: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Rpc {
    fn new(status: IvyJson, inspection: IvyJson) -> Self {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = format!("http://{}/base", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(vec![]));
        let stopped = Arc::new(AtomicBool::new(false));
        let trace = requests.clone();
        let stop = stopped.clone();
        let thread = std::thread::spawn(move || {
            let mut connections = vec![];
            while !stop.load(Ordering::SeqCst) {
                let mut stream = match listener.accept() {
                    Ok((s, _)) => s,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(2));
                        continue;
                    }
                    Err(e) => panic!("{e}"),
                };
                let trace = trace.clone();
                let status = status.clone();
                let inspection = inspection.clone();
                connections.push(std::thread::spawn(move || {
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut headers = vec![];
                while !headers.ends_with(b"\r\n\r\n") {
                    let mut b = [0];
                    match stream.read_exact(&mut b) {
                        Ok(_) => (),
                        Err(e) if headers.is_empty() && matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut | std::io::ErrorKind::UnexpectedEof) => return,
                        Err(e) => panic!("Incomplete RPC request: {e}"),
                    }
                    headers.push(b[0]);
                    assert!(headers.len() < 16384);
                }
                let head = String::from_utf8(headers).unwrap().to_lowercase();
                assert!(head.starts_with("post /base/api/v1/rpc "));
                assert!(head.contains("authorization: bearer fixture-credential\r\n"));
                let count = head
                    .lines()
                    .find_map(|s| s.strip_prefix("content-length:"))
                    .unwrap()
                    .trim()
                    .parse::<usize>()
                    .unwrap();
                let mut body = vec![0; count];
                stream.read_exact(&mut body).unwrap();
                let request = json::parse(std::str::from_utf8(&body).unwrap(), 65536).unwrap();
                let frames = request
                    .as_array()
                    .unwrap_or(std::slice::from_ref(&request));
                let responses = frames
                    .iter()
                    .map(|frame| {
                        let method = frame.required("method").unwrap().as_str().unwrap();
                        let result = match method.as_str() {
                            "system.status" => status.clone(),
                            "system.inspectStorage" => inspection.clone(),
                            _ => panic!("Unexpected RPC {method}"),
                        };
                        trace.lock().unwrap().push(IvyJson::object([
                            ("method", method.into()),
                            ("params", frame.required("params").unwrap().clone()),
                        ]));
                        IvyJson::object([
                            ("jsonrpc", "2.0".into()),
                            ("id", frame.required("id").unwrap().clone()),
                            ("result", result),
                        ])
                    })
                    .collect::<Vec<_>>();
                let body = json::stringify(
                    &if request.as_array().is_some() {
                        IvyJson::Array(responses)
                    } else {
                        responses.into_iter().next().unwrap()
                    },
                    65536,
                )
                .unwrap();
                write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",body.len()).unwrap();
                }));
            }
            for connection in connections {
                connection.join().unwrap();
            }
        });
        Self {
            endpoint,
            requests,
            stopped,
            thread: Some(thread),
        }
    }
}
impl Drop for Rpc {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::SeqCst);
        let result = self.thread.take().unwrap().join();
        if !std::thread::panicking() {
            result.unwrap();
        }
    }
}

fn hive(host: &mut IvyJson, manifest: &mut IvyJson, endpoint: Option<&str>) {
    let url = url::Url::parse(endpoint.unwrap_or("http://127.0.0.1:39081/base")).unwrap();
    let mut instance = host.required("instances").unwrap().as_array().unwrap()[0].clone();
    instance.insert(
        "settings",
        super::hive_settings("127.0.0.1", url.port().unwrap()),
    );
    host.insert("instances", IvyJson::Array(vec![instance]));
    if let Some(endpoint) = endpoint {
        host.insert("publicBaseUrl", endpoint.into());
    }
    manifest.insert("kind", "hive".into());
    manifest.insert(
        "storage",
        v(r#"{"minReadableFormat":1,"maxReadableFormat":1,"writeFormat":1}"#),
    );
}
async fn materialize(f: &Fixture, enabled: bool) -> IvyJson {
    ivy_host_runtime::target::materialize_target(
        f.context.journal.clone(),
        "fixture".into(),
        hash().as_str().unwrap(),
        enabled,
        None,
        node_path::from_native(&f.root.0),
    )
    .await
    .unwrap()
    .target
}
async fn observe(f: &Fixture, target: Option<&IvyJson>, state: &str, old: bool) {
    let date = if old {
        "2026-01-01T00:00:00.000Z".into()
    } else {
        chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
    };
    f.journal
        .record_observation(
            "fixture",
            IvyJson::object([
                ("schemaVersion", IvyJson::Number(1.0)),
                ("instanceId", "fixture".into()),
                ("ownerPid", IvyJson::Number(1.0)),
                ("ownerBootId", "fixture-owner".into()),
                ("observedAt", date.into()),
                (
                    "targetRevision",
                    target
                        .map(|t| t.required("revision").unwrap().clone())
                        .unwrap_or(IvyJson::Null),
                ),
                (
                    "candidateId",
                    target.map(|_| hash()).unwrap_or(IvyJson::Null),
                ),
                ("buildId", target.map(|_| hash()).unwrap_or(IvyJson::Null)),
                ("state", state.into()),
                ("health", IvyJson::Null),
                ("restartCount", IvyJson::Number(0.0)),
                ("nextRestartAt", IvyJson::Null),
                ("code", IvyJson::Null),
                ("message", "".into()),
            ]),
        )
        .await
        .unwrap();
}
#[tokio::test]
async fn native_live_hive_inspection_uses_bound_identity_even_after_observation_ages() {
    for case in ["ready-stale", "identity-changed", "storage-incompatible"] {
        let rpc = Rpc::new(
            IvyJson::object([
                ("ready", true.into()),
                (
                    "buildId",
                    if case == "identity-changed" {
                        "different".into()
                    } else {
                        hash()
                    },
                ),
            ]),
            IvyJson::object([
                ("schemaVersion", IvyJson::Number(1.0)),
                ("exists", true.into()),
                (
                    "format",
                    if case == "storage-incompatible" {
                        IvyJson::Number(2.0)
                    } else {
                        IvyJson::Number(1.0)
                    },
                ),
                ("contracts", IvyJson::Array(vec![])),
            ]),
        );
        let f = Fixture::new("hive", |host, _, manifest| {
            hive(host, manifest, Some(&rpc.endpoint))
        })
        .await;
        let target = materialize(&f, true).await;
        f.journal.restore_target(target.clone()).await.unwrap();
        observe(&f, Some(&target), "ready", true).await;
        let actual = wire(
            f.runtime
                .compatibility(&f.context, &hash().as_str().unwrap(), None)
                .await,
        );
        let before = rpc.requests.lock().unwrap().clone();
        let expected = oracle(f.input());
        pair(case, &actual, &expected);
        let all = rpc.requests.lock().unwrap().clone();
        assert_eq!(&all[..before.len()], &all[before.len()..]);
        assert!(f.inspector.calls.lock().unwrap().is_empty());
        assert_json_eq(
            &f.journal.target("fixture").await.unwrap().unwrap(),
            &target,
        );
        f.close().await;
    }
}
#[tokio::test]
async fn stopped_hive_requires_conclusive_identity_before_the_mandatory_owner_binding() {
    for case in [
        "no-observation",
        "stale-starting",
        "idle-current",
        "stopped-current",
        "outside-range",
        "invalid-inspection",
    ] {
        let f = Fixture::new("hive", |host, _, manifest| hive(host, manifest, None)).await;
        let selected = if case == "stopped-current" {
            let target = materialize(&f, false).await;
            f.journal.restore_target(target.clone()).await.unwrap();
            Some(target)
        } else {
            None
        };
        if case != "no-observation" {
            observe(
                &f,
                selected.as_ref(),
                if case == "stale-starting" {
                    "starting"
                } else if selected.is_some() {
                    "stopped"
                } else {
                    "idle"
                },
                true,
            )
            .await;
        }
        let marker = f.root.0.join("retained-data");
        fs::write(&marker, b"persistent bytes").unwrap();
        *f.inspector.result.lock().unwrap() = Some(Ok(if case == "invalid-inspection" {
            v(r#"{"exists":true}"#)
        } else {
            IvyJson::object([
                ("schemaVersion", IvyJson::Number(1.0)),
                ("exists", true.into()),
                (
                    "format",
                    if case == "outside-range" {
                        IvyJson::Number(2.0)
                    } else {
                        IvyJson::Number(1.0)
                    },
                ),
                ("contracts", IvyJson::Array(vec![])),
            ])
        }));
        let result = f
            .runtime
            .compatibility(&f.context, &hash().as_str().unwrap(), None)
            .await;
        let expected = match case {
            "no-observation" | "stale-starting" => Some("compatibility_unknown"),
            "outside-range" => Some("incompatible_storage"),
            "invalid-inspection" => Some("invalid_arguments"),
            _ => None,
        };
        assert_eq!(
            result.err().map(|e| e.code),
            expected.map(str::to_owned),
            "{case}"
        );
        let calls = f.inspector.calls.lock().unwrap().clone();
        assert_eq!(
            calls.len(),
            if matches!(case, "no-observation" | "stale-starting") {
                0
            } else {
                1
            },
            "{case}"
        );
        if let Some(call) = calls.first() {
            assert_eq!(
                call.required("artifactRoot").unwrap(),
                f.candidate.required("artifactRoot").unwrap()
            );
            assert_eq!(
                call.required("config")
                    .unwrap()
                    .required("instanceId")
                    .unwrap(),
                &"fixture".into()
            );
            assert_eq!(
                call.required("config")
                    .unwrap()
                    .required("buildId")
                    .unwrap(),
                &hash()
            );
        }
        assert_eq!(fs::read(marker).unwrap(), b"persistent bytes");
        assert_eq!(f.journal.target("fixture").await.unwrap(), selected);
        assert!(f.journal.installed("fixture").await.unwrap().is_none());
        f.close().await;
    }
}
#[tokio::test]
async fn native_windows_process_reconciliation_keeps_the_external_runtime_owner_authority() {
    let f = Fixture::new("fixture", |_, _, _| {}).await;
    let target = materialize(&f, true).await;
    if cfg!(windows) {
        f.runtime
            .reconcile(&f.context, Some(target), true)
            .await
            .unwrap();
        assert!(
            f.journal
                .observations()
                .await
                .unwrap()
                .get("fixture")
                .is_none()
        );
        assert!(f.journal.target("fixture").await.unwrap().is_none());
    }
    f.close().await;
}

#[tokio::test]
async fn native_rpc_admission_matches_original_actual_current_and_historical_contracts() {
    for case in [
        "compatible",
        "wrong-protocol",
        "not-ready",
        "historical-version",
        "unknown-contract",
    ] {
        let status = IvyJson::object([
            ("ready", (case != "not-ready").into()),
            (
                "hiveProtocol",
                IvyJson::Number(if case == "wrong-protocol" { 2.0 } else { 1.0 }),
            ),
        ]);
        let inspection = IvyJson::object([
            ("schemaVersion", IvyJson::Number(1.0)),
            ("exists", true.into()),
            ("format", IvyJson::Number(1.0)),
            (
                "contracts",
                IvyJson::Array(vec![IvyJson::object([
                    (
                        "key",
                        if case == "unknown-contract" {
                            "other.contract".into()
                        } else {
                            "fixture.contract".into()
                        },
                    ),
                    (
                        "versions",
                        IvyJson::Array(vec![if case == "historical-version" {
                            "2.0.0".into()
                        } else {
                            "1.0.0".into()
                        }]),
                    ),
                ])]),
            ),
        ]);
        let rpc = Rpc::new(status, inspection);
        let fixture=Fixture::new("fixture",|host,_,manifest|{host.insert("publicBaseUrl",rpc.endpoint.clone().into());manifest.insert("requirements",v(r#"{"node":">=24.18.0 <25.0.0","hiveProtocol":1,"contracts":[{"key":"fixture.contract","readVersions":["1.0.0"],"writeVersions":[],"readScope":{"roots":null,"history":"all","includeArchived":true,"references":[]}}]}"#));}).await;
        let actual = wire(
            fixture
                .runtime
                .compatibility(&fixture.context, &hash().as_str().unwrap(), None)
                .await,
        );
        let before = rpc.requests.lock().unwrap().clone();
        let expected = oracle(fixture.input());
        pair(case, &actual, &expected);
        let all = rpc.requests.lock().unwrap().clone();
        assert_eq!(&all[..before.len()], &all[before.len()..]);
        assert!(fixture.inspector.calls.lock().unwrap().is_empty());
        fixture.close().await;
    }
}

#[derive(Default)]
struct UnitModel {
    files: std::collections::BTreeMap<JsString, Vec<u8>>,
    active: String,
    trace: Vec<Vec<JsString>>,
    clock: f64,
}
#[derive(Clone, Default)]
struct UnitHost(Arc<Mutex<UnitModel>>);
struct UnitFileModel {
    host: UnitHost,
    path: JsString,
}
impl ivy_host_runtime::systemd::UnitFile for UnitFileModel {
    fn write<'a>(&'a mut self, bytes: Vec<u8>) -> BoxFuture<'a, std::io::Result<()>> {
        Box::pin(async move {
            self.host
                .0
                .lock()
                .unwrap()
                .files
                .get_mut(&self.path)
                .unwrap()
                .extend(bytes);
            Ok(())
        })
    }
    fn sync(&mut self) -> BoxFuture<'_, std::io::Result<()>> {
        Box::pin(async { Ok(()) })
    }
    fn close(self: Box<Self>) -> BoxFuture<'static, std::io::Result<()>> {
        Box::pin(async { Ok(()) })
    }
}
impl ivy_host_runtime::systemd::SystemdHost for UnitHost {
    fn platform(&self) -> &str {
        "linux"
    }
    fn user_id(&self) -> Option<u32> {
        Some(0)
    }
    fn metadata(
        &self,
        path: JsString,
    ) -> BoxFuture<'static, std::io::Result<ivy_host_runtime::linux_host::FileMetadata>> {
        let host = self.clone();
        Box::pin(async move {
            let state = host.0.lock().unwrap();
            let bytes = state
                .files
                .get(&path)
                .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::NotFound))?;
            Ok(ivy_host_runtime::linux_host::FileMetadata {
                is_file: true,
                is_symlink: false,
                uid: Some(0),
                gid: Some(0),
                mode: 0o644,
                size: bytes.len() as u64,
            })
        })
    }
    fn read(&self, path: JsString) -> BoxFuture<'static, std::io::Result<Vec<u8>>> {
        let host = self.clone();
        Box::pin(async move {
            host.0
                .lock()
                .unwrap()
                .files
                .get(&path)
                .cloned()
                .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::NotFound))
        })
    }
    fn open(
        &self,
        path: JsString,
        exclusive: bool,
    ) -> BoxFuture<'static, std::io::Result<Box<dyn ivy_host_runtime::systemd::UnitFile>>> {
        let host = self.clone();
        Box::pin(async move {
            let mut state = host.0.lock().unwrap();
            if exclusive {
                if state.files.contains_key(&path) {
                    return Err(std::io::ErrorKind::AlreadyExists.into());
                }
                state.files.insert(path.clone(), vec![]);
            } else if path != JsString::from("/etc/systemd/system")
                && !state.files.contains_key(&path)
            {
                return Err(std::io::ErrorKind::NotFound.into());
            }
            drop(state);
            Ok(Box::new(UnitFileModel { host, path })
                as Box<dyn ivy_host_runtime::systemd::UnitFile>)
        })
    }
    fn rename(&self, from: JsString, to: JsString) -> BoxFuture<'static, std::io::Result<()>> {
        let host = self.clone();
        Box::pin(async move {
            let mut state = host.0.lock().unwrap();
            let bytes = state
                .files
                .remove(&from)
                .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::NotFound))?;
            state.files.insert(to, bytes);
            Ok(())
        })
    }
    fn unlink(&self, path: JsString) -> BoxFuture<'static, std::io::Result<()>> {
        let host = self.clone();
        Box::pin(async move {
            host.0
                .lock()
                .unwrap()
                .files
                .remove(&path)
                .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::NotFound))?;
            Ok(())
        })
    }
    fn execute(&self, _: JsString, _: Vec<JsString>) -> BoxFuture<'static, IvyResult<JsString>> {
        Box::pin(async { panic!("OS model must not execute a real systemd command") })
    }
    fn uuid(&self) -> String {
        "11111111-2222-4333-8444-555555555555".into()
    }
    fn now_ms(&self) -> f64 {
        self.0.lock().unwrap().clock
    }
    fn sleep(&self, ms: u64) -> BoxFuture<'static, ()> {
        let host = self.clone();
        Box::pin(async move {
            host.0.lock().unwrap().clock += ms as f64;
        })
    }
}
impl UnitHost {
    fn adapter(&self) -> ivy_host_runtime::systemd::SystemdAdapter {
        let host = self.clone();
        let command: Arc<dyn ivy_host_runtime::systemd::SystemdCommand> = Arc::new(
            move |_: IvyJson, args: Vec<JsString>| -> BoxFuture<'static, IvyResult<JsString>> {
                let host = host.clone();
                Box::pin(async move {
                    let mut s = host.0.lock().unwrap();
                    s.trace.push(args.clone());
                    match args.first().and_then(|v| v.to_utf8().ok()).as_deref() {
                        Some("stop") => s.active = "inactive".into(),
                        Some("start") => s.active = "active".into(),
                        Some("show") | Some("daemon-reload") => {}
                        _ => panic!("Unexpected modeled systemd command"),
                    };
                    Ok(format!(
                        "LoadState=loaded\nActiveState={}\nSubState={}\nMainPID=42\n",
                        s.active,
                        if s.active == "active" {
                            "running"
                        } else {
                            "dead"
                        }
                    )
                    .into())
                })
            },
        );
        ivy_host_runtime::systemd::SystemdAdapter::with_command(Arc::new(self.clone()), command)
    }
    fn calls(&self, op: &str) -> usize {
        self.0
            .lock()
            .unwrap()
            .trace
            .iter()
            .filter(|a| a.first() == Some(&JsString::from(op)))
            .count()
    }
}
fn write_health(f: &Fixture, target: &IvyJson) {
    let config = load_json(&native(target.required("configPath").unwrap()));
    let root = native(config.required("dataRoot").unwrap());
    fs::create_dir_all(&root).unwrap();
    let at = chrono::DateTime::from_timestamp_millis(f.context.clock.utc_millis())
        .unwrap()
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let health = IvyJson::object([
        ("schemaVersion", IvyJson::Number(1.0)),
        ("instanceId", "fixture".into()),
        (
            "componentId",
            config.required("componentId").unwrap().clone(),
        ),
        ("buildId", hash()),
        ("pid", IvyJson::Number(1.0)),
        ("bootId", "fixture-health".into()),
        ("startedAt", at.clone().into()),
        ("observedAt", at.into()),
        ("ready", true.into()),
        ("generation", IvyJson::Null),
        ("launchId", target.required("revision").unwrap().clone()),
        ("details", "".into()),
    ]);
    fs::write(
        root.join("health.json"),
        json::stringify(&health, usize::MAX).unwrap(),
    )
    .unwrap();
}
fn load_json(path: &Path) -> IvyJson {
    json::parse(&fs::read_to_string(path).unwrap(), usize::MAX).unwrap()
}
fn selected_observation(value: &IvyJson) -> IvyJson {
    IvyJson::object(
        [
            "targetRevision",
            "candidateId",
            "buildId",
            "state",
            "code",
            "message",
        ]
        .into_iter()
        .map(|k| (k, value.required(k).unwrap().clone())),
    )
}
struct RuntimeClock(std::sync::atomic::AtomicI64);
impl ivy_sdk::clock::Clock for RuntimeClock {
    fn utc_millis(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
    fn monotonic(&self) -> Instant {
        Instant::now()
    }
}
impl RuntimeClock {
    fn advance(&self, milliseconds: i64) {
        self.0.fetch_add(milliseconds, Ordering::SeqCst);
    }
}
async fn observation(f: &Fixture) -> IvyJson {
    f.journal
        .observations()
        .await
        .unwrap()
        .required("fixture")
        .unwrap()
        .clone()
}
fn readiness_trace(f: &Fixture) -> PathBuf {
    native(f.host.required("runtimeRoot").unwrap()).join("runtime-readiness.jsonl")
}
fn prepare_readiness(f: &Fixture) {
    let root = native(f.candidate.required("artifactRoot").unwrap());
    let destination = json::stringify(&path(&readiness_trace(f)), usize::MAX).unwrap();
    fs::write(root.join("dist/ready.mjs"),format!("import fs from 'node:fs';fs.appendFileSync({destination},JSON.stringify({{config:process.env.IVY_INSTANCE_CONFIG,launch:process.env.IVY_LAUNCH_ID}})+'\\n');")).unwrap();
}
fn linux_runtime(f: &Fixture, host: &UnitHost) -> NativeActivationRuntime {
    NativeActivationRuntime::with_adapters(
        f.root.0.clone(),
        preparation(),
        PathBuf::from(env!("CARGO_BIN_EXE_ivy-job")),
        f.inspector.clone(),
        "linux".into(),
        DockerAdapter::new(f.root.0.clone()).unwrap(),
        host.adapter(),
    )
}
fn linux_candidate(candidate: &mut IvyJson) {
    nested(candidate, "platform", "os", "linux".into());
    #[cfg(windows)]
    {
        // Windows admits a root-relative path on the current drive. Keep the
        // actual fixture directory while presenting POSIX syntax to the pure
        // unit renderer; the systemd OS itself remains explicitly modeled.
        let root = candidate
            .required("artifactRoot")
            .unwrap()
            .as_str()
            .unwrap()
            .replace('\\', "/");
        assert!(root.as_bytes().get(1) == Some(&b':'));
        candidate.insert("artifactRoot", root[2..].into());
    }
}
#[tokio::test]
async fn native_systemd_reconciliation_fences_passive_revisions_and_retires_before_explicit_replacement()
 {
    let f = Fixture::new("fixture", |_, candidate, _| linux_candidate(candidate)).await;
    prepare_readiness(&f);
    let host = UnitHost::default();
    host.0.lock().unwrap().active = "active".into();
    let runtime = linux_runtime(&f, &host);
    let first = materialize(&f, true).await;
    f.journal.restore_target(first.clone()).await.unwrap();
    write_health(&f, &first);
    let rendered = ivy_host_runtime::systemd::render_systemd_unit(
        &f.host,
        &"fixture".into(),
        &first,
        &f.candidate,
        &f.manifest,
        None,
    )
    .unwrap();
    let file = JsString::from(format!("/etc/systemd/system/{}", rendered.unit));
    host.0.lock().unwrap().files.insert(
        file.clone(),
        rendered.content.to_utf8().unwrap().into_bytes(),
    );
    runtime
        .reconcile(&f.context, Some(first.clone()), false)
        .await
        .unwrap();
    assert_eq!(
        observation(&f).await.required("state").unwrap(),
        &"ready".into()
    );
    runtime
        .reconcile(&f.context, Some(first.clone()), false)
        .await
        .unwrap();
    assert_eq!(
        fs::read_to_string(readiness_trace(&f))
            .unwrap()
            .lines()
            .count(),
        1
    );
    assert_eq!(host.calls("start"), 0);
    let next = materialize(&f, true).await;
    f.journal.restore_target(next.clone()).await.unwrap();
    write_health(&f, &next);
    runtime
        .reconcile(&f.context, Some(next.clone()), false)
        .await
        .unwrap();
    let unknown = observation(&f).await;
    assert_eq!(unknown.required("state").unwrap(), &"unknown".into());
    assert_eq!(unknown.required("code").unwrap(), &"target_conflict".into());
    assert_eq!(host.calls("stop"), 0);
    assert_eq!(host.calls("start"), 0);
    runtime
        .reconcile(&f.context, Some(next.clone()), true)
        .await
        .unwrap();
    assert_eq!(
        observation(&f).await.required("state").unwrap(),
        &"ready".into()
    );
    assert_eq!(host.calls("stop"), 1);
    assert_eq!(host.calls("start"), 1);
    assert_eq!(host.calls("daemon-reload"), 1);
    assert_eq!(
        fs::read_to_string(readiness_trace(&f))
            .unwrap()
            .lines()
            .count(),
        2
    );
    let stopped = ivy_host_runtime::target::stopped_target(&next);
    f.journal.restore_target(stopped.clone()).await.unwrap();
    runtime
        .reconcile(&f.context, Some(stopped.clone()), false)
        .await
        .unwrap();
    let observed = observation(&f).await;
    assert_eq!(observed.required("state").unwrap(), &"stopped".into());
    assert_eq!(
        observed.required("targetRevision").unwrap(),
        stopped.required("revision").unwrap()
    );
    assert_eq!(host.calls("stop"), 2);
    let count = host.0.lock().unwrap().trace.len();
    runtime.dispose().await.unwrap();
    assert_eq!(host.0.lock().unwrap().trace.len(), count);
    f.close().await;
}
#[tokio::test]
async fn native_systemd_passive_missing_definition_never_installs_or_starts() {
    let f = Fixture::new("fixture", |_, candidate, _| linux_candidate(candidate)).await;
    let host = UnitHost::default();
    let runtime = linux_runtime(&f, &host);
    let target = materialize(&f, true).await;
    assert_eq!(
        runtime
            .reconcile(&f.context, Some(target), false)
            .await
            .unwrap_err()
            .code,
        "target_changed"
    );
    assert!(host.0.lock().unwrap().files.is_empty());
    assert!(host.0.lock().unwrap().trace.is_empty());
    assert!(
        f.journal
            .observations()
            .await
            .unwrap()
            .get("fixture")
            .is_none()
    );
    runtime.reconcile(&f.context, None, false).await.unwrap();
    assert_eq!(
        observation(&f).await.required("state").unwrap(),
        &"idle".into()
    );
    runtime.dispose().await.unwrap();
    f.close().await;
}

#[tokio::test]
async fn native_systemd_readiness_restarts_its_deadline_after_ready_and_retires_expired_owner_pid()
{
    let mut f = Fixture::new("fixture", |_, candidate, _| linux_candidate(candidate)).await;
    let clock = Arc::new(RuntimeClock(std::sync::atomic::AtomicI64::new(
        f.context.clock.utc_millis(),
    )));
    f.context.clock = clock.clone();
    let host = UnitHost::default();
    host.0.lock().unwrap().active = "active".into();
    let runtime = linux_runtime(&f, &host);
    let target = materialize(&f, true).await;
    let rendered = ivy_host_runtime::systemd::render_systemd_unit(
        &f.host,
        &"fixture".into(),
        &target,
        &f.candidate,
        &f.manifest,
        None,
    )
    .unwrap();
    host.0.lock().unwrap().files.insert(
        format!("/etc/systemd/system/{}", rendered.unit).into(),
        rendered.content.to_utf8().unwrap().into_bytes(),
    );
    write_health(&f, &target);
    runtime
        .reconcile(&f.context, Some(target.clone()), false)
        .await
        .unwrap();
    assert_eq!(
        observation(&f).await.required("ownerPid").unwrap(),
        &IvyJson::Number(42.0)
    );
    clock.advance(60_000);
    runtime
        .reconcile(&f.context, Some(target.clone()), false)
        .await
        .unwrap();
    assert_eq!(
        observation(&f).await.required("state").unwrap(),
        &"starting".into()
    );
    assert_eq!(host.calls("stop"), 0);
    clock.advance(1001);
    runtime
        .reconcile(&f.context, Some(target.clone()), false)
        .await
        .unwrap();
    let expired = observation(&f).await;
    assert_eq!(
        expired.required("ownerPid").unwrap(),
        &IvyJson::Number(std::process::id() as f64)
    );
    assert_eq!(
        selected_observation(&expired),
        IvyJson::object([
            (
                "targetRevision",
                target.required("revision").unwrap().clone()
            ),
            ("candidateId", hash()),
            ("buildId", hash()),
            ("state", "failed".into()),
            ("code", "readiness_deadline".into()),
            ("message", "Systemd readiness deadline expired.".into()),
        ])
    );
    assert_eq!(host.calls("stop"), 1);
    assert_eq!(host.calls("start"), 0);
    runtime.dispose().await.unwrap();
    f.close().await;
}

#[tokio::test]
async fn native_locked_service_journal_uses_only_exact_generation_bound_ready_evidence() {
    use ivy_store::rusqlite::Connection;
    for case in [
        "exact-stale-ready",
        "wrong-launch",
        "wrong-build",
        "not-ready",
    ] {
        let f = Fixture::new("phone-bridge", |_, _, manifest| {
            manifest.insert(
                "storage",
                v(r#"{"minReadableFormat":1,"maxReadableFormat":1,"writeFormat":1}"#),
            );
        })
        .await;
        let data = ivy_host_runtime::layout::service_paths(&f.host, &"fixture".into()).unwrap();
        let root = native(data.required("data").unwrap());
        fs::create_dir_all(&root).unwrap();
        let database = root.join("phone-commands.sqlite");
        let owner = Connection::open(&database).unwrap();
        owner
            .execute_batch("PRAGMA journal_mode=DELETE; PRAGMA user_version=1; BEGIN EXCLUSIVE")
            .unwrap();
        let target = IvyJson::object([
            ("schemaVersion", IvyJson::Number(1.0)),
            ("instanceId", "fixture".into()),
            ("candidateId", hash()),
            ("revision", "locked-journal-launch".into()),
            ("desired", "running".into()),
            ("configPath", path(&f.root.0.join("retained/config.json"))),
            ("requestedAt", "2026-01-01T00:00:00.000Z".into()),
        ]);
        f.journal.restore_target(target.clone()).await.unwrap();
        observe(&f, Some(&target), "ready", true).await;
        let mut observed = observation(&f).await;
        observed.insert(
            "health",
            IvyJson::object([
                ("schemaVersion", IvyJson::Number(1.0)),
                ("instanceId", "fixture".into()),
                ("componentId", "phone-bridge".into()),
                (
                    "buildId",
                    if case == "wrong-build" {
                        format!("sha256:{}", "b".repeat(64)).into()
                    } else {
                        hash()
                    },
                ),
                ("pid", IvyJson::Number(42.0)),
                ("bootId", "locked-phone-owner".into()),
                ("startedAt", "2026-01-01T00:00:00.000Z".into()),
                ("observedAt", "2026-01-01T00:00:00.000Z".into()),
                ("ready", (case != "not-ready").into()),
                ("generation", IvyJson::Null),
                ("details", "".into()),
                (
                    "launchId",
                    if case == "wrong-launch" {
                        "other-launch".into()
                    } else {
                        target.required("revision").unwrap().clone()
                    },
                ),
            ]),
        );
        f.journal
            .record_observation("fixture", observed)
            .await
            .unwrap();
        let actual = wire(
            f.runtime
                .compatibility(&f.context, &hash().as_str().unwrap(), None)
                .await,
        );
        let expected = oracle(f.input());
        pair(&format!("locked-journal-{case}"), &actual, &expected);
        assert_eq!(
            actual.required("ok").unwrap(),
            &(case == "exact-stale-ready").into()
        );
        owner.execute_batch("ROLLBACK").unwrap();
        drop(owner);
        f.close().await;
    }
}
