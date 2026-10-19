use ivy_agent_manager::{
    agent_health::AgentHealth,
    browser_notices::{input_browser_notice, native_browser_notice, service_owns_completion},
    capabilities::{CapabilityManagement, SqliteCapabilityStore},
    interactions::{Limits as InteractionLimits, NativeInteractions},
    inventory::NativeInventory,
    inventory_manager::InventoryManager,
    local_state::{Limits as StateLimits, LocalAgentState},
    native_journal::{Limits, NativeJournal, Owner},
    native_launch::NativeHomeInputs,
    native_management::{NativeManagement, NativeManagementOwner},
    native_operations::{NativeOperations, SqliteOperationStore},
    notice_policy::NativeNoticePolicy,
    notifications::NativeNotifications,
    project_locations::ProjectLocations,
    project_management::ProjectManagement,
    project_policy::ProjectPolicy,
    project_repository::WorkspaceGit,
    reconciliation::{AgentReconciliation, ReconciliationOwner, ReconciliationSettings},
    service_events::{ServiceEventSettings, ServiceEvents},
    service_kernel::{
        AgentHandlers, AgentRuntimeBindings, agent_service_options, service_state_observer,
        start_prepared_agent,
    },
    service_link::ServiceLink,
    service_work::AgentWork,
    staged_files::StagedFiles,
    status::{AgentStatus, StatusConfig, StatusState},
};
use ivy_contracts::{
    IvyError, IvyJson, JsString,
    hash::{digest, hash_json},
    json::{canonical, parse},
};
use ivy_host_runtime::{
    codex_rpc::{Events, NativeNotification, NativeRequest, NativeRpc},
    config::InstanceConfig,
    health::HealthFile,
    node_path,
};
use ivy_sdk::{
    rpc::{RequestFuture, RequestOptions, RpcClient},
    service::{ConnectionOptions, InvocationContext, ServiceConnection, ServiceState},
};
use std::{
    collections::BTreeMap,
    fs,
    path::PathBuf,
    process::Command,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::AtomicBool,
    },
    time::Duration,
};
use tokio::io::{AsyncBufReadExt, BufReader};
const BYTES: usize = 32 * 1024 * 1024;
fn repository() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..")
}
fn data() -> &'static IvyJson {
    static DATA: OnceLock<IvyJson> = OnceLock::new();
    DATA.get_or_init(|| {
        let output = Command::new("node")
            .arg("tests/fixtures/agent-service-kernel-oracle.mjs")
            .current_dir(repository())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        parse(std::str::from_utf8(&output.stdout).unwrap(), BYTES).unwrap()
    })
}
fn field<'a>(value: &'a IvyJson, key: &str) -> &'a IvyJson {
    value.get(key).unwrap()
}
fn text(value: &IvyJson, key: &str) -> JsString {
    field(value, key).as_js_string().unwrap().clone()
}
fn same(actual: &IvyJson, expected: &IvyJson) {
    assert_eq!(
        canonical(actual, BYTES).unwrap(),
        canonical(expected, BYTES).unwrap()
    );
}
struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("ivy-service-kernel-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn catalog(path: &str) -> IvyJson {
    parse(&fs::read_to_string(repository().join(path)).unwrap(), BYTES).unwrap()
}
fn config(root: &Root, version: IvyJson) -> InstanceConfig {
    let mut config = field(data(), "config").clone();
    for (key, value) in [
        ("schemaVersion", IvyJson::Number(1.0)),
        ("componentId", "agent-manager".into()),
        ("instanceId", "fixture-agent".into()),
        ("dataRoot", IvyJson::String(node_path::from_native(&root.0))),
        (
            "artifactRoot",
            IvyJson::String(node_path::from_native(&root.0.join("artifact"))),
        ),
    ] {
        config.insert(key, value);
    }
    config.insert(
        "settings",
        IvyJson::object([
            (
                "nativeExecutable",
                IvyJson::String(node_path::from_native(&root.0.join("missing-native.exe"))),
            ),
            ("nativeVersion", version),
            ("nativeExecutableHash", digest("fixture").into()),
            ("patcherEnabled", false.into()),
            (
                "limits",
                IvyJson::object([
                    ("maxOperations", IvyJson::Number(100.0)),
                    ("maxJournalBytes", IvyJson::Number(64.0 * 1024.0 * 1024.0)),
                    ("maxPendingInputs", IvyJson::Number(8.0)),
                    ("maxNotificationBytes", IvyJson::Number(1024.0 * 1024.0)),
                ]),
            ),
        ]),
    );
    InstanceConfig::from_json(config).unwrap()
}
fn journal(version: JsString) -> Arc<Mutex<NativeJournal>> {
    Arc::new(Mutex::new(
        NativeJournal::with_clock(
            Owner {
                service_node_id: "fixture.agent".into(),
                native_version: version,
                native_executable_hash: digest("fixture").into(),
            },
            Limits {
                max_operations: 100,
                max_journal_bytes: 64 * 1024 * 1024,
                max_pending_inputs: 8,
            },
            Arc::new(|| "2026-10-19T00:00:00.000Z".into()),
        )
        .unwrap(),
    ))
}
fn local(root: &Root) -> Arc<Mutex<LocalAgentState>> {
    Arc::new(Mutex::new(
        LocalAgentState::open(
            &root.0.join("journal/agent-runtime.sqlite"),
            "fixture.agent".into(),
            StateLimits {
                max_operations: 100,
                max_journal_bytes: 64 * 1024 * 1024,
            },
        )
        .unwrap(),
    ))
}
fn native(
    catalog: &IvyJson,
    events: Events,
) -> (NativeRpc, tokio::io::DuplexStream, tokio::io::DuplexStream) {
    let (writer, read) = tokio::io::duplex(1024 * 1024);
    let (write, reader) = tokio::io::duplex(1024 * 1024);
    (
        NativeRpc::start(catalog, writer, reader, events).unwrap(),
        read,
        write,
    )
}
#[test]
fn notice_content_and_ordered_query_fields_match_the_original_source() {
    let rows = field(data(), "notices").as_array().unwrap();
    assert_eq!(rows.len(), 84);
    for row in rows {
        let node = text(row, "node");
        let thread = text(row, "thread");
        let query = text(row, "query");
        let serialize = move |pairs: &[(JsString, JsString)]| {
            assert_eq!(
                pairs,
                &[("node".into(), node.clone()), ("id".into(), thread.clone())]
            );
            Ok(query.clone())
        };
        let actual = if field(row, "kind") == &"native".into() {
            native_browser_notice(&text(row, "node"), field(row, "input"), &serialize)
        } else {
            input_browser_notice(field(row, "input"), &serialize)
        }
        .unwrap()
        .unwrap_or(IvyJson::Null);
        same(&actual, field(row, "expected"));
    }
}
struct Pages {
    pages: Vec<IvyJson>,
    calls: Mutex<Vec<IvyJson>>,
}
impl RpcClient for Pages {
    fn request<'a>(
        &'a self,
        method: &'a str,
        params: IvyJson,
        _: RequestOptions,
    ) -> RequestFuture<'a> {
        Box::pin(async move {
            let mut calls = self.calls.lock().unwrap();
            let index = calls.len();
            calls.push(IvyJson::object([
                ("method", method.into()),
                ("params", params),
            ]));
            Ok(self.pages[index].clone())
        })
    }
}
#[tokio::test]
async fn workflow_completion_ownership_preserves_pagination_and_does_not_classify_agents_as_workflows()
 {
    for row in field(data(), "serviceOwnership").as_array().unwrap() {
        let client = Pages {
            pages: field(row, "pages").as_array().unwrap().to_vec(),
            calls: Mutex::new(vec![]),
        };
        let caller = field(row, "caller").as_js_string();
        let actual = service_owns_completion(&client, caller).await.unwrap();
        same(&IvyJson::Bool(actual), field(row, "expected"));
        same(
            &IvyJson::Array(client.calls.into_inner().unwrap()),
            field(row, "calls"),
        );
    }
}
#[tokio::test]
async fn all_seven_native_catalogs_compose_every_management_handler_and_original_service_identity()
{
    for path in field(data(), "catalogs").as_array().unwrap() {
        let root = Root::new();
        let catalog = catalog(&path.as_str().unwrap());
        let config = config(&root, field(&catalog, "version").clone());
        let journal = journal(text(&catalog, "version"));
        journal
            .lock()
            .unwrap()
            .begin_epoch(&"epoch".into())
            .unwrap();
        let state = local(&root);
        let operations = NativeOperations::new(
            Arc::new(SqliteOperationStore::new(state.clone()).unwrap()),
            journal.clone(),
        );
        let (rpc, _read, _write) = native(&catalog, Events::default());
        let closing = Arc::new(AtomicBool::new(false));
        let health = AgentHealth::new(
            Arc::new(HealthFile::with_launch_id(config.clone(), None)),
            journal.clone(),
        );
        let locations = ProjectLocations::new(
            ProjectPolicy::new(
                node_path::from_native(&root.0),
                field(config.json(), "settings"),
                &[],
                node_path::Context::current().unwrap(),
                &node_path::from_native(&root.0),
            )
            .unwrap(),
            None,
            Some(state.clone()),
            Arc::new(WorkspaceGit::new(root.0.clone(), None)),
        );
        let inventory = InventoryManager::new(
            Arc::new(NativeInventory::new(catalog.clone(), "epoch".into(), rpc.clone()).unwrap()),
            journal.clone(),
            Arc::new(Mutex::new(NativeNoticePolicy::default())),
            Arc::new(|_| Ok(())),
            closing.clone(),
        )
        .unwrap();
        let capabilities = CapabilityManagement::open(
            Arc::new(SqliteCapabilityStore(state.clone())),
            "fixture.agent".into(),
            "fixture".into(),
            None,
            IvyJson::object([("fixture", true.into())]),
            Arc::new(|_| Ok(())),
        )
        .await
        .unwrap();
        let status = Arc::new(AgentStatus::new(
            StatusConfig {
                service_node_id: "fixture.agent".into(),
                host_id: "fixture".into(),
                settings: field(config.json(), "settings").clone(),
                catalog_hash: hash_json(&catalog).unwrap().into(),
                process_stops: IvyJson::Array(vec![]),
                windows: cfg!(windows),
            },
            journal.clone(),
            Arc::new(|| {
                Ok(StatusState {
                    owner: IvyJson::object([("epoch", "epoch".into())]),
                    closing: false,
                    connected: true,
                    ready: false,
                    inventory_code: None,
                    capabilities: IvyJson::Null,
                    instructions: None,
                    mcp: None,
                    skills: None,
                })
            }),
        ));
        let projects = Arc::new(ProjectManagement {
            locations: locations.clone(),
            host_id: "fixture".into(),
            service_node_id: "fixture.agent".into(),
            available: Arc::new(|| Err(IvyError::new("fixture_unavailable", ""))),
            fresh_projects: Arc::new(|| panic!("Unavailable projects must not refresh")),
            compare_directories: Arc::new(|_, _| panic!("No locale comparator in this fixture")),
        });
        let native_management = NativeManagement::new(
            NativeManagementOwner {
                service_node_id: "fixture.agent".into(),
                epoch: "epoch".into(),
                catalog: catalog.clone(),
                rpc: rpc.clone(),
                closing: closing.clone(),
                platform: "win32".into(),
            },
            operations,
            NativeInteractions::new("epoch".into(), InteractionLimits::default()),
        )
        .unwrap();
        let registry = native_management.registry().clone();
        let work = AgentWork::default();
        let set = AgentHandlers {
            native: native_management,
            capabilities,
            projects,
            status,
            notifications: Arc::new(Mutex::new(NativeNotifications::new_js(
                "fixture.agent".into(),
                text(&catalog, "version"),
                0,
                9007199254740991,
                1024,
            ))),
            journal: journal.clone(),
            inventory: inventory.clone(),
            locations,
            staged: Arc::new(StagedFiles::new(
                node_path::from_native(&root.0.join("staged")),
                Arc::new(|_| panic!("No production codec in this fixture")),
            )),
        }
        .compose(&work);
        let mut names = set
            .management
            .keys()
            .cloned()
            .map(IvyJson::String)
            .collect::<Vec<_>>();
        names.sort_by_key(|v| v.as_js_string().unwrap().clone());
        same(&IvyJson::Array(names), field(data(), "handlers"));
        let mut expected_native = field(&catalog, "clientRequests")
            .as_array()
            .unwrap()
            .iter()
            .map(|v| {
                let mut n = JsString::from("codex.");
                n.0.extend(text(v, "method").0);
                n
            })
            .collect::<Vec<_>>();
        expected_native.sort();
        expected_native.dedup();
        let mut actual_native = set.native.keys().cloned().collect::<Vec<_>>();
        actual_native.sort();
        assert_eq!(actual_native, expected_native);
        let reconciliation = AgentReconciliation::new(ReconciliationSettings {
            service_node_id: "fixture.agent".into(),
            registry: registry.clone(),
            owner: ReconciliationOwner {
                rpc: rpc.clone(),
                epoch: "epoch".into(),
                closing: closing.clone(),
            },
            journal,
            state: state.clone(),
            inventory,
            environment: None,
            environment_at: 0.0,
            health: health.clone(),
        })
        .unwrap();
        let options = agent_service_options(
            &config,
            registry.clone(),
            set,
            &reconciliation,
            health,
            closing,
        )
        .unwrap();
        let mut identity = field(data(), "identity").clone();
        identity.insert("nativeVersion", field(&catalog, "version").clone());
        same(&options.identity, &identity);
        assert_eq!(
            options.heartbeat.as_millis(),
            field(data(), "heartbeatMs").as_f64().unwrap() as u128
        );
        same(&(options.registry)().await.unwrap(), &registry);
        assert_eq!((options.credential)().await.unwrap(), "fixture-only");
        // Exercise the real Shared provider registry gate over a local socket.
        // This fixture has no Hive/provider account and launches no process.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let peer = tokio::spawn(async move {
            use futures_util::StreamExt;
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            while let Some(message) = socket.next().await { if message.is_err() { break; } }
        });
        let mut connection_options = ConnectionOptions::new("fixture.agent".into());
        connection_options.handlers = options.handlers.clone();
        connection_options.json_handlers = options.json_handlers.clone();
        let connection = ServiceConnection::connect(&base, "fixture-only", connection_options, Default::default()).await.unwrap();
        connection.prepare_registry(&registry).unwrap();
        connection.close();
        tokio::time::timeout(Duration::from_secs(5), peer).await.unwrap().unwrap();
        let context = InvocationContext::detached(
            "caller".into(),
            None,
            3,
            Default::default(),
            IvyError::internal(),
        );
        same(
            &options.handlers[&"agent.catalog".into()](IvyJson::Null, context.clone())
                .await
                .unwrap(),
            &catalog,
        );
        assert_eq!(
            options.handlers[&"agent.resolveProject".into()](
                IvyJson::object([(
                    "selection",
                    IvyJson::object([("kind", "projectless".into())])
                )]),
                context
            )
            .await
            .unwrap_err()
            .code,
            "fixture_unavailable"
        );
        work.drain_snapshot().await.unwrap();
        rpc.close("fixture_complete").await;
        state.lock().unwrap().close_shared().unwrap();
    }
}
#[tokio::test]
async fn service_state_health_matches_original_reconciliation_messages_and_does_not_close_native() {
    for row in field(data(), "states").as_array().unwrap() {
        let root = Root::new();
        let config = config(&root, "0.154.0".into());
        let journal = journal("0.154.0".into());
        let health = AgentHealth::new(Arc::new(HealthFile::with_launch_id(config, None)), journal);
        let closing = Arc::new(AtomicBool::new(
            field(row, "closing") == &IvyJson::Bool(true),
        ));
        let observer = service_state_observer(health.clone(), closing);
        let status = text(row, "status").to_utf8().unwrap();
        observer(ServiceState {
            status: match status.as_str() {
                "connecting" => "connecting",
                "offline" => "offline",
                "stopped" => "stopped",
                _ => "ready",
            },
            generation: Some(7),
            code: field(row, "code").as_js_string().cloned(),
            message: None,
            phase: Some("fixture"),
        });
        health.drain_snapshot().await.unwrap();
        let expected = field(row, "health").as_array().unwrap();
        if expected.is_empty() {
            assert!(!root.0.join("health.json").exists());
        } else {
            let stored = parse(
                &fs::read_to_string(root.0.join("health.json")).unwrap(),
                BYTES,
            )
            .unwrap();
            let args = expected[0].as_array().unwrap();
            same(field(&stored, "ready"), &args[0]);
            same(field(&stored, "generation"), &args[1]);
            same(field(&stored, "details"), &args[2]);
        }
    }
}
#[tokio::test]
async fn launch_time_clock_is_answered_after_attach_and_offline_notifications_remain_recoverable() {
    let root = Root::new();
    let catalog = catalog("specs/native/codex-0.154.0/catalog.json");
    let journal = journal(text(&catalog, "version"));
    let state = local(&root);
    let operations = NativeOperations::new(
        Arc::new(SqliteOperationStore::new(state.clone()).unwrap()),
        journal.clone(),
    );
    let notifications = Arc::new(Mutex::new(NativeNotifications::new_js(
        "fixture.agent".into(),
        text(&catalog, "version"),
        0,
        9007199254740991,
        4096,
    )));
    let policy = Arc::new(Mutex::new(NativeNoticePolicy::default()));
    let events = ServiceEvents::with_clock(
        ServiceEventSettings {
            service_node_id: "fixture.agent".into(),
            service: ServiceLink::default(),
            closing: Arc::new(AtomicBool::new(false)),
            operations,
            notifications: notifications.clone(),
            policy: policy.clone(),
            query: Arc::new(|_| panic!("Offline observations cannot construct notice URLs")),
            work: AgentWork::default(),
        },
        Arc::new(|| "2026-10-19T00:00:00.000Z".into()),
    );
    let callbacks = events.begin_epoch("epoch".into()).unwrap();
    (callbacks.on_request)(NativeRequest {
        id: "clock".into(),
        method: "currentTime/read".into(),
        params: IvyJson::object([("threadId", "thread".into()), ("turnId", "turn".into())]),
    })
    .await
    .unwrap();
    let (rpc, read, _write) = native(&catalog, callbacks.clone());
    events.attach_native(rpc.clone()).unwrap();
    let mut lines = BufReader::new(read).lines();
    let line = tokio::time::timeout(Duration::from_secs(5), lines.next_line())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let reply = parse(&line, BYTES).unwrap();
    same(field(&reply, "id"), &"clock".into());
    assert!(
        field(field(&reply, "result"), "currentTimeAt")
            .as_f64()
            .unwrap()
            > 0.0
    );
    events.attach_native(rpc.clone()).unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(30), lines.next_line())
            .await
            .is_err()
    );
    (callbacks.on_notification)(NativeNotification {
        method: "thread/started".into(),
        params: IvyJson::object([(
            "thread",
            IvyJson::object([("id", "thread".into()), ("cwd", "/external".into())]),
        )]),
    })
    .await
    .unwrap();
    assert_eq!(
        policy
            .lock()
            .unwrap()
            .is_internal(&"thread".into())
            .unwrap(),
        Some(false)
    );
    rpc.close("fixture_closed").await;
    let page = notifications
        .lock()
        .unwrap()
        .page_json(
            &IvyJson::object([("afterSequence", IvyJson::Number(0.0))]),
            None,
        )
        .unwrap();
    assert_eq!(field(&page, "items").as_array().unwrap().len(), 1);
    assert_eq!(journal.lock().unwrap().epoch(), None);
    state.lock().unwrap().close_shared().unwrap();
}
struct NoBootstrap;
impl RpcClient for NoBootstrap {
    fn request<'a>(&'a self, _: &'a str, _: IvyJson, _: RequestOptions) -> RequestFuture<'a> {
        panic!(
            "Disabled patcher must not read bootstrap overrides before the missing executable is refused"
        )
    }
}
fn bindings(root: &Root) -> AgentRuntimeBindings {
    AgentRuntimeBindings {
        bootstrap: Arc::new(NoBootstrap),
        home: NativeHomeInputs {
            paths: node_path::Context::current().unwrap(),
            environment: BTreeMap::new(),
            user_home: None,
            cwd: node_path::from_native(&root.0),
            account_home: node_path::from_native(&root.0),
        },
        english_hint_comparator: Arc::new(|_, _| panic!("No locale binding in this fixture")),
        skills_comparator: Arc::new(|_, _| panic!("No locale binding in this fixture")),
        directory_comparator: Arc::new(|_, _| panic!("No locale binding in this fixture")),
        canonical_base64: Arc::new(|_| panic!("No codec binding in this fixture")),
        form_query: Arc::new(|_| panic!("No form serializer in this fixture")),
    }
}
#[tokio::test]
async fn actual_prepared_start_refuses_before_any_cli_probe_and_closes_its_resources() {
    let root = Root::new();
    let error = start_prepared_agent(config(&root, "0.154.0".into()), bindings(&root))
        .await
        .err()
        .unwrap();
    assert_ne!(error.code, "");
    let health = parse(
        &fs::read_to_string(root.0.join("health.json")).unwrap(),
        BYTES,
    )
    .unwrap();
    same(field(&health, "ready"), &false.into());
    same(
        field(&health, "details"),
        &"AgentManager stopped: agent_start_failed.".into(),
    );
    let state = local(&root);
    assert!(
        state
            .lock()
            .unwrap()
            .read("capabilities", "profile")
            .unwrap()
            .is_some()
    );
    state.lock().unwrap().close_shared().unwrap();
}
#[tokio::test]
async fn dropping_the_prepared_start_waiter_preserves_admitted_cleanup() {
    let root = Root::new();
    drop(start_prepared_agent(
        config(&root, "0.154.0".into()),
        bindings(&root),
    ));
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(bytes) = fs::read_to_string(root.0.join("health.json")) {
                if let Ok(v) = parse(&bytes, BYTES) {
                    if v.get("details") == Some(&"AgentManager stopped: agent_start_failed.".into())
                    {
                        break;
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}
