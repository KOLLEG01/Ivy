use super::*;
use crate::store::{CollectorStore, Limits};
use ivy_contracts::{hash::hash_json, json::parse};
use ivy_sdk::rpc::RequestFuture;
use std::{collections::HashMap, path::PathBuf};
use tokio::sync::Notify;

const TIME: &str = "2026-01-02T03:04:05.006Z";
fn json(source: &str) -> IvyJson {
    parse(source, usize::MAX).unwrap()
}
fn digest(c: char) -> IvyJson {
    format!("sha256:{}", c.to_string().repeat(64)).into()
}
fn catalog() -> &'static IvyJson {
    static VALUE: std::sync::OnceLock<IvyJson> = std::sync::OnceLock::new();
    VALUE.get_or_init(|| {
        let mut value = json(include_str!(
            "../../specs/native/codex-0.159.2/catalog.json"
        ));
        let methods = value
            .required("clientRequests")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .filter(|method| {
                [
                    "config/read",
                    "thread/start",
                    "thread/unsubscribe",
                    "thread/loaded/list",
                    "mcpServer/tool/call",
                ]
                .iter()
                .any(|name| method.get("method") == Some(&(*name).into()))
            })
            .map(|method| {
                let mut method = method.clone();
                method.insert("inputSchema", true.into());
                method.insert("outputSchema", true.into());
                method
            })
            .collect();
        value.insert("clientRequests", IvyJson::Array(methods));
        for name in [
            "serverRequests",
            "serverNotifications",
            "clientNotifications",
            "derivedLegacyTypes",
        ] {
            value.insert(name, IvyJson::Array(vec![]));
        }
        value.insert("nativeExecutableHash", digest('d'));
        value
    })
}
#[derive(Clone, Copy, Default)]
enum Mode {
    #[default]
    Normal,
    HoldStart,
    LostStart,
    UnknownStart,
    HoldTool,
    LostUnsubscribe,
    HoldUnsubscribe,
    HoldInvokeBinding,
    HoldDirectBinding,
    RejectTool,
    UnknownToolAdmission,
}
#[derive(Default)]
struct Client {
    backend: Mutex<Option<BackendIdentity>>,
    status_backend: Mutex<Option<BackendIdentity>>,
    management_calls: Mutex<Vec<IvyJson>>,
    bindings: Mutex<Vec<(String, String)>>,
    replace_backend_on_config: Mutex<Option<BackendIdentity>>,
    mcp_unavailable: AtomicBool,
    store: Mutex<Option<StoreOwner>>,
    operations: Mutex<HashMap<String, IvyJson>>,
    invokes: Mutex<Vec<IvyJson>>,
    direct: Mutex<Vec<IvyJson>>,
    native_configuration: Mutex<Option<IvyJson>>,
    configuration_reads: Mutex<Vec<IvyJson>>,
    mode: Mutex<Mode>,
    epoch: Mutex<Option<String>>,
    stopped_epochs: Mutex<Vec<IvyJson>>,
    loaded: Mutex<Option<IvyJson>>,
    entered: Notify,
    release: Notify,
}
impl Client {
    fn catalog(&self) -> IvyJson {
        let mut value = catalog().clone();
        if self.mcp_unavailable.load(Ordering::SeqCst) {
            let methods = value
                .remove("clientRequests")
                .unwrap()
                .into_array()
                .unwrap()
                .into_iter()
                .filter(|method| method.get("method") != Some(&"mcpServer/tool/call".into()))
                .collect();
            value.insert("clientRequests", IvyJson::Array(methods));
        }
        value
    }
    fn status(&self) -> IvyJson {
        let mut value = IvyJson::object([
            ("serviceNodeId", "native-agent".into()),
            ("hostId", "fixture".into()),
            ("nativeVersion", "0.159.2".into()),
            ("nativeExecutableHash", digest('d')),
            ("catalogHash", hash_json(&self.catalog()).unwrap().into()),
            (
                "epoch",
                self.epoch
                    .lock()
                    .unwrap()
                    .as_deref()
                    .unwrap_or("epoch")
                    .into(),
            ),
            ("pid", 42.0.into()),
            ("state", "ready".into()),
            ("observedAt", TIME.into()),
            ("code", IvyJson::Null),
            ("initialized", object()),
            ("pendingInputs", 0.0.into()),
            (
                "operations",
                json(r#"{"retained":0,"maximum":100,"bytes":0,"maximumBytes":1048576}"#),
            ),
            ("observedMethods", IvyJson::Array(vec![])),
            (
                "processStops",
                IvyJson::Array(self.stopped_epochs.lock().unwrap().clone()),
            ),
            (
                "connection",
                json(
                    r#"{"mode":"owned-stdio","ownsServer":true,"ownsHome":false,"actualHome":"/fixture","actualVersion":"0.159.2","socketPath":null,"serverPid":null,"lifecycle":"owned-stdio","mcpIdentity":"instance-environment"}"#,
                ),
            ),
        ]);
        let selected = self
            .status_backend
            .lock()
            .unwrap()
            .clone()
            .or_else(|| self.backend.lock().unwrap().clone());
        if let Some(backend) = selected {
            value.insert("backend", backend.json());
        }
        value
    }
    fn observation(&self, args: &IvyJson, phase: &str) -> IvyJson {
        let method = args.required("method").unwrap().clone();
        let params = args.required("params").unwrap().clone();
        let request_hash = hash_json(&IvyJson::object([
            ("method", method.clone()),
            ("params", params.clone()),
        ]))
        .unwrap();
        let mut value = IvyJson::object([
            ("schemaVersion", 1.0.into()),
            ("operationId", args.required("operationId").unwrap().clone()),
            ("callerPrincipalId", "collector".into()),
            ("serviceNodeId", "native-agent".into()),
            ("nativeVersion", "0.159.2".into()),
            ("nativeExecutableHash", digest('d')),
            ("method", method),
            ("params", params),
            ("requestHash", request_hash.into()),
            ("createdAt", TIME.into()),
            ("updatedAt", TIME.into()),
            ("phase", phase.into()),
            (
                "epoch",
                self.epoch
                    .lock()
                    .unwrap()
                    .as_deref()
                    .unwrap_or("epoch")
                    .into(),
            ),
            ("requestId", 1.0.into()),
            ("reply", IvyJson::Null),
            ("code", IvyJson::Null),
        ]);
        if let Some(backend) = self.backend.lock().unwrap().as_ref() {
            value.insert("backend", backend.json());
        }
        if phase == "succeeded" {
            value.insert("reply", IvyJson::object([("result", Self::reply(args))]));
        }
        if phase == "outcome_unknown" {
            value.insert("code", "native_exit".into());
        }
        value
    }
    fn reply(args: &IvyJson) -> IvyJson {
        match model::text(args, "method").unwrap().as_str() {
            "thread/start" => json(r#"{"thread":{"id":"own-thread","ephemeral":true}}"#),
            "mcpServer/tool/call" => {
                json(r#"{"content":[{"type":"text","text":"{\"ok\":true}"}],"isError":false}"#)
            }
            "thread/unsubscribe" => object(),
            other => panic!("Unexpected native method {other}"),
        }
    }
    fn finish(&self, method: &str) {
        let mut operations = self.operations.lock().unwrap();
        let mut found = false;
        for operation in operations
            .values_mut()
            .filter(|v| v.get("method") == Some(&method.into()))
        {
            operation.insert("phase", "succeeded".into());
            operation.insert("code", IvyJson::Null);
            operation.insert(
                "reply",
                IvyJson::object([("result", Self::reply(operation))]),
            );
            found = true;
        }
        assert!(found, "No original {method}");
    }
    fn count(&self, method: &str) -> usize {
        self.invokes
            .lock()
            .unwrap()
            .iter()
            .filter(|v| v.get("method") == Some(&method.into()))
            .count()
    }
    fn original(&self, method: &str) -> String {
        model::text(
            self.invokes
                .lock()
                .unwrap()
                .iter()
                .find(|v| v.get("method") == Some(&method.into()))
                .unwrap(),
            "operationId",
        )
        .unwrap()
    }
}
impl RpcClient for Client {
    fn request<'a>(
        &'a self,
        method: &'a str,
        params: IvyJson,
        _: RequestOptions,
    ) -> RequestFuture<'a> {
        Box::pin(async move {
            if method == "system.status" {
                return Ok(json(
                    r#"{"callerPrincipalId":"collector","runtimeEpoch":"hive-epoch"}"#,
                ));
            }
            if method == "tools.list" {
                let namespace = model::text(&params, "namespace")?;
                let name = model::text(&params, "namePrefix")?;
                self.bindings
                    .lock()
                    .unwrap()
                    .push((namespace.clone(), name.clone()));
                if name == "config/read" {
                    if let Some(replacement) = self.replace_backend_on_config.lock().unwrap().take()
                    {
                        *self.backend.lock().unwrap() = Some(replacement);
                    }
                }
                if namespace == "fixture"
                    && matches!(*self.mode.lock().unwrap(), Mode::HoldDirectBinding)
                {
                    self.entered.notify_one();
                    self.release.notified().await;
                }
                if namespace == "agent"
                    && name == "invoke"
                    && matches!(*self.mode.lock().unwrap(), Mode::HoldInvokeBinding)
                {
                    self.entered.notify_one();
                    self.release.notified().await;
                }
                let native = if namespace != "agent" && namespace != "fixture" {
                    let contract = ivy_contracts::native_contract::NativeContract::for_backend(
                        &self.catalog(),
                        self.backend.lock().unwrap().clone(),
                    )?;
                    Some((
                        contract.definition(&name.clone().into())?,
                        contract.definition_hash(&name.clone().into())?.to_owned(),
                    ))
                } else {
                    None
                };
                return Ok(IvyJson::object([
                    (
                        "provider",
                        json(r#"{"node":{"serviceNodeId":"native-agent"}}"#),
                    ),
                    (
                        "items",
                        IvyJson::Array(vec![IvyJson::object([
                            ("qualifiedName", format!("{namespace}.{name}").into()),
                            (
                                "definition",
                                native
                                    .as_ref()
                                    .map(|(definition, _)| definition.clone())
                                    .unwrap_or_else(|| {
                                        IvyJson::object([
                                            ("namespace", namespace.clone().into()),
                                            (
                                                "annotations",
                                                IvyJson::object([(
                                                    "readOnlyHint",
                                                    (name == "read"
                                                        || namespace == "codex"
                                                            && name == "config/read")
                                                        .into(),
                                                )]),
                                            ),
                                            ("name", name.into()),
                                            (
                                                "interfaceVersion",
                                                if namespace == "agent" {
                                                    "1.0.0"
                                                } else {
                                                    "0.159.2"
                                                }
                                                .into(),
                                            ),
                                            ("inputSchema", object()),
                                            ("outputSchema", object()),
                                        ])
                                    }),
                            ),
                            (
                                "definitionHash",
                                native
                                    .as_ref()
                                    .map(|(_, hash)| hash.clone().into())
                                    .unwrap_or_else(|| {
                                        digest(if namespace == "agent" { 'a' } else { 'b' })
                                    }),
                            ),
                            ("catalogHash", hash_json(&self.catalog()).unwrap().into()),
                        ])]),
                    ),
                    ("nextCursor", IvyJson::Null),
                ]));
            }
            assert_eq!(method, "tools.call");
            let args = params.required("arguments")?;
            if model::text(&params, "qualifiedName")?.starts_with("agent.") {
                self.management_calls.lock().unwrap().push(params.clone());
                if !BackendIdentity::matches(args, self.backend.lock().unwrap().as_ref()) {
                    return Err(IvyError::new(
                        "native_backend_mismatch",
                        "The selected account is not configured.",
                    ));
                }
            }
            match model::text(&params, "qualifiedName")?.as_str() {
                "codex.config/read" => {
                    self.configuration_reads.lock().unwrap().push(args.clone());
                    Ok(self.native_configuration.lock().unwrap().clone().unwrap_or_else(|| {
                        json(r#"{"config":{"mcp_servers":{"node_repl":{"command":"node"},"cua_repl":{"command":"node"}}}}"#)
                    }))
                }
                "fixture.read" => {
                    self.direct.lock().unwrap().push(params.clone());
                    Ok(json(r#"{"cursor":"next","items":[1,2]}"#))
                }
                "fixture.write" => {
                    self.direct.lock().unwrap().push(params.clone());
                    Err(IvyError::new("connection_closed", "Lost reply")
                        .with_outcome(Outcome::Unknown))
                }
                "agent.status" => Ok(self.status()),
                "agent.catalog" => Ok(self.catalog()),
                "agent.interact" => {
                    let method = model::text(args, "method")?;
                    let config = if method == "thread/loaded/list" {
                        self.loaded
                            .lock()
                            .unwrap()
                            .clone()
                            .unwrap_or_else(|| json(r#"{"data":["own-thread"],"nextCursor":null}"#))
                    } else {
                        assert_eq!(method, "config/read");
                        self.configuration_reads
                            .lock()
                            .unwrap()
                            .push(args.required("params")?.clone());
                        self.native_configuration.lock().unwrap().clone().unwrap_or_else(|| {
                        json(r#"{"config":{"mcp_servers":{"node_repl":{"command":"node"},"cua_repl":{"command":"node"}}}}"#)
                        })
                    };
                    let mut value = IvyJson::object([
                        ("operationId", args.required("operationId")?.clone()),
                        (
                            "epoch",
                            self.epoch
                                .lock()
                                .unwrap()
                                .as_deref()
                                .unwrap_or("epoch")
                                .into(),
                        ),
                        ("reply", IvyJson::object([("result", config)])),
                    ]);
                    if let Some(backend) = self.backend.lock().unwrap().as_ref() {
                        value.insert("backend", backend.json());
                    }
                    Ok(value)
                }
                "agent.operation" => {
                    let id = model::text(args, "operationId")?;
                    self.operations
                        .lock()
                        .unwrap()
                        .get(&id)
                        .cloned()
                        .ok_or_else(|| {
                            let mut error = IvyError::new("not_found", "No original operation.");
                            error.details = Some(IvyJson::object([
                                ("kind", "agent_operation_absent".into()),
                                ("operationId", id.into()),
                                ("serviceNodeId", "native-agent".into()),
                                ("epoch", "epoch".into()),
                            ]));
                            if let Some(backend) = self.backend.lock().unwrap().as_ref() {
                                error
                                    .details
                                    .as_mut()
                                    .unwrap()
                                    .insert("backend", backend.json());
                            }
                            error
                        })
                }
                "agent.invoke" => {
                    let store = self.store.lock().unwrap().as_ref().unwrap().clone();
                    let id = model::text(args, "operationId")?;
                    let wanted = id.clone();
                    // Assert the durable write, including exact native parameters, precedes every send.
                    let request = args.clone();
                    store
                        .call(move |store| {
                            let mut rows = store
                                .connection()
                                .prepare("SELECT value FROM metadata WHERE key GLOB 'native-mcp:*'")
                                .unwrap();
                            let mut values = rows.query([]).unwrap();
                            let mut found = false;
                            while let Some(row) = values.next().unwrap() {
                                let value = json(&row.get::<_, String>(0).unwrap());
                                for name in ["start", "pending", "unsubscribe"] {
                                    if let Some(call) = value.get(name).filter(|v| {
                                        v.get("operationId") == Some(&wanted.clone().into())
                                    }) {
                                        assert_eq!(call.get("method"), request.get("method"));
                                        assert_eq!(call.get("params"), request.get("params"));
                                        let backend = BackendIdentity::optional(request.get("backend"))?;
                                        assert!(BackendIdentity::matches(value.required("owner")?,backend.as_ref()), "The complete account owner must be persisted before dispatch");
                                        found = true;
                                    }
                                }
                            }
                            assert!(found, "Native dispatch had no durable original identity");
                            Ok(())
                        })
                        .await?;
                    self.invokes.lock().unwrap().push(args.clone());
                    let native_method = model::text(args, "method")?;
                    let mode = *self.mode.lock().unwrap();
                    if native_method == "mcpServer/tool/call" {
                        match mode {
                            Mode::RejectTool => {
                                return Err(IvyError::new("cancelled", "Not admitted.")
                                    .with_outcome(Outcome::NotExecuted));
                            }
                            Mode::UnknownToolAdmission => {
                                return Err(IvyError::new(
                                    "connection_closed",
                                    "Admission unknown.",
                                )
                                .with_outcome(Outcome::Unknown));
                            }
                            _ => (),
                        }
                    }
                    let held = matches!(
                        (mode, native_method.as_str()),
                        (Mode::HoldStart, "thread/start")
                            | (Mode::HoldTool, "mcpServer/tool/call")
                            | (Mode::HoldUnsubscribe, "thread/unsubscribe")
                    );
                    let lost = matches!(
                        (mode, native_method.as_str()),
                        (Mode::LostStart, "thread/start")
                            | (Mode::LostUnsubscribe, "thread/unsubscribe")
                    );
                    let unknown = matches!(
                        (mode, native_method.as_str()),
                        (Mode::UnknownStart, "thread/start")
                    );
                    let value = self.observation(
                        args,
                        if unknown {
                            "outcome_unknown"
                        } else if held || lost {
                            "dispatched"
                        } else {
                            "succeeded"
                        },
                    );
                    self.operations.lock().unwrap().insert(id, value.clone());
                    if held {
                        self.entered.notify_one();
                        self.release.notified().await;
                    }
                    if lost {
                        self.entered.notify_one();
                        return Err(IvyError::new("connection_closed", "Reply lost.")
                            .with_outcome(Outcome::Unknown));
                    }
                    Ok(value)
                }
                other => panic!("Unexpected management method {other}"),
            }
        })
    }
}
struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        let root =
            std::env::temp_dir().join(format!("ivy-collector-native-mcp-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        Self(root)
    }
    fn store(&self) -> StoreOwner {
        StoreOwner::new(
            CollectorStore::open(&self.0.join("collector.sqlite"), Limits::default(), TIME)
                .unwrap(),
        )
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn fixture(mode: Mode) -> (Root, Arc<Client>, Arc<NativeMcpFactory>, NativeMcpSession) {
    let root = Root::new();
    let store = root.store();
    let client = Arc::new(Client::default());
    *client.store.lock().unwrap() = Some(store.clone());
    *client.mode.lock().unwrap() = mode;
    let current = client.clone();
    let factory = Arc::new(NativeMcpFactory::new(
        Arc::new(move || Ok(current.clone())),
        store,
    ));
    let (rpc, tools) = factory.selected().unwrap();
    let cwd = if cfg!(windows) {
        "C:/fixture"
    } else {
        "/fixture"
    };
    let configuration = IvyJson::object([
        ("serviceNodeId", "native-agent".into()),
        ("cwd", cwd.into()),
        ("server", "fixture".into()),
        ("tools", IvyJson::Array(vec!["echo".into()])),
    ]);
    let session = NativeMcpSession::new(
        rpc,
        tools,
        configuration,
        CancellationToken::new(),
        factory.contexts.clone(),
    )
    .unwrap();
    (root, client, factory, session)
}
async fn retained(factory: &NativeMcpFactory, client: &Client) -> Option<Context> {
    factory
        .contexts
        .read(format!("native-mcp:{}", client.original("thread/start")))
        .await
        .unwrap()
}
async fn call(session: &NativeMcpSession) -> IvyResult<IvyJson> {
    session.call("echo".into(), object()).await
}
#[tokio::test]
async fn headless_overrides_only_disable_existing_local_servers() {
    for configured in [
        r#"{"other":{"url":"https://fixture.invalid/mcp"}}"#,
        r#"{"node_repl":{"command":"node"},"other":{"command":"fixture"}}"#,
    ] {
        let (_root, client, _factory, session) = fixture(Mode::Normal);
        let configuration = json(&format!(r#"{{"config":{{"mcp_servers":{configured}}}}}"#));
        *client.native_configuration.lock().unwrap() = Some(configuration.clone());
        call(&session).await.unwrap();
        call(&session).await.unwrap();
        let reads = client.configuration_reads.lock().unwrap().clone();
        assert_eq!(
            reads.len(),
            1,
            "Read effective configuration only on context creation"
        );
        assert_eq!(reads[0].get("includeLayers"), Some(&false.into()));
        assert_eq!(reads[0].get("cwd"), session.0.configuration.get("cwd"));
        let start = client.invokes.lock().unwrap()[0].clone();
        let overrides = start
            .required("params")
            .unwrap()
            .required("config")
            .unwrap();
        let servers = configuration
            .required("config")
            .unwrap()
            .required("mcp_servers")
            .unwrap();
        let disabled = IvyJson::Bool(false);
        assert_eq!(
            overrides.get("mcp_servers.node_repl.enabled"),
            servers.get("node_repl").map(|_| &disabled)
        );
        assert_eq!(overrides.get("mcp_servers.cua_repl.enabled"), None);
        assert_eq!(
            overrides.as_object().unwrap().len(),
            usize::from(servers.get("node_repl").is_some())
        );
        assert_eq!(
            *client.native_configuration.lock().unwrap(),
            Some(configuration)
        );
        session.close().await.unwrap();
    }
}
#[tokio::test]
async fn explicit_ivy_bindings_preserve_core_and_service_envelopes_and_never_start_native() {
    let (_root, client, factory, _) = fixture(Mode::Normal);
    let configuration = json(
        r#"{"transport":"ivy","callerPrincipalId":"collector","bindings":[{"kind":"core","tool":"hive_status"},{"kind":"service","tool":"read","serviceNodeId":"native-agent","qualifiedName":"fixture.read","interfaceVersion":"0.159.2"},{"kind":"service","tool":"write","serviceNodeId":"native-agent","qualifiedName":"fixture.write","interfaceVersion":"0.159.2"}]}"#,
    );
    crate::schema::parse_schema("mcpSchema", &configuration).unwrap();
    let (rpc, tools) = factory.selected().unwrap();
    let session =
        crate::ivy_mcp::IvyMcp::new(rpc, tools, configuration.clone(), CancellationToken::new());
    let core = session.call("hive_status".into(), object()).await.unwrap();
    assert_eq!(
        core,
        json(r#"{"result":{"callerPrincipalId":"collector","runtimeEpoch":"hive-epoch"}}"#)
    );
    assert_eq!(
        core.get("result").unwrap().get("runtimeEpoch"),
        Some(&"hive-epoch".into())
    );
    let args =
        json(r#"{"serviceNodeId":"native-agent","input":{"view":"history","cursor":"original"}}"#);
    assert_eq!(
        session.call("read".into(), args.clone()).await.unwrap(),
        json(r#"{"result":{"cursor":"next","items":[1,2]}}"#)
    );
    let sent = client.direct.lock().unwrap()[0].clone();
    assert_eq!(sent.get("arguments"), args.get("input"));
    assert_eq!(
        sent.get("expectedCallerPrincipalId"),
        Some(&"collector".into())
    );
    for (tool, args) in [
        ("ungranted", object()),
        ("hive_status", json(r#"{"method":"system.reset"}"#)),
        (
            "read",
            json(r#"{"serviceNodeId":"other-account","input":{}}"#),
        ),
        (
            "write",
            json(r#"{"serviceNodeId":"native-agent","input":{}}"#),
        ),
        (
            "write",
            json(
                r#"{"serviceNodeId":"native-agent","operationId":"original","input":{"operationId":"other"}}"#,
            ),
        ),
    ] {
        assert!(session.call(tool.into(), args).await.is_err());
    }
    let args = json(
        r#"{"serviceNodeId":"native-agent","operationId":"original","input":{"operationId":"original"}}"#,
    );
    assert_eq!(
        session
            .call("write".into(), args)
            .await
            .unwrap_err()
            .outcome,
        Outcome::Unknown
    );
    assert_eq!(
        client.direct.lock().unwrap().len(),
        2,
        "A lost write reply is never replayed"
    );
    assert_eq!(
        client.direct.lock().unwrap()[1].get("operationId"),
        Some(&"original".into())
    );
    let mut other = configuration.clone();
    other.insert("callerPrincipalId", "other-account".into());
    let (rpc, tools) = factory.selected().unwrap();
    let wrong = crate::ivy_mcp::IvyMcp::new(rpc, tools, other, CancellationToken::new());
    assert_eq!(
        wrong
            .call("hive_status".into(), object())
            .await
            .unwrap_err()
            .code,
        "target_conflict"
    );
    let stop = CancellationToken::new();
    stop.cancel();
    let (rpc, tools) = factory.selected().unwrap();
    let stopped = crate::ivy_mcp::IvyMcp::new(rpc, tools, configuration, stop);
    assert!(stopped.call("hive_status".into(), object()).await.is_err());
    assert!(client.invokes.lock().unwrap().is_empty());
}
#[tokio::test]
async fn ivy_discovery_can_be_cancelled_without_dispatching_a_service_call() {
    let (_root, client, factory, _) = fixture(Mode::HoldDirectBinding);
    let (rpc, tools) = factory.selected().unwrap();
    let signal = CancellationToken::new();
    let session = crate::ivy_mcp::IvyMcp::new(
        rpc,
        tools,
        json(
            r#"{"transport":"ivy","callerPrincipalId":"collector","bindings":[{"kind":"service","tool":"read","serviceNodeId":"native-agent","qualifiedName":"fixture.read","interfaceVersion":"0.159.2"}]}"#,
        ),
        signal.clone(),
    );
    let pending = tokio::spawn(async move {
        session
            .call(
                "read".into(),
                json(r#"{"serviceNodeId":"native-agent","input":{}}"#),
            )
            .await
    });
    entered(&client).await;
    signal.cancel();
    let error = tokio::time::timeout(Duration::from_secs(1), pending)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert_eq!(error.code, "cancelled");
    assert_eq!(error.outcome, Outcome::NotExecuted);
    assert!(client.direct.lock().unwrap().is_empty());
    client.release.notify_one();
}

async fn saved_task(factory: &NativeMcpFactory, id: &str, interval: u64) -> IvyJson {
    let configuration = json(&format!(
        r#"{{"serviceNodeId":"native-agent","cwd":"{}","server":"fixture","tools":["echo"]}}"#,
        if cfg!(windows) {
            "C:/fixture"
        } else {
            "/fixture"
        }
    ));
    let task = IvyJson::object([
        ("id", id.into()),
        ("enabled", true.into()),
        ("intervalSeconds", (interval as f64).into()),
        ("mcp", configuration),
    ]);
    let saved = task.clone();
    factory
        .contexts
        .store
        .call(move |store| {
            store.save(&IvyJson::object([
                ("task", saved),
                ("revision", 1.0.into()),
                ("deleted", false.into()),
            ]))
        })
        .await
        .unwrap();
    task
}
async fn bridge_call(bridge: &dyn McpBridge) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let context = bridge.context();
    let url = model::text(&context, "url").unwrap();
    let address = url
        .strip_prefix("http://")
        .unwrap()
        .strip_suffix("/call")
        .unwrap();
    let token = model::text(&context, "token").unwrap();
    let body = r#"{"tool":"echo","arguments":{}}"#;
    let request = format!(
        "POST /call HTTP/1.1\r\nHost: {address}\r\nAuthorization: Bearer {token}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).await.unwrap();
    response
}
#[tokio::test]
async fn periodic_runs_reuse_exclusively_with_new_bridges_and_release_on_disable() {
    let (_root, client, factory, _) = fixture(Mode::Normal);
    let task = saved_task(&factory, "periodic", 60).await;
    let first = factory
        .open(task.clone(), CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(client.count("thread/start"), 0, "Context starts lazily");
    assert!(
        factory
            .open(task.clone(), CancellationToken::new())
            .await
            .is_err()
    );
    assert!(bridge_call(first.as_ref()).await.contains("200 OK"));
    let original_context = first.context();
    // Long runs retain from their completion, not from their opening time.
    factory
        .slots
        .lock()
        .unwrap()
        .get("periodic")
        .unwrap()
        .retained
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .until = tokio::time::Instant::now() - Duration::from_secs(1);
    first.close().await.unwrap();
    assert_eq!(client.count("thread/unsubscribe"), 0);
    let second = factory
        .open(task.clone(), CancellationToken::new())
        .await
        .unwrap();
    assert_ne!(original_context, second.context());
    drop(first); // An old bridge must not release the next run's exclusive lease.
    assert!(
        factory
            .open(task.clone(), CancellationToken::new())
            .await
            .is_err()
    );
    assert!(bridge_call(second.as_ref()).await.contains("200 OK"));
    assert_eq!(client.count("thread/start"), 1);
    assert_eq!(client.count("mcpServer/tool/call"), 2);
    let params = client.invokes.lock().unwrap()[0]
        .get("params")
        .unwrap()
        .clone();
    assert_eq!(
        params
            .get("config")
            .unwrap()
            .get("mcp_servers.node_repl.enabled"),
        Some(&false.into())
    );
    assert_eq!(
        params
            .get("config")
            .unwrap()
            .get("mcp_servers.cua_repl.enabled"),
        Some(&false.into())
    );
    second.close().await.unwrap();
    factory
        .contexts
        .store
        .call(|s| {
            let mut row = s.get("periodic", false)?.unwrap();
            let mut task = row.required("task")?.clone();
            task.insert("enabled", false.into());
            row.insert("task", task);
            s.save(&row)
        })
        .await
        .unwrap();
    factory.recover().await.unwrap();
    assert_eq!(client.count("thread/unsubscribe"), 1);
    assert!(
        !factory
            .contexts
            .task_pending("periodic".into())
            .await
            .unwrap()
    );
}
#[tokio::test]
async fn transport_tasks_do_not_share_contexts_and_manual_runs_unsubscribe() {
    let (_root, client, factory, _) = fixture(Mode::Normal);
    let periodic = saved_task(&factory, "periodic", 60).await;
    let manual = saved_task(&factory, "manual", 0).await;
    // Existing manual admission also allows disabled tasks. Such a run must
    // retain that capability and must unsubscribe at completion.
    let mut manual = manual;
    manual.insert("enabled", false.into());
    let saved = manual.clone();
    factory
        .contexts
        .store
        .call(move |store| {
            let mut row = store.get("manual", false)?.unwrap();
            row.insert("task", saved);
            store.save(&row)
        })
        .await
        .unwrap();
    let first = factory
        .open(periodic, CancellationToken::new())
        .await
        .unwrap();
    let second = factory
        .open(manual, CancellationToken::new())
        .await
        .unwrap();
    assert!(bridge_call(first.as_ref()).await.contains("200 OK"));
    assert!(bridge_call(second.as_ref()).await.contains("200 OK"));
    first.close().await.unwrap();
    second.close().await.unwrap();
    assert_eq!(client.count("thread/start"), 2);
    assert_eq!(client.count("thread/unsubscribe"), 1);
    assert!(
        !factory
            .contexts
            .task_pending("manual".into())
            .await
            .unwrap()
    );
    factory.close().await;
    assert_eq!(client.count("thread/unsubscribe"), 2);
}
#[tokio::test]
async fn unknown_transport_call_fences_its_task_until_the_original_settles() {
    let (_root, client, factory, _) = fixture(Mode::HoldTool);
    let task = saved_task(&factory, "fenced", 60).await;
    let signal = CancellationToken::new();
    let bridge = factory.open(task.clone(), signal.clone()).await.unwrap();
    let work = {
        let bridge = bridge.clone();
        tokio::spawn(async move { bridge_call(bridge.as_ref()).await })
    };
    entered(&client).await;
    signal.cancel();
    let _ = work.await;
    bridge.close().await.unwrap();
    for _ in 0..3 {
        assert!(
            factory
                .open(task.clone(), CancellationToken::new())
                .await
                .is_err()
        );
    }
    assert_eq!(client.count("thread/start"), 1);
    assert_eq!(client.count("mcpServer/tool/call"), 1);
    client.finish("mcpServer/tool/call");
    *client.mode.lock().unwrap() = Mode::Normal;
    factory.recover().await.unwrap();
    let next = factory.open(task, CancellationToken::new()).await.unwrap();
    assert!(bridge_call(next.as_ref()).await.contains("200 OK"));
    assert_eq!(client.count("thread/start"), 2);
    next.close().await.unwrap();
    factory.close().await;
}
#[tokio::test]
async fn configuration_and_connection_replacement_retire_the_previous_task_context() {
    let root = Root::new();
    let store = root.store();
    let first_client = Arc::new(Client::default());
    *first_client.store.lock().unwrap() = Some(store.clone());
    let selected: Arc<Mutex<Arc<dyn RpcClient>>> = Arc::new(Mutex::new(first_client.clone()));
    let current = selected.clone();
    let factory = NativeMcpFactory::new(
        Arc::new(move || Ok(current.lock().unwrap().clone())),
        store.clone(),
    );
    let mut task = saved_task(&factory, "changing", 60).await;
    let first = factory
        .open(task.clone(), CancellationToken::new())
        .await
        .unwrap();
    assert!(bridge_call(first.as_ref()).await.contains("200 OK"));
    first.close().await.unwrap();
    let mut mcp = task.required("mcp").unwrap().clone();
    mcp.insert("server", "another-account".into());
    task.insert("mcp", mcp);
    let saved = task.clone();
    store
        .call(move |s| {
            let mut row = s.get("changing", false)?.unwrap();
            row.insert("task", saved);
            row.insert("revision", 2.0.into());
            s.save(&row)
        })
        .await
        .unwrap();
    let second = factory
        .open(task.clone(), CancellationToken::new())
        .await
        .unwrap();
    assert!(bridge_call(second.as_ref()).await.contains("200 OK"));
    second.close().await.unwrap();
    assert_eq!(first_client.count("thread/start"), 2);
    assert_eq!(first_client.count("thread/unsubscribe"), 1);
    assert_eq!(
        first_client
            .invokes
            .lock()
            .unwrap()
            .last()
            .unwrap()
            .get("params")
            .unwrap()
            .get("server"),
        Some(&"another-account".into())
    );
    let replacement = Arc::new(Client::default());
    *replacement.store.lock().unwrap() = Some(store);
    *selected.lock().unwrap() = replacement.clone();
    let third = factory.open(task, CancellationToken::new()).await.unwrap();
    assert!(bridge_call(third.as_ref()).await.contains("200 OK"));
    third.close().await.unwrap();
    assert_eq!(first_client.count("thread/unsubscribe"), 2);
    assert_eq!(replacement.count("thread/start"), 1);
    factory.close().await;
    assert_eq!(replacement.count("thread/unsubscribe"), 1);
}
#[tokio::test]
async fn legacy_unassigned_contexts_are_reconciled_before_admitting_reuse() {
    let (_root, client, factory, session) = fixture(Mode::UnknownStart);
    call(&session).await.unwrap_err();
    session.close().await.unwrap();
    let task = saved_task(&factory, "upgraded", 60).await;
    assert!(factory.open(task, CancellationToken::new()).await.is_err());
    assert_eq!(client.count("thread/start"), 1);
}
async fn entered(client: &Client) {
    tokio::time::timeout(Duration::from_secs(5), client.entered.notified())
        .await
        .unwrap();
}
async fn cancel_before_admission(
    session: &NativeMcpSession,
    client: &Client,
    task: tokio::task::JoinHandle<IvyResult<IvyJson>>,
    cancel: &str,
) {
    let close = match cancel {
        "close" => {
            let session = session.clone();
            Some(tokio::spawn(async move { session.close().await }))
        }
        "signal" => {
            session.0.signal.cancel();
            None
        }
        _ => {
            task.abort();
            None
        }
    };
    assert!(
        !tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .unwrap()
            .is_ok_and(|result| result.is_ok())
    );
    client.release.notify_one();
    if let Some(close) = close {
        close.await.unwrap().unwrap();
    } else {
        session.close().await.unwrap();
    }
}

#[tokio::test]
async fn normal_context_is_durably_owned_before_dispatch_and_retires_once() {
    let (_root, client, factory, session) = fixture(Mode::Normal);
    assert!(call(&session).await.is_ok());
    assert!(
        session
            .call(
                "echo".into(),
                json(r#"{"lone":"\ud800","null":null,"false":false,"nested":["😀",{"zero":0}]}"#)
            )
            .await
            .is_ok()
    );
    assert_eq!(client.count("thread/start"), 1);
    assert_eq!(client.count("mcpServer/tool/call"), 2);
    assert!(retained(&factory, &client).await.unwrap().pending.is_none());
    session.close().await.unwrap();
    session.close().await.unwrap();
    factory.recover().await.unwrap();
    assert_eq!(client.count("thread/unsubscribe"), 1);
    assert!(retained(&factory, &client).await.is_none());
    assert!(call(&session).await.is_err());
    assert_eq!(
        client.operations.lock().unwrap().len(),
        4,
        "Original receipts stay with the native owner"
    );
}

fn scoped(session: &mut NativeMcpSession, client: &Client, account: &str) -> BackendIdentity {
    let backend = BackendIdentity::new(
        "worker".into(),
        ivy_contracts::native_backend::NativeProvider::Codex,
        account.into(),
    )
    .unwrap();
    let mut configuration = session.0.configuration.clone();
    configuration.insert("backend", backend.json());
    *session = NativeMcpSession::new(
        session.0.client.clone(),
        session.0.tools.clone(),
        configuration,
        session.0.signal.clone(),
        session.0.contexts.clone(),
    )
    .unwrap();
    *client.backend.lock().unwrap() = Some(backend.clone());
    backend
}

#[tokio::test]
async fn scoped_owner_survives_database_reopen_and_fences_foreign_account_cleanup() {
    let (root, client, factory, mut session) = fixture(Mode::UnknownStart);
    let backend = scoped(&mut session, &client, "work");
    call(&session).await.unwrap_err();
    let context = retained(&factory, &client).await.unwrap();
    assert_eq!(context.owner.backend, Some(backend.clone()));
    let key = context.key().unwrap();
    let original = context.start.operation_id;
    session.close().await.unwrap();
    let store = factory.contexts.store.clone();
    drop(session);
    drop(factory);
    store.close().await.unwrap();
    let reopened = root.store();
    *client.store.lock().unwrap() = Some(reopened.clone());
    let current = client.clone();
    let factory = NativeMcpFactory::new(Arc::new(move || Ok(current.clone())), reopened);
    factory.recover().await.unwrap_err();
    let retained = factory.contexts.read(key.clone()).await.unwrap().unwrap();
    assert_eq!(retained.owner.backend, Some(backend.clone()));
    assert_eq!(retained.start.operation_id, original);
    assert_eq!(client.count("thread/unsubscribe"), 0);
    client.finish("thread/start");
    *client.status_backend.lock().unwrap() = Some(
        BackendIdentity::new(
            "worker".into(),
            ivy_contracts::native_backend::NativeProvider::Codex,
            "other".into(),
        )
        .unwrap(),
    );
    factory.recover().await.unwrap_err();
    assert_eq!(client.count("thread/unsubscribe"), 0);
    assert!(factory.contexts.read(key.clone()).await.unwrap().is_some());
    *client.status_backend.lock().unwrap() = None;
    factory.recover().await.unwrap();
    assert!(factory.contexts.read(key).await.unwrap().is_none());
    assert_eq!(client.count("thread/start"), 1);
    assert_eq!(client.count("thread/unsubscribe"), 1);
    for call in client.management_calls.lock().unwrap().iter() {
        assert!(BackendIdentity::matches(
            call.required("arguments").unwrap(),
            Some(&backend)
        ));
    }
    assert!(
        client
            .bindings
            .lock()
            .unwrap()
            .iter()
            .filter(|(namespace, _)| namespace != "agent")
            .all(|(namespace, _)| namespace == "worker")
    );
    factory.contexts.store.close().await.unwrap();
}

#[tokio::test]
async fn foreign_status_and_account_changes_before_configuration_read_never_start_a_context() {
    for replace_during_discovery in [false, true] {
        let (_root, client, factory, mut session) = fixture(Mode::Normal);
        let selected = scoped(&mut session, &client, "work");
        let other = BackendIdentity::new(
            "worker".into(),
            ivy_contracts::native_backend::NativeProvider::Codex,
            "other".into(),
        )
        .unwrap();
        if replace_during_discovery {
            *client.replace_backend_on_config.lock().unwrap() = Some(other);
        } else {
            *client.status_backend.lock().unwrap() = Some(other);
        }
        let error = call(&session).await.unwrap_err();
        assert_eq!(
            error.code,
            if replace_during_discovery {
                "tool_definition_changed"
            } else {
                "native_owner_mismatch"
            }
        );
        assert_eq!(client.count("thread/start"), 0);
        assert!(client.configuration_reads.lock().unwrap().is_empty());
        assert!(
            factory
                .contexts
                .next(String::new())
                .await
                .unwrap()
                .is_none()
        );
        for call in client.management_calls.lock().unwrap().iter() {
            assert!(BackendIdentity::matches(
                call.required("arguments").unwrap(),
                Some(&selected)
            ));
        }
        session.close().await.unwrap();
    }
}

#[tokio::test]
async fn missing_backend_mcp_capability_is_rejected_before_reads_or_context_creation() {
    let (_root, client, factory, mut session) = fixture(Mode::Normal);
    scoped(&mut session, &client, "work");
    client.mcp_unavailable.store(true, Ordering::SeqCst);
    let error = call(&session).await.unwrap_err();
    assert_eq!(error.code, "native_method_unsupported");
    assert!(client.configuration_reads.lock().unwrap().is_empty());
    assert_eq!(client.count("thread/start"), 0);
    assert!(
        factory
            .contexts
            .next(String::new())
            .await
            .unwrap()
            .is_none()
    );
    session.close().await.unwrap();
}
#[tokio::test]
async fn close_during_start_keeps_the_original_and_retires_its_delayed_thread() {
    let (_root, client, factory, session) = fixture(Mode::HoldStart);
    let pending = session.clone();
    let task = tokio::spawn(async move { call(&pending).await });
    entered(&client).await;
    session.close().await.unwrap();
    assert!(task.await.unwrap().is_err());
    let context = retained(&factory, &client).await.unwrap();
    assert!(context.closing && context.thread.is_none());
    assert_eq!(client.count("thread/unsubscribe"), 0);
    client.finish("thread/start");
    factory.recover().await.unwrap();
    assert_eq!(client.count("thread/start"), 1);
    assert_eq!(client.count("thread/unsubscribe"), 1);
    assert!(retained(&factory, &client).await.is_none());
}
#[tokio::test]
async fn task_cancellation_keeps_the_start_identity() {
    let (_root, client, factory, session) = fixture(Mode::HoldStart);
    let pending = session.clone();
    let task = tokio::spawn(async move { call(&pending).await });
    entered(&client).await;
    session.0.signal.cancel();
    task.await.unwrap().unwrap_err();
    session.close().await.unwrap();
    assert!(retained(&factory, &client).await.unwrap().closing);
    client.finish("thread/start");
    factory.recover().await.unwrap();
    assert!(retained(&factory, &client).await.is_none());
    assert_eq!(client.count("thread/start"), 1);
}
#[tokio::test]
async fn cancellation_before_start_admission_releases_only_the_local_intent() {
    for cancel in ["close", "signal", "drop"] {
        let (_root, client, factory, session) = fixture(Mode::HoldInvokeBinding);
        let pending = session.clone();
        let task = tokio::spawn(async move { call(&pending).await });
        entered(&client).await;
        cancel_before_admission(&session, &client, task, cancel).await;
        factory.recover().await.unwrap();
        assert!(
            factory
                .contexts
                .next(String::new())
                .await
                .unwrap()
                .is_none()
        );
        assert!(client.invokes.lock().unwrap().is_empty(), "{cancel}");
    }
}
#[tokio::test]
async fn cancellation_before_tool_admission_allows_the_original_context_to_close() {
    for cancel in ["close", "signal", "drop"] {
        let (_root, client, factory, mut session) = fixture(Mode::Normal);
        call(&session).await.unwrap();
        // A fresh provider cache makes discovery pause after the next durable
        // pending write, before the native dispatch guard has permitted a send.
        Arc::get_mut(&mut session.0).unwrap().tools = Arc::new(ToolClients::new(client.clone()));
        *client.mode.lock().unwrap() = Mode::HoldInvokeBinding;
        let pending = session.clone();
        let task = tokio::spawn(async move { call(&pending).await });
        entered(&client).await;
        let context = retained(&factory, &client).await.unwrap();
        let original = context.pending.unwrap().operation_id;
        assert!(
            !client
                .operations
                .lock()
                .unwrap()
                .contains_key(&original.to_utf8().unwrap())
        );
        cancel_before_admission(&session, &client, task, cancel).await;
        factory.recover().await.unwrap();
        assert!(retained(&factory, &client).await.is_none(), "{cancel}");
        assert_eq!(client.count("thread/start"), 1, "{cancel}");
        assert_eq!(client.count("mcpServer/tool/call"), 1, "{cancel}");
        assert_eq!(client.count("thread/unsubscribe"), 1, "{cancel}");
    }
}
#[tokio::test]
async fn definitive_rejection_allows_cleanup_but_a_lost_admission_does_not() {
    for mode in [Mode::RejectTool, Mode::UnknownToolAdmission] {
        let (_root, client, factory, session) = fixture(mode);
        call(&session).await.unwrap_err();
        let pending = retained(&factory, &client).await.unwrap().pending.unwrap();
        assert!(
            !client
                .operations
                .lock()
                .unwrap()
                .contains_key(&pending.operation_id.to_utf8().unwrap())
        );
        session.close().await.unwrap();
        if matches!(mode, Mode::RejectTool) {
            factory.recover().await.unwrap();
            assert!(retained(&factory, &client).await.is_none());
            assert_eq!(client.count("thread/unsubscribe"), 1);
        } else {
            factory.recover().await.unwrap_err();
            assert_eq!(
                retained(&factory, &client)
                    .await
                    .unwrap()
                    .pending
                    .unwrap()
                    .operation_id,
                pending.operation_id
            );
            assert_eq!(client.count("thread/unsubscribe"), 0);
        }
        assert_eq!(client.count("thread/start"), 1);
        assert_eq!(client.count("mcpServer/tool/call"), 1);
    }
}
#[tokio::test]
async fn a_process_reopen_cannot_infer_that_an_absent_admission_was_unsent() {
    let (root, client, factory, mut session) = fixture(Mode::Normal);
    call(&session).await.unwrap();
    Arc::get_mut(&mut session.0).unwrap().tools = Arc::new(ToolClients::new(client.clone()));
    *client.mode.lock().unwrap() = Mode::HoldInvokeBinding;
    let pending = session.clone();
    let task = tokio::spawn(async move { call(&pending).await });
    entered(&client).await;
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    let context = retained(&factory, &client).await.unwrap();
    let key = context.key().unwrap();
    let original = context.pending.unwrap().operation_id;
    client.release.notify_one();
    let store = factory.contexts.store.clone();
    drop(session);
    drop(factory);
    store.close().await.unwrap();
    let reopened = root.store();
    *client.store.lock().unwrap() = Some(reopened.clone());
    let current = client.clone();
    let factory = NativeMcpFactory::new(Arc::new(move || Ok(current.clone())), reopened);
    factory.recover().await.unwrap_err();
    assert_eq!(
        factory
            .contexts
            .read(key)
            .await
            .unwrap()
            .unwrap()
            .pending
            .unwrap()
            .operation_id,
        original
    );
    assert_eq!(client.count("thread/start"), 1);
    assert_eq!(client.count("mcpServer/tool/call"), 1);
    assert_eq!(client.count("thread/unsubscribe"), 0);
}
#[tokio::test]
async fn unsent_cleanup_is_released_after_close_or_recovery_times_out() {
    for cleanup in ["close", "drop", "recover"] {
        let (_root, client, factory, mut session) = fixture(Mode::Normal);
        call(&session).await.unwrap();
        Arc::get_mut(&mut session.0).unwrap().tools = Arc::new(ToolClients::new(client.clone()));
        *client.mode.lock().unwrap() = Mode::HoldInvokeBinding;
        let task = if cleanup == "recover" {
            let context = retained(&factory, &client).await.unwrap();
            factory.contexts.mark_closing(&context).await.unwrap();
            // Recovery must discover its own uncached management binding too.
            *factory.cached.lock().unwrap() = None;
            let factory = factory.clone();
            tokio::spawn(async move { factory.recover().await })
        } else {
            let session = session.clone();
            tokio::spawn(async move { session.close().await })
        };
        entered(&client).await;
        let original = retained(&factory, &client)
            .await
            .unwrap()
            .unsubscribe
            .unwrap()
            .operation_id;
        if cleanup == "drop" {
            task.abort();
            assert!(task.await.unwrap_err().is_cancelled());
        } else {
            let result = tokio::time::timeout(Duration::from_secs(6), task)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(result.is_ok(), cleanup == "close");
            assert!(
                retained(&factory, &client)
                    .await
                    .unwrap()
                    .unsubscribe
                    .is_none()
            );
        }
        assert_eq!(client.count("thread/unsubscribe"), 0);
        *client.mode.lock().unwrap() = Mode::Normal;
        client.release.notify_one();
        if cleanup == "drop" {
            session.close().await.unwrap();
        } else {
            factory.recover().await.unwrap();
        }
        assert!(retained(&factory, &client).await.is_none(), "{cleanup}");
        assert_eq!(client.count("thread/unsubscribe"), 1);
        assert!(
            !client
                .operations
                .lock()
                .unwrap()
                .contains_key(&original.to_utf8().unwrap())
        );
    }
}
#[tokio::test]
async fn lost_start_is_not_replayed_and_survives_a_real_database_reopen() {
    let (root, client, factory, session) = fixture(Mode::LostStart);
    let pending = session.clone();
    let task = tokio::spawn(async move { call(&pending).await });
    entered(&client).await;
    // The SDK polls the original after a lost admission reply. Losing that
    // waiter must not lose the already persisted identity or dispatch again.
    task.abort();
    let _ = task.await;
    call(&session).await.unwrap_err();
    assert_eq!(client.count("thread/start"), 1);
    // Simulate a crash before close: the old session did not mark itself closing.
    let old_key = retained(&factory, &client).await.unwrap().key().unwrap();
    assert!(
        factory
            .contexts
            .next(String::new())
            .await
            .unwrap()
            .is_none()
    );
    let store = factory.contexts.store.clone();
    drop(session);
    drop(factory);
    store.close().await.unwrap();
    let reopened = root.store();
    *client.store.lock().unwrap() = Some(reopened.clone());
    let current = client.clone();
    let factory = NativeMcpFactory::new(Arc::new(move || Ok(current.clone())), reopened);
    factory.recover().await.unwrap_err();
    assert!(
        factory
            .contexts
            .read(old_key.clone())
            .await
            .unwrap()
            .is_some()
    );
    client.finish("thread/start");
    factory.recover().await.unwrap();
    assert!(factory.contexts.read(old_key).await.unwrap().is_none());
    assert_eq!(client.count("thread/start"), 1);
    assert_eq!(client.count("thread/unsubscribe"), 1);
}
#[tokio::test]
async fn outcome_unknown_absence_and_changed_owner_do_not_authorize_unloading() {
    for mismatch in [
        "unknown",
        "absent",
        "caller",
        "params",
        "epoch",
        "persistent",
        "truthy",
    ] {
        let (_root, client, factory, session) = fixture(Mode::UnknownStart);
        tokio::time::timeout(Duration::from_secs(2), call(&session))
            .await
            .unwrap()
            .unwrap_err();
        let id = client.original("thread/start");
        if mismatch != "unknown" {
            client.finish("thread/start");
        }
        {
            let mut operations = client.operations.lock().unwrap();
            match mismatch {
                "absent" => {
                    operations.remove(&id);
                }
                "caller" => {
                    operations
                        .get_mut(&id)
                        .unwrap()
                        .insert("callerPrincipalId", "someone-else".into());
                }
                "params" => {
                    operations.get_mut(&id).unwrap().insert("params", object());
                }
                "epoch" => {
                    *client.epoch.lock().unwrap() = Some("another-epoch".into());
                }
                "truthy" => {
                    operations.get_mut(&id).unwrap().insert(
                        "reply",
                        json(r#"{"result":{"thread":{"id":"foreign-thread","ephemeral":"true"}}}"#),
                    );
                }
                "persistent" => {
                    operations.get_mut(&id).unwrap().insert(
                        "reply",
                        json(r#"{"result":{"thread":{"id":"foreign-thread","ephemeral":false}}}"#),
                    );
                }
                _ => (),
            }
        }
        session.close().await.unwrap();
        factory.recover().await.unwrap_err();
        assert!(retained(&factory, &client).await.is_some(), "{mismatch}");
        assert_eq!(client.count("thread/unsubscribe"), 0, "{mismatch}");
        assert_eq!(client.count("thread/start"), 1, "{mismatch}");
    }
}
#[tokio::test]
async fn a_confirmed_failed_start_releases_only_its_local_slot() {
    let (_root, client, factory, session) = fixture(Mode::UnknownStart);
    call(&session).await.unwrap_err();
    let id = client.original("thread/start");
    {
        let mut operations = client.operations.lock().unwrap();
        let operation = operations.get_mut(&id).unwrap();
        operation.insert("phase", "failed".into());
        operation.insert("code", "native_error".into());
        operation.insert(
            "reply",
            json(r#"{"error":{"code":-32602,"message":"Start rejected."}}"#),
        );
    }
    session.close().await.unwrap();
    assert!(retained(&factory, &client).await.is_none());
    assert_eq!(client.count("thread/unsubscribe"), 0);
    assert_eq!(client.count("thread/start"), 1);
    assert_eq!(
        client
            .operations
            .lock()
            .unwrap()
            .get(&id)
            .unwrap()
            .get("phase"),
        Some(&"failed".into())
    );
}
#[tokio::test]
async fn unresolved_tool_work_keeps_its_context_and_original_call() {
    let (_root, client, factory, session) = fixture(Mode::HoldTool);
    let pending = session.clone();
    let task = tokio::spawn(async move { call(&pending).await });
    entered(&client).await;
    session.close().await.unwrap();
    task.await.unwrap().unwrap_err();
    let before = retained(&factory, &client).await.unwrap();
    assert_eq!(
        before.pending.unwrap().operation_id,
        client.original("mcpServer/tool/call").into()
    );
    factory.recover().await.unwrap_err();
    assert_eq!(client.count("thread/unsubscribe"), 0);
    client.finish("mcpServer/tool/call");
    factory.recover().await.unwrap();
    assert_eq!(client.count("mcpServer/tool/call"), 1);
    assert_eq!(client.count("thread/unsubscribe"), 1);
    assert!(retained(&factory, &client).await.is_none());
}
#[tokio::test]
async fn epoch_recovery_requires_actual_absence_and_preserves_uncertain_reads() {
    for scenario in [
        "stopped_read",
        "absent_read",
        "same_epoch_read",
        "write",
        "live",
        "partial",
        "missing",
        "wrong_tool",
        "wrong_server",
    ] {
        let (_root, client, factory, session) = fixture(Mode::HoldTool);
        let task = saved_task(&factory, "source", 60).await;
        let session = session.for_run(CancellationToken::new(), "source".into());
        let pending = session.clone();
        let work = tokio::spawn(async move { call(&pending).await });
        entered(&client).await;
        session.close().await.unwrap();
        work.await.unwrap().unwrap_err();
        let call_id = client.original("mcpServer/tool/call");
        client
            .operations
            .lock()
            .unwrap()
            .get_mut(&call_id)
            .unwrap()
            .insert("phase", "outcome_unknown".into());
        client
            .operations
            .lock()
            .unwrap()
            .get_mut(&call_id)
            .unwrap()
            .insert("code", "native_deadline_exceeded".into());
        let original = client
            .operations
            .lock()
            .unwrap()
            .get(&call_id)
            .unwrap()
            .clone();
        let mut task = task;
        if scenario != "write" {
            task.get_mut("mcp").unwrap().insert("readOnly", true.into());
        }
        if scenario == "wrong_tool" {
            task.get_mut("mcp")
                .unwrap()
                .insert("tools", IvyJson::Array(vec!["other".into()]));
        }
        if scenario == "wrong_server" {
            task.get_mut("mcp")
                .unwrap()
                .insert("server", "other".into());
        }
        let saved = task.clone();
        factory
            .contexts
            .store
            .call(move |store| {
                store.save(&IvyJson::object([
                    ("task", saved),
                    ("revision", 2.0.into()),
                    ("deleted", false.into()),
                ]))
            })
            .await
            .unwrap();
        if scenario != "same_epoch_read" {
            *client.epoch.lock().unwrap() = Some("new-epoch".into());
        }
        if scenario == "stopped_read" {
            client.stopped_epochs.lock().unwrap().push(json(
                r#"{"epoch":"epoch","observedAt":"2026-01-02T03:04:05.006Z"}"#,
            ));
        }
        if scenario == "absent_read"
            || scenario == "missing"
            || scenario == "wrong_tool"
            || scenario == "wrong_server"
        {
            *client.loaded.lock().unwrap() = Some(json(r#"{"data":[],"nextCursor":null}"#));
        }
        if scenario == "partial" {
            *client.loaded.lock().unwrap() =
                Some(json(r#"{"data":[],"nextCursor":"another-page"}"#));
        }
        if scenario == "missing" {
            client.operations.lock().unwrap().remove(&call_id);
        }
        let recovered = factory.recover().await;
        let allowed = matches!(scenario, "stopped_read" | "absent_read" | "same_epoch_read");
        assert_eq!(recovered.is_ok(), allowed, "{scenario}: {recovered:?}");
        assert_eq!(
            factory
                .contexts
                .task_pending("source".into())
                .await
                .unwrap(),
            !allowed,
            "{scenario}"
        );
        assert_eq!(
            client.count("thread/unsubscribe"),
            usize::from(scenario == "same_epoch_read"),
            "{scenario}"
        );
        assert_eq!(client.count("mcpServer/tool/call"), 1, "{scenario}");
        let context = retained(&factory, &client).await.unwrap();
        assert_eq!(
            context.pending.unwrap().operation_id,
            JsString::from(call_id.clone())
        );
        if scenario != "missing" {
            assert_eq!(
                client.operations.lock().unwrap().get(&call_id),
                Some(&original)
            );
        }
        if allowed {
            factory.recover().await.unwrap();
            // A new accepted read gets its own context; the old receipt remains.
            *client.mode.lock().unwrap() = Mode::Normal;
            let next = factory.open(task, CancellationToken::new()).await.unwrap();
            assert!(bridge_call(next.as_ref()).await.contains("200"));
            next.close().await.unwrap();
        }
    }
}
#[tokio::test]
async fn an_empty_context_can_retire_after_a_fresh_complete_absence_read() {
    let (_root, client, factory, session) = fixture(Mode::Normal);
    call(&session).await.unwrap();
    *client.epoch.lock().unwrap() = Some("new-epoch".into());
    *client.loaded.lock().unwrap() = Some(json(r#"{"data":[],"nextCursor":null}"#));
    session.close().await.unwrap();
    assert!(retained(&factory, &client).await.is_none());
    assert_eq!(client.count("thread/unsubscribe"), 0);
}
#[tokio::test]
async fn a_lost_unsubscribe_is_reconciled_without_another_send() {
    let (_root, client, factory, session) = fixture(Mode::LostUnsubscribe);
    call(&session).await.unwrap();
    let mut stale = retained(&factory, &client).await.unwrap();
    session.close().await.unwrap();
    // A canceled store waiter may leave this old write queued on the blocking
    // owner. It must not erase closing or the newly retained unsubscribe.
    factory.contexts.save(&mut stale).await.unwrap_err();
    let original = retained(&factory, &client)
        .await
        .unwrap()
        .unsubscribe
        .unwrap()
        .operation_id;
    factory.recover().await.unwrap_err();
    session.close().await.unwrap();
    assert_eq!(client.count("thread/unsubscribe"), 1);
    assert_eq!(
        retained(&factory, &client)
            .await
            .unwrap()
            .unsubscribe
            .unwrap()
            .operation_id,
        original
    );
    client.finish("thread/unsubscribe");
    factory.recover().await.unwrap();
    assert_eq!(client.count("thread/unsubscribe"), 1);
    assert!(retained(&factory, &client).await.is_none());
}
#[tokio::test]
async fn a_dropped_bridge_close_and_network_timeout_keep_the_cleanup_receipt() {
    let (_root, client, factory, session) = fixture(Mode::HoldUnsubscribe);
    call(&session).await.unwrap();
    let bridge = LocalBridge::open(Arc::new(session), CancellationToken::new())
        .await
        .unwrap();
    let close = tokio::spawn(async move { bridge.close().await });
    entered(&client).await;
    close.abort();
    tokio::time::sleep(Duration::from_millis(5100)).await;
    let context = retained(&factory, &client).await.unwrap();
    assert!(context.closing && context.unsubscribe.is_some());
    client.finish("thread/unsubscribe");
    factory.recover().await.unwrap();
    assert_eq!(client.count("thread/unsubscribe"), 1);
    assert!(retained(&factory, &client).await.is_none());
}
#[tokio::test]
async fn pending_context_limit_prevents_dispatch_and_recovery_rotates_past_unknowns() {
    let (_root, client, factory, session) = fixture(Mode::UnknownStart);
    call(&session).await.unwrap_err();
    session.close().await.unwrap();
    let original = retained(&factory, &client).await.unwrap();
    for index in 1..64 {
        let mut context = Context {
            revision: 0,
            session_id: "previous-process".into(),
            caller: original.caller.clone(),
            owner: original.owner.clone(),
            epoch: original.epoch.clone(),
            start: original.start.clone(),
            thread: None,
            pending: None,
            unsubscribe: None,
            closing: true,
            task_id: None,
        };
        context.start.operation_id = format!("another-{index:02}").into();
        factory
            .contexts
            .insert(&context, &CancellationToken::new())
            .await
            .unwrap();
    }
    let mut context = original;
    context.start.operation_id = "over-limit".into();
    factory
        .contexts
        .insert(&context, &CancellationToken::new())
        .await
        .unwrap_err();
    let first = factory.contexts.next(String::new()).await.unwrap().unwrap();
    let second = factory.contexts.next(first.clone()).await.unwrap().unwrap();
    assert_ne!(first, second);
    factory.recover().await.unwrap_err();
    assert_eq!(*factory.contexts.cleanup.lock().await, first);
    factory.recover().await.unwrap_err();
    assert_eq!(*factory.contexts.cleanup.lock().await, second);
    assert_eq!(client.count("thread/start"), 1);
    assert_eq!(client.count("thread/unsubscribe"), 0);
    context.start.operation_id = "closed-before-insert".into();
    let closed = CancellationToken::new();
    closed.cancel();
    factory
        .contexts
        .insert(&context, &closed)
        .await
        .unwrap_err();
    assert!(
        factory
            .contexts
            .read(context.key().unwrap())
            .await
            .unwrap()
            .is_none()
    );
}
