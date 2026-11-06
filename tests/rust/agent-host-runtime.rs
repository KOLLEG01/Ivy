use super::*;
use futures_util::{SinkExt, StreamExt};
use ivy_contracts::{
    agent_schema::validate_agent,
    hash::tool_definition_hash,
    native_backend::{BackendIdentity, NativeProvider},
};
use tokio::sync::{Notify, mpsc, oneshot};

struct Bootstrap {
    released: AtomicBool,
    waiting: AtomicUsize,
    wake: Notify,
}
impl Bootstrap {
    fn release(&self) {
        self.released.store(true, Ordering::Release);
        self.wake.notify_waiters();
    }
}
fn environment_reply(method: &str) -> IvyJson {
    match method {
        "objects.stat" => IvyJson::Null,
        "discovery.instructions" => {
            IvyJson::object([("instructions", "Fixture instructions.".into())])
        }
        _ => panic!("Unexpected environment method: {method}"),
    }
}
impl RpcClient for Bootstrap {
    fn request<'a>(&'a self, method: &'a str, _: IvyJson, _: RequestOptions) -> RequestFuture<'a> {
        Box::pin(async move {
            self.waiting.fetch_add(1, Ordering::AcqRel);
            loop {
                let wake = self.wake.notified();
                if self.released.load(Ordering::Acquire) {
                    break;
                }
                wake.await;
            }
            Ok(environment_reply(method))
        })
    }
}
enum PeerCommand {
    Invoke {
        method: String,
        args: IvyJson,
        operation: Option<String>,
        reply: oneshot::Sender<IvyJson>,
    },
    Reconnect(oneshot::Sender<()>),
}
struct Peer {
    url: String,
    commands: mpsc::Sender<PeerCommand>,
    connects: Arc<AtomicUsize>,
    statuses: Arc<AtomicUsize>,
    heartbeat: Arc<Mutex<IvyJson>>,
    inventories: Arc<Mutex<std::collections::HashSet<String>>>,
    notifications: Arc<Mutex<Vec<IvyJson>>>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Peer {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Peer {
    async fn new() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let (commands, mut receiver) = mpsc::channel::<PeerCommand>(16);
        let connects = Arc::new(AtomicUsize::new(0));
        let sessions = connects.clone();
        let statuses = Arc::new(AtomicUsize::new(0));
        let status_calls = statuses.clone();
        let heartbeat = Arc::new(Mutex::new(IvyJson::Null));
        let latest = heartbeat.clone();
        let inventories = Arc::new(Mutex::new(std::collections::HashSet::<String>::new()));
        let observed_inventory = inventories.clone();
        let notifications = Arc::new(Mutex::new(Vec::new()));
        let observed_notifications = notifications.clone();
        let task = tokio::spawn(async move {
            let mut next = 0_u64;
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
                let mut definitions = std::collections::HashMap::<String, String>::new();
                let mut pending =
                    std::collections::HashMap::<JsString, oneshot::Sender<IvyJson>>::new();
                let mut generation = 0;
                loop {
                    tokio::select! {
                        command = receiver.recv() => {
                            match command {
                                None => return,
                                Some(PeerCommand::Reconnect(done)) => {
                                    let _ = socket.close(None).await; let _ = done.send(()); break;
                                }
                                Some(PeerCommand::Invoke { method, args, operation, reply }) => {
                                    next += 1;
                                    let id = JsString::from(format!("invoke-{next}"));
                                    let mut params = IvyJson::object([
                                        ("qualifiedName", method.clone().into()), ("arguments", args),
                                        ("definitionHash", definitions.get(&method).unwrap_or_else(|| panic!("No projected definition: {method}")).as_str().into()),
                                        ("generation", IvyJson::Number(generation as f64)), ("callerPrincipalId", "fixture.caller".into()),
                                    ]);
                                    if let Some(operation) = operation { params.insert("operationId", operation.into()); }
                                    let request = IvyJson::object([("jsonrpc", "2.0".into()), ("id", IvyJson::String(id.clone())), ("method", "provider.invoke".into()), ("params", params)]);
                                    pending.insert(id, reply);
                                    socket.send(tokio_tungstenite::tungstenite::Message::Text(canonical(&request, BYTES).unwrap().into())).await.unwrap();
                                }
                            }
                        }
                        message = socket.next() => {
                            let Some(Ok(message)) = message else { break; };
                            let Ok(line) = message.into_text() else { continue; };
                            let frame = parse(&line, BYTES).unwrap();
                            if let Some(method) = frame.get("method") {
                                let Some(id) = frame.get("id") else {
                                    if method.is_str("service.notification") { observed_notifications.lock().unwrap().push(field(field(&frame, "params"), "payload").clone()); }
                                    continue;
                                };
                                let params = frame.required("params").unwrap();
                                let result = match method.as_str().unwrap().as_str() {
                                    "service.connect" => {
                                        sessions.fetch_add(1, Ordering::AcqRel);
                                        // Reconnection to a new Hive epoch may reuse a generation number.
                                        generation = 1;
                                        assert!(params.get("nativeVersion").is_none(), "The host must not advertise a single provider version");
                                        IvyJson::object([("generation", IvyJson::Number(generation as f64))])
                                    }
                                    "registry.sync" => {
                                        for namespace in field(params, "namespaces").as_array().unwrap() {
                                            let ns = field(namespace, "namespace").as_str().unwrap();
                                            for tool in field(namespace, "tools").as_array().unwrap() {
                                                definitions.insert(format!("{ns}.{}", field(tool, "name").as_str().unwrap()), tool_definition_hash(tool).unwrap());
                                            }
                                        }
                                        IvyJson::object::<&str>([])
                                    }
                                    "system.status" => {
                                        status_calls.fetch_add(1, Ordering::AcqRel);
                                        IvyJson::object([("runtimeEpoch", if sessions.load(Ordering::Acquire) == 1 { "fixture-epoch" } else { "fixture-epoch-restarted" }.into()), ("nativeToolTransport", IvyJson::object([("parameters", "json".into()), ("preReadyOutcome", "not_executed".into())]))])
                                    }
                                    "service.heartbeat" => {
                                        *latest.lock().unwrap() = params.clone();
                                        let mut identities = std::collections::HashSet::new();
                                        for diagnostic in field(params, "diagnostics").as_array().unwrap() {
                                            let identity = hash_json(&IvyJson::object([("code", field(diagnostic, "code").clone()), ("resource", field(diagnostic, "resource").clone())])).unwrap();
                                            assert!(identities.insert(identity), "Backend diagnostics must have distinct Hive identities");
                                        }
                                        IvyJson::object([("ready", field(params, "ready").clone())])
                                    }
                                    "inventory.sync" => {
                                        ivy_contracts::core_schema::validate_input("inventory.sync", params).unwrap();
                                        observed_inventory.lock().unwrap().insert(field(params, "namespace").as_str().unwrap());
                                        IvyJson::object::<&str>([])
                                    }
                                    "objects.stat" | "discovery.instructions" => environment_reply(&method.as_str().unwrap()),
                                    method => panic!("Unexpected host request, including any negative native probe: {method}"),
                                };
                                let reply = IvyJson::object([("jsonrpc", "2.0".into()), ("id", id.clone()), ("result", result)]);
                                socket.send(tokio_tungstenite::tungstenite::Message::Text(canonical(&reply, BYTES).unwrap().into())).await.unwrap();
                            } else if let Some(id) = frame.get("id").and_then(IvyJson::as_js_string) {
                                if let Some(reply) = pending.remove(id) { let _ = reply.send(frame); }
                            }
                        }
                    }
                }
            }
        });
        Self {
            url,
            commands,
            connects,
            statuses,
            heartbeat,
            inventories,
            notifications,
            task,
        }
    }
    async fn begin(
        &self,
        method: &str,
        args: IvyJson,
        operation: Option<String>,
    ) -> oneshot::Receiver<IvyJson> {
        let (reply, result) = oneshot::channel();
        self.commands
            .send(PeerCommand::Invoke {
                method: method.into(),
                args,
                operation,
                reply,
            })
            .await
            .unwrap();
        result
    }
    async fn invoke(&self, method: &str, args: IvyJson, operation: Option<String>) -> IvyJson {
        tokio::time::timeout(
            Duration::from_secs(15),
            self.begin(method, args, operation).await,
        )
        .await
        .unwrap()
        .unwrap()
    }
    async fn reconnect(&self) {
        let (done, wait) = oneshot::channel();
        self.commands
            .send(PeerCommand::Reconnect(done))
            .await
            .unwrap();
        wait.await.unwrap();
    }
}
fn success(frame: &IvyJson) -> IvyJson {
    assert!(frame.get("error").is_none(), "{frame:?}");
    field(frame, "result").clone()
}
fn empty() -> IvyJson {
    IvyJson::object::<&str>([])
}
async fn until(mut predicate: impl FnMut() -> bool) -> Result<(), tokio::time::error::Elapsed> {
    tokio::time::timeout(Duration::from_secs(30), async {
        while !predicate() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
}
fn identity(id: &str, provider: NativeProvider) -> BackendIdentity {
    BackendIdentity::new(id.into(), provider, format!("{id}-account")).unwrap()
}
fn save_catalog(artifact: &std::path::Path, version: &str, executable_hash: &str) {
    let relative = format!("specs/native/codex-{version}/catalog.json");
    let mut value = catalog(&relative);
    value.insert("nativeExecutableHash", executable_hash.into());
    let output = artifact.join("dist").join(relative);
    fs::create_dir_all(output.parent().unwrap()).unwrap();
    fs::write(output, canonical(&value, BYTES).unwrap()).unwrap();
}
fn path(value: &std::path::Path) -> IvyJson {
    IvyJson::String(node_path::from_native(value))
}

// Real production launch boundaries with inert provider endpoints. No installed
// service, account or external model participates in this fixture.
fn host_fixture(root: &Root, url: &str) -> (InstanceConfig, AgentRuntimeBindings, Arc<Bootstrap>) {
    let artifact = root.0.join("artifact");
    let bin = root.0.join("bin");
    let sdk = root.0.join("sdk");
    fs::create_dir_all(artifact.join("dist/native")).unwrap();
    fs::create_dir_all(&bin).unwrap();
    let executable = bin.join(if cfg!(windows) {
        "native.exe"
    } else {
        "native"
    });
    let compiled =
        Command::new(std::env::var_os("IVY_TEST_RUSTC").unwrap_or_else(|| "rustc".into()))
            .args(["--edition=2024", "-C", "debuginfo=0"])
            .arg(repository().join("tests/fixtures/native-launch-child.rs"))
            .arg("-o")
            .arg(&executable)
            .output()
            .unwrap();
    assert!(
        compiled.status.success(),
        "{}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    let native_hash = digest(fs::read(&executable).unwrap());
    let node = Command::new(std::env::var_os("IVY_TEST_NODE").unwrap_or_else(|| "node".into()))
        .args(["-p", "process.execPath"])
        .output()
        .unwrap();
    assert!(node.status.success());
    let node = PathBuf::from(String::from_utf8(node.stdout).unwrap().trim());
    let node_hash = digest(fs::read(&node).unwrap());
    fs::write(
        bin.join("launch-fixture.txt"),
        format!(
            "{}\n{}\nlaunch-host\n{}\n{}\n{{}}\n0.154.0\n",
            node.display(),
            repository()
                .join("tests/fixtures/native-connection-child.mjs")
                .display(),
            root.0.join("codex/native").display(),
            root.0.join("codex-child").display()
        ),
    )
    .unwrap();
    fs::create_dir_all(sdk.join("bridge")).unwrap();
    for name in ["main.mjs", "session.mjs"] {
        fs::copy(
            repository()
                .join("services/agent-manager/claude/bridge")
                .join(name),
            sdk.join("bridge").join(name),
        )
        .unwrap();
    }
    let module = sdk.join("node_modules/@anthropic-ai/claude-agent-sdk");
    fs::create_dir_all(&module).unwrap();
    fs::write(
        module.join("package.json"),
        r#"{"type":"module","exports":"./index.mjs"}"#,
    )
    .unwrap();
    fs::copy(
        repository().join("tests/fixtures/agent-claude-sdk-native.mjs"),
        module.join("fixture.mjs"),
    )
    .unwrap();
    fs::write(module.join("index.mjs"), "import {existsSync,writeFileSync} from 'node:fs';import {join} from 'node:path';const root=process.env.CLAUDE_CONFIG_DIR;writeFileSync(join(root,'fixture-sdk.pid'),String(process.pid));setInterval(()=>{if(existsSync(join(root,'fixture-exit')))process.exit(7)},10).unref();export {query} from './fixture.mjs';").unwrap();
    fs::write(sdk.join("package-lock.json"), "{}").unwrap();
    let mut manifest = Vec::new();
    for name in ["bridge/main.mjs", "bridge/session.mjs", "package-lock.json"] {
        manifest.extend_from_slice(name.as_bytes());
        manifest.push(0);
        manifest.extend_from_slice(digest(fs::read(sdk.join(name)).unwrap())[7..].as_bytes());
        manifest.push(0);
    }
    let bundle = digest(manifest);
    save_catalog(&artifact, "0.154.0", &native_hash);
    save_catalog(&artifact, "0.142.3", &bundle);
    let limits = IvyJson::object([
        ("maxOperations", 100.0.into()),
        ("maxJournalBytes", 67108864.0.into()),
        ("maxPendingInputs", 8.0.into()),
        ("maxNotificationBytes", 1048576.0.into()),
    ]);
    let shell = if cfg!(windows) {
        let shell = bin.join("pwsh.exe");
        fs::copy(&executable, &shell).unwrap();
        let target = PathBuf::from(env!("CARGO_BIN_EXE_ivy-agent-manager"));
        let target = target.parent().unwrap();
        fs::copy(
            target.join("ivy-job.exe"),
            artifact.join("dist/native/ivy-job.exe"),
        )
        .unwrap();
        for entry in fs::read_dir(target).unwrap().flatten() {
            if entry.path().extension().is_some_and(|v| v == "dll") {
                fs::copy(
                    entry.path(),
                    artifact.join("dist/native").join(entry.file_name()),
                )
                .unwrap();
            }
        }
        Some(IvyJson::object([
            ("executable", path(&shell)),
            ("executableHash", native_hash.clone().into()),
        ]))
    } else {
        None
    };
    let mut backends = Vec::new();
    for (id, provider, version, hash) in [
        (
            "codex",
            NativeProvider::Codex,
            "0.154.0",
            native_hash.as_str(),
        ),
        ("claude", NativeProvider::Claude, "0.142.3", bundle.as_str()),
        (
            "unavailable",
            NativeProvider::Codex,
            "0.154.0",
            native_hash.as_str(),
        ),
    ] {
        let data = root.0.join(id);
        fs::create_dir_all(&data).unwrap();
        let native_path = if provider == NativeProvider::Claude {
            node.clone()
        } else if id == "unavailable" {
            bin.join("missing.exe")
        } else {
            executable.clone()
        };
        let mut settings = IvyJson::object([
            ("backend", identity(id, provider).json()),
            ("nativeExecutable", path(&native_path)),
            ("nativeVersion", version.into()),
            ("nativeExecutableHash", hash.into()),
            ("codexHome", path(&data.join("native"))),
            ("limits", limits.clone()),
        ]);
        if let Some(shell) = &shell {
            settings.insert("windowsShell", shell.clone());
        }
        if provider == NativeProvider::Claude {
            settings.insert(
                "appServer",
                IvyJson::object([
                    ("mode", "claude-adapter".into()),
                    ("adapterRoot", path(&sdk)),
                    ("nodeExecutableHash", node_hash.as_str().into()),
                    ("claudeExecutable", path(&node)),
                    ("claudeExecutableHash", node_hash.as_str().into()),
                    ("defaultModel", "sonnet".into()),
                ]),
            );
        } else {
            settings.insert("patcherEnabled", false.into());
            settings.insert(
                "appServer",
                IvyJson::object([("mode", "owned-stdio".into())]),
            );
        }
        backends.push(IvyJson::object([
            ("dataRoot", path(&data)),
            ("settings", settings),
        ]));
    }
    let mut value = config(root, "0.154.0".into()).json().clone();
    value.insert("dataRoot", path(&root.0.join("host")));
    value.insert("artifactRoot", path(&artifact));
    value.insert("publicBaseUrl", url.into());
    value.insert(
        "settings",
        IvyJson::object([("backends", IvyJson::Array(backends))]),
    );
    let mut bindings = ivy_agent_manager::runtime::native_bindings(
        &InstanceConfig::from_json(value.clone()).unwrap(),
    )
    .unwrap();
    bindings.home.account_home = node_path::from_native(&root.0);
    bindings.home.user_home = None;
    bindings.home.environment.retain(|key, _| {
        key.to_utf8().is_ok_and(|name| {
            matches!(
                name.to_ascii_lowercase().as_str(),
                "systemroot" | "windir" | "temp" | "tmp" | "path" | "pathext"
            )
        })
    });
    let bootstrap = Arc::new(Bootstrap {
        released: AtomicBool::new(false),
        waiting: AtomicUsize::new(0),
        wake: Notify::new(),
    });
    bindings.bootstrap = bootstrap.clone();
    (
        InstanceConfig::from_json(value).unwrap(),
        bindings,
        bootstrap,
    )
}
async fn statuses(peer: &Peer) -> Vec<IvyJson> {
    let result = success(&peer.invoke("agent.backends", empty(), None).await);
    validate_agent("BackendsResult", &result).unwrap();
    field(&result, "backends").as_array().unwrap().to_vec()
}
fn state(rows: &[IvyJson], id: &str) -> String {
    rows.iter()
        .find(|v| v.get("backend").and_then(|v| v.get("backendId")) == Some(&id.into()))
        .unwrap()
        .required("state")
        .unwrap()
        .as_str()
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sqlite_catalog_and_reset_failures_do_not_prevent_other_backends_from_starting() {
    for variant in [
        "sqlite",
        "account-pin",
        "missing-catalog",
        "invalid-catalog",
        "catalog-pin",
        "reset",
        "reset-account",
    ] {
        let root = Root::new();
        let peer = Peer::new().await;
        let (config, bindings, bootstrap) = host_fixture(&root, &peer.url);
        bootstrap.release();
        let mut value = config.json().clone();
        // Put the failed account first, so success proves preparation continues.
        let IvyJson::Array(entries) = value
            .get_mut("settings")
            .unwrap()
            .get_mut("backends")
            .unwrap()
        else {
            panic!("Backend array required");
        };
        entries.swap(0, 1);
        let selected_settings = entries[0].required("settings").unwrap().clone();
        let journal = root.0.join("claude/journal/agent-runtime.sqlite");
        let mut originals = Vec::new();
        if variant.contains("catalog") {
            let mut state = LocalAgentState::open(
                &journal,
                "fixture.agent".into(),
                StateLimits {
                    max_operations: 100,
                    max_journal_bytes: 67108864,
                },
            )
            .unwrap();
            state
                .bind_backend(
                    NativeProvider::Claude,
                    Some(&identity("claude", NativeProvider::Claude)),
                )
                .unwrap();
            state.set_runtime_epoch("fixture-epoch".into()).unwrap();
            let mut memory = NativeJournal::new_scoped(
                Owner {
                    service_node_id: "fixture.agent".into(),
                    native_version: "0.142.3".into(),
                    native_executable_hash: text(&selected_settings, "nativeExecutableHash"),
                },
                Limits {
                    max_operations: 100,
                    max_journal_bytes: 67108864,
                    max_pending_inputs: 8,
                },
                Some(identity("claude", NativeProvider::Claude)),
            )
            .unwrap();
            for (operation, pending) in [("retained-terminal", false), ("retained-unknown", true)] {
                let id = ivy_agent_manager::native_journal::Identity {
                    caller_principal_id: "fixture.caller".into(),
                    operation_id: ivy_contracts::operation::operation_id(
                        "fixture-epoch",
                        chrono::Utc::now().timestamp_millis() as u64,
                        operation,
                    )
                    .unwrap()
                    .into(),
                };
                let mut receipt = if pending {
                    memory
                        .accept(&id, &"model/list".into(), &empty())
                        .unwrap()
                        .0
                } else {
                    memory.prevent(&id, &"model/list".into(), &empty()).unwrap()
                };
                if pending {
                    receipt.insert("phase", "dispatched".into());
                    receipt.insert("epoch", "original-native-epoch".into());
                    receipt.insert("requestId", "original-native-request".into());
                }
                let key = hash_json(&id.value()).unwrap();
                let saved = state.write("operation", &key, &receipt, None).unwrap();
                originals.push((id, receipt, saved.pin));
            }
            state.close_shared().unwrap();
        }
        let catalog = root
            .0
            .join("artifact/dist/specs/native/codex-0.142.3/catalog.json");
        let expected = match variant {
            "sqlite" => {
                fs::create_dir_all(journal.parent().unwrap()).unwrap();
                fs::write(&journal, "unreadable original journal").unwrap();
                None
            }
            "account-pin" => {
                let mut state = LocalAgentState::open(
                    &journal,
                    "fixture.agent".into(),
                    StateLimits {
                        max_operations: 100,
                        max_journal_bytes: 67108864,
                    },
                )
                .unwrap();
                state
                    .bind_backend(
                        NativeProvider::Claude,
                        Some(
                            &BackendIdentity::new(
                                "claude".into(),
                                NativeProvider::Claude,
                                "another-account".into(),
                            )
                            .unwrap(),
                        ),
                    )
                    .unwrap();
                state.close_shared().unwrap();
                Some("native_backend_mismatch")
            }
            "missing-catalog" => {
                fs::remove_file(&catalog).unwrap();
                None
            }
            "invalid-catalog" => {
                fs::write(&catalog, "{}").unwrap();
                None
            }
            "catalog-pin" => {
                let mut catalog_value =
                    parse(&fs::read_to_string(&catalog).unwrap(), BYTES).unwrap();
                catalog_value.insert("nativeExecutableHash", digest("another-build").into());
                fs::write(&catalog, canonical(&catalog_value, BYTES).unwrap()).unwrap();
                Some("native_build_mismatch")
            }
            "reset" => {
                fs::write(root.0.join("claude/runtime-reset.json"), "{}").unwrap();
                Some("invalid_arguments")
            }
            "reset-account" => {
                let mut marker = reset_marker("claude", NativeProvider::Claude);
                marker.insert(
                    "backend",
                    BackendIdentity::new(
                        "claude".into(),
                        NativeProvider::Claude,
                        "another-account".into(),
                    )
                    .unwrap()
                    .json(),
                );
                fs::write(
                    root.0.join("claude/runtime-reset.json"),
                    canonical(&marker, BYTES).unwrap(),
                )
                .unwrap();
                Some("native_backend_mismatch")
            }
            _ => unreachable!(),
        };
        let build = IvyJson::object([
            ("buildId", field(&value, "buildId").clone()),
            ("version", field(&value, "version").clone()),
        ]);
        let entry = entry_files(&root, &value, &build);
        let sessions = root.0.join("codex/native/sessions");
        fs::create_dir_all(&sessions).unwrap();
        fs::write(sessions.join("transient"), "reset this account only").unwrap();
        fs::write(
            root.0.join("codex/native/config.toml"),
            "retained auth settings",
        )
        .unwrap();
        fs::write(
            root.0.join("codex/runtime-reset.json"),
            canonical(&reset_marker("codex", NativeProvider::Codex), BYTES).unwrap(),
        )
        .unwrap();
        let runtime = start_agent_manager(entry, node_runtime(&root), bindings)
            .await
            .unwrap();
        until(|| runtime.service.connection().is_ok())
            .await
            .unwrap();
        let rows = statuses(&peer).await;
        let failed = rows
            .iter()
            .find(|row| {
                row.get("backend")
                    .and_then(|backend| backend.get("backendId"))
                    == Some(&"claude".into())
            })
            .unwrap();
        assert_eq!(failed.get("state"), Some(&"failed".into()), "{variant}");
        assert_eq!(
            failed.get("phase"),
            Some(&"preparation".into()),
            "{variant}"
        );
        if let Some(expected) = expected {
            assert_eq!(failed.get("code"), Some(&expected.into()), "{variant}");
        }
        assert_eq!(
            failed.get("operationReadsAvailable"),
            Some(&(!originals.is_empty()).into())
        );
        assert!(
            failed.get("catalogHash").is_none()
                && failed.get("operations").is_none()
                && failed.get("pendingInputs").is_none()
        );
        assert_eq!(state(&rows, "codex"), "ready", "{variant}");
        success(&peer.invoke("codex.model/list", empty(), None).await);
        for method in ["agent.invoke", "agent.operation", "agent.status"] {
            if method == "agent.operation" && !originals.is_empty() {
                continue;
            }
            let args = identity("claude", NativeProvider::Claude)
                .arguments(if method == "agent.status" {
                    empty()
                } else if method == "agent.invoke" {
                    IvyJson::object([
                        ("operationId", "unknown-original".into()),
                        ("nativeVersion", "0.142.3".into()),
                        ("method", "model/list".into()),
                        ("expectedDefinitionHash", digest("unadmitted").into()),
                        ("params", empty()),
                    ])
                } else {
                    IvyJson::object([("operationId", "unknown-original".into())])
                })
                .unwrap();
            let refused = peer.invoke(method, args, None).await;
            let error = field(field(&refused, "error"), "data");
            assert_eq!(error.get("code"), failed.get("code"), "{variant} {method}");
            assert!(error.get("details").is_none_or(
                |details| details.get("kind") != Some(&"agent_operation_absent".into())
            ));
        }
        for (id, original, _) in &originals {
            let observed = success(
                &peer
                    .invoke(
                        "agent.operation",
                        identity("claude", NativeProvider::Claude)
                            .arguments(IvyJson::object([(
                                "operationId",
                                IvyJson::String(id.operation_id.clone()),
                            )]))
                            .unwrap(),
                        None,
                    )
                    .await,
            );
            let mut expected = original.clone();
            if original.get("phase") == Some(&"dispatched".into()) {
                expected.insert("phase", "outcome_unknown".into());
                expected.insert("code", "native_connection_lost".into());
            }
            same(&observed, &expected);
        }
        until(|| peer.heartbeat.lock().unwrap().get("ready") == Some(&true.into()))
            .await
            .unwrap();
        assert_eq!(peer.connects.load(Ordering::Acquire), 1);
        peer.reconnect().await;
        until(|| {
            peer.connects.load(Ordering::Acquire) == 2 && runtime.service.connection().is_ok()
        })
        .await
        .unwrap();
        assert_eq!(state(&statuses(&peer).await, "codex"), "ready");
        if !originals.is_empty() {
            let state = LocalAgentState::open(
                &journal,
                "fixture.agent".into(),
                StateLimits {
                    max_operations: 100,
                    max_journal_bytes: 67108864,
                },
            )
            .unwrap();
            for (id, receipt, pin) in &originals {
                let saved = state
                    .read("operation", &hash_json(&id.value()).unwrap())
                    .unwrap()
                    .unwrap();
                same(&saved.value, receipt);
                assert_eq!(&saved.pin, pin);
            }
            drop(state);
        }
        assert!(!root.0.join("claude/claude-config/fixture-sdk.pid").exists());
        if variant == "sqlite" {
            assert_eq!(
                fs::read_to_string(&journal).unwrap(),
                "unreadable original journal"
            );
        }
        if variant.starts_with("reset") {
            assert!(!journal.exists());
            assert!(!root.0.join("claude/runtime-reset-applied.json").exists());
        }
        assert!(!sessions.join("transient").exists());
        assert_eq!(
            fs::read_to_string(root.0.join("codex/native/config.toml")).unwrap(),
            "retained auth settings"
        );
        let applied = parse(
            &fs::read_to_string(root.0.join("codex/runtime-reset-applied.json")).unwrap(),
            BYTES,
        )
        .unwrap();
        assert!(BackendIdentity::matches(
            &applied,
            Some(&identity("codex", NativeProvider::Codex))
        ));
        tokio::time::timeout(Duration::from_secs(15), runtime.close().unwrap().wait())
            .await
            .unwrap()
            .unwrap();
        assert!(root.0.join("codex/owned-process-stops.json").exists());
    }
}

fn reset_marker(id: &str, provider: NativeProvider) -> IvyJson {
    IvyJson::object([
        ("schemaVersion", 1.0.into()),
        ("resetId", "explicit-fixture-reset".into()),
        ("runtimeEpoch", "fixture-epoch".into()),
        ("completedAt", "2026-01-02T03:04:05.000Z".into()),
        ("backend", identity(id, provider).json()),
    ])
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_host_with_no_prepared_backend_reports_diagnostics_and_closes_without_waiting_for_a_launch()
 {
    for abandon in [false, true] {
        let root = Root::new();
        let peer = Peer::new().await;
        let (config, bindings, _) = host_fixture(&root, &peer.url);
        for id in ["codex", "claude", "unavailable"] {
            let journal = root.0.join(id).join("journal");
            fs::create_dir_all(&journal).unwrap();
            fs::write(
                journal.join("agent-runtime.sqlite"),
                "preserve unreadable journal",
            )
            .unwrap();
        }
        let runtime = start_prepared_agent(config, bindings).await.unwrap();
        until(|| {
            peer.connects.load(Ordering::Acquire) > 0
                && peer.heartbeat.lock().unwrap().get("ready").is_some()
        })
        .await
        .unwrap();
        assert!(runtime.service.connection().is_err());
        let refused = peer.invoke("agent.backends", empty(), None).await;
        let error = field(field(&refused, "error"), "data");
        assert_eq!(error.get("code"), Some(&"stale_generation".into()));
        assert_eq!(error.get("outcome"), Some(&"not_executed".into()));
        until(|| peer.heartbeat.lock().unwrap().get("ready") == Some(&false.into()))
            .await
            .unwrap();
        let heartbeat = peer.heartbeat.lock().unwrap().clone();
        ivy_contracts::core_schema::validate_input("service.heartbeat", &heartbeat).unwrap();
        assert_eq!(
            field(&heartbeat, "diagnostics").as_array().unwrap().len(),
            3
        );
        if abandon {
            drop(runtime);
        } else {
            tokio::time::timeout(Duration::from_secs(5), runtime.close().unwrap().wait())
                .await
                .unwrap()
                .unwrap();
        }
        until(|| {
            fs::read_to_string(root.0.join("host/health.json"))
                .is_ok_and(|body| body.contains("AgentManager stopped: agent_stopped."))
        })
        .await
        .unwrap();
        assert!(!root.0.join("codex-child").exists());
        assert!(!root.0.join("claude/claude-config/fixture-sdk.pid").exists());
        for id in ["codex", "claude", "unavailable"] {
            assert_eq!(
                fs::read_to_string(root.0.join(id).join("journal/agent-runtime.sqlite")).unwrap(),
                "preserve unreadable journal"
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn one_host_runs_both_engines_and_preserves_receipts_through_partial_start_failure_reconnect_and_drain()
 {
    let root = Root::new();
    let peer = Peer::new().await;
    let (config, bindings, bootstrap) = host_fixture(&root, &peer.url);
    let runtime = start_prepared_agent(config, bindings).await.unwrap();
    until(|| runtime.service.connection().is_ok() && bootstrap.waiting.load(Ordering::Acquire) > 0)
        .await
        .unwrap_or_else(|_| {
            panic!(
                "Host startup: connects={}, bootstrap={}, heartbeat={}",
                peer.connects.load(Ordering::Acquire),
                bootstrap.waiting.load(Ordering::Acquire),
                canonical(&peer.heartbeat.lock().unwrap(), BYTES).unwrap()
            )
        });
    let before = statuses(&peer).await;
    assert_eq!(state(&before, "codex"), "ready");
    assert_eq!(state(&before, "claude"), "starting");
    assert_eq!(state(&before, "unavailable"), "failed");
    // Reading admitted storage does not wait for or start the native engine.
    for (id, provider) in [
        ("codex", NativeProvider::Codex),
        ("claude", NativeProvider::Claude),
    ] {
        let origin = success(
            &peer
                .invoke(
                    "agent.storageIdentity",
                    identity(id, provider).arguments(empty()).unwrap(),
                    None,
                )
                .await,
        );
        ivy_contracts::agent_schema::validate_agent("StorageIdentity", &origin).unwrap();
        assert_eq!(origin.get("serviceNodeId"), Some(&"fixture.agent".into()));
        assert_eq!(
            origin.get("originalServiceNodeId"),
            Some(&"fixture.agent".into())
        );
        assert_eq!(origin.get("provider"), Some(&provider.as_str().into()));
    }
    let refused = peer.invoke("claude.model/list", empty(), None).await;
    assert_eq!(
        refused
            .get("error")
            .and_then(|e| e.get("data"))
            .and_then(|e| e.get("code")),
        Some(&"native_unavailable".into())
    );
    bootstrap.release();
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if state(&statuses(&peer).await, "claude") == "ready" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    success(&peer.invoke("codex.model/list", empty(), None).await);
    let models = success(&peer.invoke("claude.model/list", empty(), None).await);
    assert!(!field(&models, "data").as_array().unwrap().is_empty());
    assert_eq!(peer.connects.load(Ordering::Acquire), 1);
    assert_eq!(peer.statuses.load(Ordering::Acquire), 1);
    let operation = format!(
        "fixture-epoch:{}:original-thread",
        chrono::Utc::now().timestamp_millis()
    );
    let thread = success(
        &peer
            .invoke(
                "claude.thread/start",
                IvyJson::object([
                    ("cwd", path(&root.0)),
                    ("model", "sonnet".into()),
                    ("approvalPolicy", "never".into()),
                    ("sandbox", "workspace-write".into()),
                ]),
                Some(operation.clone()),
            )
            .await,
    );
    assert!(thread.get("thread").and_then(|v| v.get("id")).is_some());
    let lookup = identity("claude", NativeProvider::Claude)
        .arguments(IvyJson::object([(
            "operationId",
            operation.as_str().into(),
        )]))
        .unwrap();
    let original = success(&peer.invoke("agent.operation", lookup.clone(), None).await);
    until(|| {
        let inventories = peer.inventories.lock().unwrap();
        inventories.contains("codex") && inventories.contains("claude")
    })
    .await
    .expect("Backend inventory snapshots must use separate registered namespaces");
    until(|| {
        peer.notifications
            .lock()
            .unwrap()
            .iter()
            .any(|event| event.get("method") == Some(&"thread/started".into()))
    })
    .await
    .expect("The started native thread must publish its scoped notification");
    for event in peer.notifications.lock().unwrap().iter() {
        let backend = BackendIdentity::from_json(field(event, "backend")).unwrap();
        assert!(["codex", "claude"].contains(&backend.id()));
        if event.get("method") == Some(&"thread/started".into()) {
            assert_eq!(backend, identity("claude", NativeProvider::Claude));
        }
    }
    fs::write(
        root.0.join("claude/claude-config/fixture-exit"),
        "exit exact fixture process",
    )
    .unwrap();
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if state(&statuses(&peer).await, "claude") == "failed" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    same(
        &success(&peer.invoke("agent.operation", lookup.clone(), None).await),
        &original,
    );
    assert_eq!(state(&statuses(&peer).await, "codex"), "ready");
    success(&peer.invoke("codex.model/list", empty(), None).await);
    assert_eq!(peer.connects.load(Ordering::Acquire), 1);
    peer.reconnect().await;
    until(|| peer.connects.load(Ordering::Acquire) == 2 && runtime.service.connection().is_ok())
        .await
        .expect("The healthy backend must reconcile the second Hive generation");
    same(
        &success(&peer.invoke("agent.operation", lookup, None).await),
        &original,
    );
    success(&peer.invoke("codex.model/list", empty(), None).await);
    assert_eq!(peer.statuses.load(Ordering::Acquire), 2);
    fs::write(root.0.join("codex-child.hold"), "hold accepted read").unwrap();
    let accepted = peer.begin("codex.model/list", empty(), None).await;
    until(|| root.0.join("codex-child.pending").exists())
        .await
        .expect("The fixture must have accepted its native read");
    let closing = runtime.close().unwrap();
    let refused = peer.invoke("codex.model/list", empty(), None).await;
    assert_eq!(
        refused
            .get("error")
            .and_then(|v| v.get("data"))
            .and_then(|v| v.get("code")),
        Some(&"service_unavailable".into())
    );
    fs::remove_file(root.0.join("codex-child.hold")).unwrap();
    success(&accepted.await.unwrap());
    let stopped = tokio::time::timeout(Duration::from_secs(15), closing.wait())
        .await
        .unwrap();
    assert_eq!(
        stopped.unwrap_err().code,
        "adapter_error",
        "Retain the failed backend's original owner error after draining every backend"
    );
    assert_eq!(
        runtime.closed().await.unwrap(),
        JsString::from("agent_shutdown_failed")
    );
    for id in ["codex", "claude", "unavailable"] {
        let state = LocalAgentState::open(
            &root.0.join(id).join("journal/agent-runtime.sqlite"),
            "fixture.agent".into(),
            StateLimits {
                max_operations: 100,
                max_journal_bytes: 67108864,
            },
        )
        .unwrap();
        drop(state);
        assert!(
            !root.0.join(id).join("health.json").exists(),
            "Only the host writes health"
        );
    }
    let health = parse(
        &fs::read_to_string(root.0.join("host/health.json")).unwrap(),
        BYTES,
    )
    .unwrap();
    assert_eq!(health.get("ready"), Some(&false.into()));
}

#[tokio::test]
async fn invalid_backend_accounts_and_overlapping_storage_are_refused_before_side_effects() {
    for variant in [
        "missing-identity",
        "missing-home",
        "missing-mode",
        "duplicate-id",
        "engine",
        "relative-root",
        "shared-root",
        "nested-root",
        "shared-home",
        "shared-server-home",
        "unknown-setting",
    ] {
        let root = Root::new();
        let mut value = config(&root, "0.154.0".into()).json().clone();
        let base = field(&value, "settings").clone();
        let mut entries = Vec::new();
        for id in ["first", "second"] {
            let mut settings = base.clone();
            settings.insert("backend", identity(id, NativeProvider::Codex).json());
            settings.insert("codexHome", path(&root.0.join(format!("home-{id}"))));
            settings.insert(
                "appServer",
                IvyJson::object([("mode", "owned-stdio".into())]),
            );
            entries.push(IvyJson::object([
                ("dataRoot", path(&root.0.join(id))),
                ("settings", settings),
            ]));
        }
        let first_root = field(&entries[0], "dataRoot").clone();
        let first_home = field(field(&entries[0], "settings"), "codexHome").clone();
        if variant == "shared-server-home" {
            for entry in &mut entries {
                entry.get_mut("settings").unwrap().insert(
                    "appServer",
                    IvyJson::object([
                        ("mode", "external-proxy".into()),
                        (
                            "expectedCodexHome",
                            path(&root.0.join("shared-server-home")),
                        ),
                    ]),
                );
            }
        }
        let second = &mut entries[1];
        match variant {
            "missing-identity" => {
                second.get_mut("settings").unwrap().remove("backend");
            }
            "missing-home" => {
                second.get_mut("settings").unwrap().remove("codexHome");
            }
            "missing-mode" => {
                second.get_mut("settings").unwrap().remove("appServer");
            }
            "duplicate-id" => {
                second
                    .get_mut("settings")
                    .unwrap()
                    .insert("backend", identity("first", NativeProvider::Codex).json());
            }
            "engine" => {
                second
                    .get_mut("settings")
                    .unwrap()
                    .insert("backend", identity("second", NativeProvider::Claude).json());
            }
            "relative-root" => {
                second.insert("dataRoot", "relative".into());
            }
            "shared-root" => {
                second.insert("dataRoot", first_root);
            }
            "nested-root" => {
                second.insert("dataRoot", path(&root.0.join("first/nested")));
            }
            "shared-home" => {
                second
                    .get_mut("settings")
                    .unwrap()
                    .insert("codexHome", first_home);
            }
            "shared-server-home" => {}
            "unknown-setting" => {
                second
                    .get_mut("settings")
                    .unwrap()
                    .insert("unexpected", true.into());
            }
            _ => unreachable!(),
        }
        value.insert(
            "settings",
            IvyJson::object([("backends", IvyJson::Array(entries))]),
        );
        let result =
            start_prepared_agent(InstanceConfig::from_json(value).unwrap(), bindings(&root)).await;
        assert!(result.is_err(), "{variant}");
        assert!(!root.0.join("health.json").exists(), "{variant}");
        for id in ["first", "second"] {
            assert!(!root.0.join(id).exists(), "{variant}");
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn abandoning_a_host_drains_startup_without_launching_the_retired_backend() {
    let root = Root::new();
    let peer = Peer::new().await;
    let (config, bindings, bootstrap) = host_fixture(&root, &peer.url);
    let runtime = start_prepared_agent(config, bindings).await.unwrap();
    until(|| runtime.service.connection().is_ok() && bootstrap.waiting.load(Ordering::Acquire) > 0)
        .await
        .expect("The healthy engine must be ready while the other startup is blocked");
    drop(runtime);
    bootstrap.release();
    until(|| {
        fs::read_to_string(root.0.join("host/health.json"))
            .is_ok_and(|text| text.contains("AgentManager stopped: agent_stopped."))
    })
    .await
    .expect("Dropping the runtime must retain and finish its host cleanup");
    assert!(
        !root.0.join("claude/claude-config/fixture-sdk.pid").exists(),
        "The backend retired during bootstrap must not launch its SDK afterward"
    );
    assert!(root.0.join("codex/owned-process-stops.json").exists());
}

struct Capabilities(IvyJson, Mutex<Vec<String>>);
impl RpcClient for Capabilities {
    fn request<'a>(&'a self, method: &'a str, _: IvyJson, _: RequestOptions) -> RequestFuture<'a> {
        Box::pin(async move {
            self.1.lock().unwrap().push(method.into());
            Ok(self.0.clone())
        })
    }
}
#[tokio::test]
async fn positive_transport_handshake_never_dispatches_a_native_action() {
    for (parameters, outcome, accepted) in [
        (Some("json"), "not_executed", true),
        (None, "not_executed", false),
        (Some("object"), "not_executed", false),
        (Some("json"), "unknown", false),
    ] {
        let mut status = IvyJson::object([("runtimeEpoch", "epoch".into())]);
        if let Some(parameters) = parameters {
            status.insert(
                "nativeToolTransport",
                IvyJson::object([
                    ("parameters", parameters.into()),
                    ("preReadyOutcome", outcome.into()),
                ]),
            );
        }
        let peer = Capabilities(status, Mutex::new(Vec::new()));
        let result = ivy_agent_manager::reconciliation::native_transport_epoch(&peer).await;
        assert_eq!(result.is_ok(), accepted);
        if let Err(error) = result {
            assert_eq!(error.code, "hive_native_json_unsupported");
            assert_eq!(error.outcome, ivy_contracts::Outcome::NotExecuted);
        }
        assert_eq!(*peer.1.lock().unwrap(), ["system.status"]);
    }
}
