//! Agent service composition after instance validation and runtime reset.
use crate::{
    agent_health::AgentHealth,
    backend_readiness::BackendReadiness,
    backend_routes::{BackendFailure, BackendRoute, BackendRoutes},
    browser_notices::{BrowserNoticeSettings, BrowserNotices, FormQuery},
    capabilities::{CapabilityManagement, SqliteCapabilityStore, initialize_profile},
    directories::DirectoryComparator,
    environment_defaults::load_environment_defaults,
    environment_manager::{EnvironmentManagerSettings, ManagedEnvironment},
    instructions_source::EnglishHintComparator,
    interactions::{Limits as InteractionLimits, NativeInteractions},
    inventory::NativeInventory,
    inventory_manager::{InventoryConnection, InventoryManager},
    local_state::{Limits as StateLimits, LocalAgentState},
    native_journal::{Limits as JournalLimits, NativeJournal, Owner},
    native_launch::{
        NativeHomeInputs, NativeProcessOptions, load_native_catalog,
        managed_environment_before_launch, start_validated_native_process,
    },
    native_management::{NativeManagement, NativeManagementOwner},
    native_operations::{NativeOperations, SqliteOperationStore},
    notice_policy::NativeNoticePolicy,
    notifications::{NativeNotifications, notification_handler},
    process_evidence::OwnedProcessEvidence,
    project_locations::ProjectLocations,
    project_management::ProjectManagement,
    project_policy::ProjectPolicy,
    project_repository::WorkspaceGit,
    reconciliation::{AgentReconciliation, ReconciliationOwner, ReconciliationSettings},
    service_events::{ServiceEventSettings, ServiceEvents},
    service_link::ServiceLink,
    service_work::AgentWork,
    shutdown::{AgentShutdown, ShutdownRun},
    skills_files::SkillsComparator,
    staged_files::{CanonicalBase64Decoder, StagedFiles},
    status::{AgentStatus, StatusConfig, StatusState},
};
use crate::{
    backend_runtime::{BackendResources, backend_handlers, get, put, slot},
    host_service::{HostResources, control_timer},
};
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString,
    hash::hash_json,
    json::stringify,
    native_backend::{BackendIdentity, NativeProvider},
};
use ivy_host_runtime::{config::InstanceConfig, health::HealthFile, node_path};
use ivy_sdk::{
    rpc::RpcClient,
    service::{ConnectionDependency, ServiceClient, ServiceOptions, StateObserver, ToolHandler},
};
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::sync::{oneshot, watch};

/// Runtime dependencies for embedded consumers and tests. The executable uses
/// the native host, codecs and shared Hive client from `runtime::native_bindings`.
#[derive(Clone)]
pub struct AgentRuntimeBindings {
    pub bootstrap: Arc<dyn RpcClient>,
    pub home: NativeHomeInputs,
    pub english_hint_comparator: Arc<EnglishHintComparator>,
    pub skills_comparator: Arc<SkillsComparator>,
    pub directory_comparator: Arc<DirectoryComparator>,
    pub canonical_base64: Arc<CanonicalBase64Decoder>,
    pub form_query: Arc<FormQuery>,
}

/// Every management and installed native handler is composed before registry
/// publication. The original tracked management calls and every native call
/// use the same work owner as shutdown; pure queries stay immediate.
pub struct AgentHandlers {
    pub native: NativeManagement,
    pub capabilities: CapabilityManagement,
    pub projects: Arc<ProjectManagement>,
    pub status: Arc<AgentStatus>,
    pub notifications: Arc<Mutex<NativeNotifications>>,
    pub journal: Arc<Mutex<NativeJournal>>,
    pub inventory: InventoryManager,
    pub locations: ProjectLocations,
    pub staged: Arc<StagedFiles>,
}
#[derive(Clone)]
pub struct HandlerSet {
    pub management: HashMap<JsString, ToolHandler>,
    pub native: HashMap<JsString, ToolHandler>,
}
impl AgentHandlers {
    pub fn compose(self, work: &AgentWork) -> HandlerSet {
        let mut handlers = self.native.handlers();
        handlers.extend(self.capabilities.handlers());
        handlers.extend(self.projects.handlers());
        handlers.insert("agent.status".into(), self.status.handler());
        handlers.insert(
            "agent.notifications".into(),
            notification_handler(self.notifications, self.journal),
        );
        handlers.insert(
            "agent.projects".into(),
            self.inventory
                .projects_handler(Arc::new(move || Ok(self.locations.defaults()))),
        );
        handlers.insert("agent.stageFile".into(), self.staged.handler());
        HandlerSet {
            management: handlers
                .into_iter()
                .map(|(name, handler)| {
                    let tracked = [
                        "agent.configureCapabilities",
                        "agent.resolveWorkspace",
                        "agent.listDirectories",
                        "agent.resolveProject",
                        "agent.stageFile",
                        "agent.interact",
                        "agent.read",
                    ]
                    .iter()
                    .any(|v| name == JsString::from(*v));
                    let handler = if name == JsString::from("agent.answer") {
                        work.reply_handler(handler)
                    } else if name == JsString::from("agent.prevent")
                        || name == JsString::from("agent.release")
                    {
                        work.control_handler(handler)
                    } else if name == JsString::from("agent.invoke") {
                        work.invoke_handler(handler)
                    } else if tracked {
                        work.handler(handler)
                    } else {
                        handler
                    };
                    (name, handler)
                })
                .collect(),
            native: self
                .native
                .json_handlers()
                .into_iter()
                .map(|(name, handler)| {
                    let handler = if name.eq_str("codex.turn/interrupt")
                        || name.eq_str("codex.thread/unsubscribe")
                    {
                        work.control_handler(handler)
                    } else {
                        work.handler(handler)
                    };
                    (name, handler)
                })
                .collect(),
        }
    }
}

pub(crate) fn text(value: &IvyJson, key: &str) -> IvyResult<JsString> {
    value
        .required(key)?
        .as_js_string()
        .cloned()
        .ok_or_else(IvyError::internal)
}
fn number(value: &IvyJson, key: &str) -> IvyResult<usize> {
    value
        .required(key)?
        .as_f64()
        .map(|v| v as usize)
        .ok_or_else(IvyError::internal)
}
pub(crate) fn now() -> JsString {
    chrono::Utc::now()
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
        .into()
}
fn emit(value: IvyJson) {
    if let Ok(line) = stringify(&value, usize::MAX) {
        eprintln!("{line}");
    }
}
pub fn service_state_observer(health: AgentHealth, closing: Arc<AtomicBool>) -> StateObserver {
    Arc::new(move |state| {
        if closing.load(Ordering::Acquire) {
            return;
        }
        if state.status == "offline" {
            let mut event = IvyJson::object([
                ("at", IvyJson::String(now())),
                ("event", "agent_hive_connection_failed".into()),
                (
                    "generation",
                    state
                        .generation
                        .map(|v| IvyJson::Number(v as f64))
                        .unwrap_or(IvyJson::Null),
                ),
            ]);
            if let Some(code) = &state.code {
                event.insert("code", IvyJson::String(code.clone()));
            }
            if let Some(phase) = state.phase {
                event.insert("phase", phase.into());
            }
            emit(event);
        }
        if ["offline", "stopped", "connecting"].contains(&state.status) {
            let mut message = JsString::from("Hive generation is not reconciled");
            if let Some(code) = state.code.filter(|v| !v.0.is_empty()) {
                message.0.extend(": ".encode_utf16());
                message.0.extend(code.0);
            }
            message.0.push(46);
            let run = health.write(false, state.generation, message);
            tokio::spawn(async move {
                let result = match run {
                    Ok(run) => run.wait().await,
                    Err(error) => Err(error),
                };
                if let Err(error) = result {
                    emit(IvyJson::object([
                        ("event", "agent_health_write_failed".into()),
                        ("code", IvyJson::String(error.code_js())),
                    ]));
                }
            });
        }
    })
}
pub fn agent_service_options(
    config: &InstanceConfig,
    registry: IvyJson,
    handlers: HandlerSet,
    reconciliation: &AgentReconciliation,
    health: AgentHealth,
    closing: Arc<AtomicBool>,
) -> IvyResult<ServiceOptions> {
    let (reconcile, readiness) = reconciliation.dependencies();
    service_options(
        config, registry, handlers, reconcile, readiness, health, closing,
    )
}
fn service_options(
    config: &InstanceConfig,
    registry: IvyJson,
    handlers: HandlerSet,
    reconcile: ConnectionDependency<()>,
    readiness: ConnectionDependency<IvyJson>,
    health: AgentHealth,
    closing: Arc<AtomicBool>,
) -> IvyResult<ServiceOptions> {
    let value = config.json();
    let credential = text(value, "credential")?.to_utf8()?;
    let mut identity = IvyJson::object([
        ("serviceNodeId", value.required("serviceNodeId")?.clone()),
        ("serviceName", "agent-manager".into()),
        ("hostId", value.required("hostId")?.clone()),
        ("version", value.required("version")?.clone()),
        ("buildId", value.required("buildId")?.clone()),
        ("hiveProtocol", IvyJson::Number(1.0)),
    ]);
    if let Some(version) = value.required("settings")?.get("nativeVersion") {
        identity.insert("nativeVersion", version.clone());
    }
    let mut options = ServiceOptions::new(
        text(value, "publicBaseUrl")?.to_utf8()?,
        Arc::new(move || {
            let credential = credential.clone();
            Box::pin(async move { Ok(credential) })
        }),
        identity,
        Arc::new(move || {
            let registry = registry.clone();
            Box::pin(async move { Ok(registry) })
        }),
        reconcile,
    );
    options.handlers = handlers.management;
    options.json_handlers = handlers.native;
    options.readiness = Some(readiness);
    options.heartbeat = Duration::from_secs(5);
    options.on_state = Some(service_state_observer(health, closing));
    Ok(options)
}

pub struct AgentRuntime {
    pub service: Arc<ServiceClient>,
    shutdown: Arc<AgentShutdown>,
}
impl AgentRuntime {
    pub fn close(&self) -> IvyResult<ShutdownRun> {
        self.shutdown.close("agent_stopped".into())
    }
    pub async fn closed(&self) -> IvyResult<JsString> {
        self.shutdown.closed().await
    }
}
impl Drop for AgentRuntime {
    fn drop(&mut self) {
        // Cleanup belongs to the host owner, including an abandoned runtime.
        let _ = self.shutdown.close("agent_stopped".into());
    }
}

/// Called only after the original configuration/runtime-reset entry has
/// completed. Explicit imports, when needed, run separately before startup.
/// Startup remains owned if its waiter is dropped; an abandoned successful
/// startup closes through the same resource owner instead of leaking a service.
pub fn start_prepared_agent(
    config: InstanceConfig,
    bindings: AgentRuntimeBindings,
) -> impl std::future::Future<Output = IvyResult<AgentRuntime>> + Send {
    owned_start(async move {
        let config = ValidatedAgentConfig::new(config)?;
        config.validate_homes(&bindings.home)?;
        start(config, bindings, false).await
    })
}
pub(crate) fn start_validated_agent(
    config: ValidatedAgentConfig,
    bindings: AgentRuntimeBindings,
) -> impl std::future::Future<Output = IvyResult<AgentRuntime>> + Send {
    owned_start(start(config, bindings, true))
}
fn owned_start(
    startup: impl std::future::Future<Output = IvyResult<AgentRuntime>> + Send + 'static,
) -> impl std::future::Future<Output = IvyResult<AgentRuntime>> + Send {
    let (sender, receiver) = oneshot::channel();
    tokio::spawn(async move {
        if let Err(Ok(runtime)) = sender.send(startup.await) {
            if let Ok(run) = runtime.close() {
                let _ = run.wait().await;
            }
        }
    });
    async move { receiver.await.map_err(|_| IvyError::internal())? }
}
pub(crate) use crate::agent_config::ValidatedAgentConfig;
#[cfg(test)]
#[path = "../../../../tests/rust/agent-host-shutdown.rs"]
mod host_shutdown_cases;
async fn prepare_backend(
    config: Arc<InstanceConfig>,
    backend: Option<BackendIdentity>,
    health: AgentHealth,
) -> IvyResult<Arc<BackendResources>> {
    let value = config.json();
    let settings = value.required("settings")?.clone();
    let service_node = text(value, "serviceNodeId")?;
    let host = text(value, "hostId")?;
    let limits = settings.required("limits")?;
    let journal = Arc::new(Mutex::new(NativeJournal::new_scoped(
        Owner {
            service_node_id: service_node.clone(),
            native_version: text(&settings, "nativeVersion")?,
            native_executable_hash: text(&settings, "nativeExecutableHash")?,
        },
        JournalLimits {
            max_operations: number(limits, "maxOperations")?,
            max_journal_bytes: number(limits, "maxJournalBytes")?,
            max_pending_inputs: number(limits, "maxPendingInputs")?,
        },
        backend.clone(),
    )?));
    let range = journal
        .lock()
        .map_err(|_| IvyError::internal())?
        .reserve_notification_sequence();
    let notifications = Arc::new(Mutex::new(
        NativeNotifications::new_js(
            service_node.clone(),
            text(&settings, "nativeVersion")?,
            number(&range, "start")? as u64,
            number(&range, "end")? as u64,
            number(limits, "maxNotificationBytes")?,
        )
        .with_backend(backend.clone()),
    ));
    let policy = Arc::new(Mutex::new(NativeNoticePolicy::default()));
    let filename = config.data_root().join("journal/agent-runtime.sqlite");
    let state_service = service_node.to_utf8()?;
    let state_limits = StateLimits {
        max_operations: number(limits, "maxOperations")? as i64,
        max_journal_bytes: number(limits, "maxJournalBytes")? as i64,
    };
    let state_backend = backend.clone();
    let state_provider = if settings
        .get("appServer")
        .and_then(|server| server.get("mode"))
        == Some(&"claude-adapter".into())
    {
        NativeProvider::Claude
    } else {
        NativeProvider::Codex
    };
    let state = Arc::new(Mutex::new(
        tokio::task::spawn_blocking(move || {
            let mut state = LocalAgentState::open(&filename, state_service, state_limits)?;
            state.bind_backend(state_provider, state_backend.as_ref())?;
            Ok::<_, IvyError>(state)
        })
        .await
        .map_err(|_| IvyError::internal())??,
    ));
    let capability_store = Arc::new(SqliteCapabilityStore(state.clone()));
    // This initialization was outside the original startup catch. Do not add
    // the separately gated bootstrap guard while composing the service.
    initialize_profile(
        capability_store.as_ref(),
        &service_node,
        &host,
        settings.get("capabilities").cloned(),
        &now,
    )
    .await?;
    let operations = NativeOperations::new(
        Arc::new(SqliteOperationStore::new(state.clone())?),
        journal.clone(),
    );
    health.register(journal.clone())?;
    let receipts = operations.reconcile_terminals(Arc::new(move |error| {
        emit(IvyJson::object([
            ("event", "agent_receipt_save_failed".into()),
            ("code", IvyJson::String(error.code_js())),
        ]));
    }));
    let closing = Arc::new(AtomicBool::new(false));
    Ok(Arc::new(BackendResources {
        config,
        backend,
        native: slot(),
        events: slot(),
        environment: slot(),
        inventory: slot(),
        locations: slot(),
        work: AgentWork::default(),
        state,
        receipts,
        operations,
        notifications,
        policy,
        journal,
        readiness: BackendReadiness::new(closing.clone()),
        closing,
        retiring: AtomicBool::new(false),
        interactions: slot(),
        handlers: slot(),
        startup: watch::channel(None).0,
        failure: Arc::new(Mutex::new(None)),
    }))
}

async fn launch_backend(
    resources: Arc<BackendResources>,
    bindings: AgentRuntimeBindings,
    health: AgentHealth,
    service_link: ServiceLink,
    catalog: Arc<IvyJson>,
) -> IvyResult<()> {
    resources.ensure_starting()?;
    let service_link = service_link.scoped(resources.backend.clone(), resources.readiness.clone());
    let config = resources.config.clone();
    let value = config.json();
    let settings = value.required("settings")?.clone();
    let backend = resources.backend.clone();
    let service_node = text(value, "serviceNodeId")?;
    let host = text(value, "hostId")?;
    let credential = Some(text(value, "credential")?);
    let state = resources.state.clone();
    let capability_store = Arc::new(SqliteCapabilityStore(state.clone()));
    let operations = resources.operations.clone();
    let notifications = resources.notifications.clone();
    let journal = resources.journal.clone();
    let policy = resources.policy.clone();
    let closing = resources.closing.clone();
    let work = resources.work.clone();
    let artifact_root = text(value, "artifactRoot")?;
    let data_root = text(value, "dataRoot")?;
    let base_url = text(value, "publicBaseUrl")?;
    let defaults = load_environment_defaults(&artifact_root, &base_url)?;
    let environment = ManagedEnvironment::new(EnvironmentManagerSettings {
        settings: settings.clone(),
        data_root: data_root.clone(),
        host_id: host.clone(),
        hive_url: base_url,
        credential: credential.clone().unwrap(),
        defaults: defaults.clone(),
        english_hint_comparator: bindings.english_hint_comparator.clone(),
        skills_comparator: bindings.skills_comparator.clone(),
    });
    put(&resources.environment, environment.clone())?;
    let events = ServiceEvents::new(ServiceEventSettings {
        service_node_id: service_node.clone(),
        service: service_link.clone(),
        closing: closing.clone(),
        operations: operations.clone(),
        state: state.clone(),
        receipts: resources.receipts.clone(),
        notifications: notifications.clone(),
        policy: policy.clone(),
        query: bindings.form_query.clone(),
    });
    put(&resources.events, events.clone())?;
    let paths = bindings.home.paths.clone();
    let account_home = bindings.home.account_home.clone();
    let begin = events.clone();
    let prepare_environment =
        managed_environment_before_launch(environment.clone(), bindings.bootstrap.clone());
    let starting = Arc::downgrade(&resources);
    let native = Arc::new(
        start_validated_native_process(
            NativeProcessOptions {
                artifact_root: artifact_root.clone(),
                data_root: data_root.clone(),
                settings: settings.clone(),
                client_version: text(value, "version")?,
                hive_credential: credential,
                home_inputs: bindings.home.clone(),
                before_launch: Some(Arc::new(move |home| {
                    let prepare = prepare_environment.clone();
                    let starting = starting.clone();
                    Box::pin(async move {
                        starting
                            .upgrade()
                            .ok_or_else(IvyError::internal)?
                            .ensure_starting()?;
                        prepare(home).await?;
                        starting
                            .upgrade()
                            .ok_or_else(IvyError::internal)?
                            .ensure_starting()
                    })
                })),
                begin_epoch: Arc::new(move |epoch| begin.begin_epoch(epoch)),
            },
            catalog.clone(),
        )
        .await?,
    );
    put(&resources.native, native.clone())?;
    let home = node_path::from_native(&native.native_home);
    let mut protected = vec![artifact_root, home.clone()];
    for key in ["workRoot", "logsRoot"] {
        if let Some(path) = value
            .get(key)
            .and_then(IvyJson::as_js_string)
            .filter(|v| !v.0.is_empty())
        {
            protected.push(path.clone());
        }
    }
    let register_inventory = resources.inventory.clone();
    let register_service = service_link.clone();
    let register_rpc = native.rpc.clone();
    let locations = Arc::new(ProjectLocations::new(
        ProjectPolicy::new(
            data_root.clone(),
            &settings,
            &protected,
            paths,
            &account_home,
        )?,
        Some(Arc::new(move |request| {
            let inventory = get(&register_inventory);
            let service = register_service.clone();
            let rpc = register_rpc.clone();
            Box::pin(async move {
                let reply = rpc
                    .request(
                        "project/create".into(),
                        IvyJson::object([
                            ("name", request.required("name")?.clone()),
                            (
                                "roots",
                                IvyJson::Array(vec![IvyJson::object([(
                                    "path",
                                    request.required("cwd")?.clone(),
                                )])]),
                            ),
                            (
                                "idempotencyKey",
                                request.required("idempotencyKey")?.clone(),
                            ),
                        ]),
                        Default::default(),
                    )
                    .await?;
                if let Some(error) = reply.get("error") {
                    return Err(IvyError::new_js(
                        "native_error".into(),
                        text(error, "message")?,
                    ));
                }
                let inventory = inventory?.ok_or_else(IvyError::internal)?;
                inventory
                    .fresh_projects(InventoryConnection::from_connection(service.connection()?))
                    .await?;
                let snapshot = inventory.project_model()?;
                let id = reply
                    .required("result")?
                    .required("project")?
                    .required("id")?;
                snapshot
                    .projects
                    .find(id.as_js_string().ok_or_else(IvyError::internal)?)
                    .map(|project| project.to_json())
                    .ok_or_else(|| {
                        IvyError::new(
                            "native_inventory_changed",
                            "The created Codex project is not in the current inventory.",
                        )
                    })
            })
        })),
        Some(state.clone()),
        Arc::new(WorkspaceGit::new(
            config.artifact_root().to_owned(),
            Some(ivy_host_runtime::native_plan::native_binary_path(
                config.artifact_root(),
                "ivy-job",
            )),
        )),
    ));
    put(&resources.locations, locations.clone())?;
    environment.attach_after_launch(Some(&home))?;
    events.attach_native(native.rpc.clone())?;
    let fail = resources.clone();
    let rpc = native.rpc.clone();
    let completion = native.completion.clone();
    tokio::spawn(async move {
        let code = rpc.closed().await;
        if !fail.retiring.load(Ordering::Acquire) {
            *fail.failure.lock().unwrap_or_else(|e| e.into_inner()) = Some(code.into());
        }
        let (_, stopped) = tokio::join!(fail.work.close(), completion.wait());
        if let Err(error) = stopped {
            *fail.failure.lock().unwrap_or_else(|e| e.into_inner()) = Some(error.code_js());
        }
    });
    let inventory_source = Arc::new(NativeInventory::new(
        &native.catalog,
        native.epoch.clone(),
        native.rpc.clone(),
    )?);
    let sink = Arc::downgrade(&locations);
    let inventory = InventoryManager::new(
        inventory_source,
        journal.clone(),
        policy.clone(),
        Arc::new(move |projects| {
            if let Some(locations) = sink.upgrade() {
                locations.set_native_projects(projects)?;
            }
            Ok(())
        }),
        closing.clone(),
    )?;
    put(&resources.inventory, inventory.clone())?;
    let notices = BrowserNotices::new(BrowserNoticeSettings {
        service: service_link.clone(),
        closing: closing.clone(),
        operations: operations.clone(),
        state: state.clone(),
        inventory: inventory.clone(),
        policy,
        rpc: native.rpc.clone(),
    });
    events.install(inventory.clone(), notices)?;
    // One retained stop read, at the original startup point before handlers.
    let process_stops = IvyJson::Array(
        OwnedProcessEvidence::new(config.data_root().to_owned())
            .stops()
            .await?,
    );
    let capabilities_service = service_link.clone();
    let capabilities = CapabilityManagement::open(
        capability_store,
        service_node.clone(),
        host.clone(),
        None,
        defaults,
        Arc::new(move |profile| capabilities_service.notify("capabilities", profile.clone())),
    )
    .await?;
    let status_rpc = native.rpc.clone();
    let status_owner = native.status_owner();
    let status_inventory = inventory.clone();
    let status_capabilities = capabilities.clone();
    let status_environment = environment.clone();
    let status_readiness = resources.readiness.clone();
    let status_closing = closing.clone();
    let status_failure = resources.failure.clone();
    let status = Arc::new(AgentStatus::new(
        StatusConfig {
            service_node_id: service_node.clone(),
            host_id: host.clone(),
            settings: settings.clone(),
            catalog_hash: hash_json(&native.catalog)?.into(),
            process_stops,
            windows: cfg!(windows),
        },
        journal.clone(),
        Arc::new(move || {
            let environment = status_environment.status()?;
            Ok(StatusState {
                owner: status_owner.clone(),
                closing: status_closing.load(Ordering::Acquire),
                connected: status_rpc.connected(),
                ready: status_readiness.ready(),
                inventory_code: status_failure
                    .lock()
                    .map_err(|_| IvyError::internal())?
                    .clone()
                    .or(status_inventory.code()?),
                capabilities: status_capabilities.profile()?,
                instructions: environment.as_ref().map(|s| s.instructions.clone()),
                mcp: environment.as_ref().map(|s| s.mcp.clone()),
                skills: environment.map(|s| s.skills),
            })
        }),
    ));
    let interactions = NativeInteractions::new(native.epoch.clone(), InteractionLimits::default());
    let native_management = NativeManagement::new(
        NativeManagementOwner {
            backend: backend.clone(),
            service_node_id: service_node.clone(),
            epoch: native.epoch.clone(),
            catalog: native.catalog.clone(),
            rpc: native.rpc.clone(),
            closing: closing.clone(),
            platform: if cfg!(windows) {
                "win32"
            } else if cfg!(target_os = "macos") {
                "darwin"
            } else {
                std::env::consts::OS
            }
            .into(),
        },
        operations,
        interactions.clone(),
    )?;
    if let Some(tools) = settings.get("bundledTools") {
        let tools = crate::bundled_tools::BundledTools::new(
            native.native_home.clone(),
            config.artifact_root().to_owned(),
            tools.clone(),
        )?;
        native_management.set_thread_preparation(Arc::new(move || {
            let tools = tools.clone();
            Box::pin(async move { tools.refresh().await })
        }));
    }
    let reconciliation = AgentReconciliation::new(ReconciliationSettings {
        service_node_id: service_node.clone(),
        owner: ReconciliationOwner {
            rpc: native.rpc.clone(),
            epoch: native.epoch.clone(),
            closing: closing.clone(),
        },
        journal: journal.clone(),
        state: state.clone(),
        inventory: inventory.clone(),
        environment: environment.synchronizers()?,
        environment_at: environment.startup_at()?,
        health: health.clone(),
    })?;
    let fresh_inventory = inventory.clone();
    let fresh_service = service_link.clone();
    let available = reconciliation.clone();
    let projects = Arc::new(ProjectManagement {
        locations: locations.as_ref().clone(),
        host_id: host,
        service_node_id: service_node,
        available: Arc::new(move || available.available()),
        fresh_projects: Arc::new(move || {
            let inventory = fresh_inventory.clone();
            let connection = fresh_service.connection();
            Box::pin(async move {
                inventory
                    .fresh_projects(InventoryConnection::from_connection(connection?))
                    .await
            })
        }),
        compare_directories: bindings.directory_comparator.clone(),
    });
    let handlers = AgentHandlers {
        native: native_management,
        capabilities,
        projects,
        status,
        notifications,
        journal,
        inventory,
        locations: locations.as_ref().clone(),
        staged: Arc::new(StagedFiles::new(
            node_path::join(
                node_path::Style::native(),
                &[data_root, "staged-files".into()],
            ),
            bindings.canonical_base64.clone(),
        )),
    }
    .compose(&work);
    resources.ensure_starting()?;
    put(&resources.interactions, interactions)?;
    put(&resources.handlers, handlers)?;
    resources.readiness.install(reconciliation)?;
    Ok(())
}

async fn start(
    config: ValidatedAgentConfig,
    bindings: AgentRuntimeBindings,
    apply_reset: bool,
) -> IvyResult<AgentRuntime> {
    let hosted = config.hosted;
    let host_config = config.config().clone();
    let closing = Arc::new(AtomicBool::new(false));
    let health = AgentHealth::host(Arc::new(HealthFile::new(host_config.clone())));
    let resources = Arc::new(HostResources {
        backends: Mutex::new(Vec::new()),
        preparation_failures: Mutex::new(Vec::new()),
        service: slot(),
        health: health.clone(),
        stop_timer: watch::channel(false).0,
    });
    let shutdown = Arc::new(resources.shutdown(closing.clone()));
    let result = async {
        health
            .write(
                false,
                None,
                "Starting the pinned native backends in their own homes.".into(),
            )?
            .wait()
            .await?;
        let service_link = ServiceLink::default();
        let mut routes = Vec::new();
        let mut launches = Vec::new();
        let mut catalogs = HashMap::<(JsString, JsString), Arc<IvyJson>>::new();
        // Cross-account configuration is already validated. All remaining
        // preparation and launch failures belong to the selected backend.
        for backend in config.backends {
            let backend_config = Arc::new(backend.config);
            let mut prepared_owner = None;
            let prepared = async {
                if apply_reset {
                    crate::runtime_reset::apply_agent_runtime_reset(
                        backend_config.data_root().to_owned(),
                        backend_config.json().required("settings")?.clone(),
                        bindings.home.clone(),
                    )
                    .await?;
                }
                let owner = prepare_backend(
                    backend_config.clone(),
                    backend.identity.clone(),
                    health.clone(),
                )
                .await?;
                prepared_owner = Some(owner.clone());
                resources
                    .backends
                    .lock()
                    .map_err(|_| IvyError::internal())?
                    .push(owner.clone());
                let settings = owner.config.json().required("settings")?;
                let catalog_key = (
                    text(settings, "nativeVersion")?,
                    text(settings, "nativeExecutableHash")?,
                );
                let catalog = if let Some(catalog) = catalogs.get(&catalog_key) {
                    catalog.clone()
                } else {
                    let catalog = load_native_catalog(
                        &text(owner.config.json(), "artifactRoot")?,
                        settings,
                        bindings.home.paths.style,
                    )
                    .await?;
                    catalogs.insert(catalog_key, catalog.clone());
                    catalog
                };
                let registry = crate::registry::backend_registry(&catalog, owner.backend.as_ref())?;
                let handlers = backend_handlers(owner.clone(), &registry, catalog.clone())?;
                Ok::<_, IvyError>((
                    BackendRoute {
                        identity: owner.backend.clone(),
                        registry,
                        handlers,
                        preparation_failure: None,
                    },
                    owner,
                    catalog,
                ))
            }
            .await;
            match prepared {
                Ok((route, owner, catalog)) => {
                    routes.push(route);
                    launches.push((owner, catalog));
                }
                Err(error) => {
                    if let Some(owner) = &prepared_owner {
                        *owner.failure.lock().map_err(|_| IvyError::internal())? =
                            Some(error.code_js());
                        owner.closing.store(true, Ordering::Release);
                        owner.startup.send_replace(Some(Err(error.clone())));
                    }
                    if !hosted {
                        return Err(error);
                    }
                    let failure = Arc::new(BackendFailure::new(
                        &backend_config,
                        backend.identity.ok_or_else(IvyError::internal)?,
                        &error,
                        prepared_owner.is_some(),
                    )?);
                    if prepared_owner.is_none() {
                        resources
                            .preparation_failures
                            .lock()
                            .map_err(|_| IvyError::internal())?
                            .push(failure.clone());
                    }
                    let mut route = BackendRoute::failed(failure)?;
                    if let Some(owner) = prepared_owner {
                        let service_node = text(owner.config.json(), "serviceNodeId")?;
                        let operations = owner.operations.clone();
                        route.handlers.management.insert(
                            "agent.operation".into(),
                            Arc::new(move |args, context| {
                                let service_node = service_node.clone();
                                let operations = operations.clone();
                                Box::pin(async move {
                                    context.throw_if_aborted()?;
                                    crate::native_management::read_operation(
                                        &operations,
                                        &service_node,
                                        &args,
                                        context,
                                    )
                                    .await
                                })
                            }),
                        );
                    }
                    routes.push(route);
                }
            }
        }
        let routes = BackendRoutes::new(routes)?;
        // Roots become known once each backend's native home is resolved.
        let roots = Arc::downgrade(&resources);
        routes.host_metrics().set_project_roots(Arc::new(move || {
            roots.upgrade().map_or_else(Vec::new, |resources| {
                resources
                    .backends()
                    .iter()
                    .filter_map(|backend| get(&backend.locations).ok().flatten())
                    .map(|locations| locations.project_root())
                    .collect()
            })
        }));
        let (reconcile, readiness) =
            resources.dependencies(text(host_config.json(), "serviceNodeId")?);
        let service = Arc::new(ServiceClient::new(service_options(
            &host_config,
            routes.registry().clone(),
            routes.handlers(),
            reconcile,
            readiness,
            health.clone(),
            closing.clone(),
        )?)?);
        service_link.bind(&service)?;
        put(&resources.service, service.clone())?;
        for (owner, catalog) in launches {
            let bindings = bindings.clone();
            let health = health.clone();
            let service_link = service_link.clone();
            tokio::spawn(async move {
                let result =
                    launch_backend(owner.clone(), bindings, health, service_link, catalog).await;
                if let Err(error) = &result {
                    *owner.failure.lock().unwrap_or_else(|e| e.into_inner()) =
                        Some(error.code_js());
                    owner.closing.store(true, Ordering::Release);
                    if let Ok(Some(native)) = get(&owner.native) {
                        let _ = native.close().await;
                    }
                }
                owner.startup.send_replace(Some(result));
            });
        }
        if !hosted {
            for backend in resources.backends() {
                backend.started().await?;
            }
        }
        control_timer(
            health,
            resources.clone(),
            resources.stop_timer.subscribe(),
            Arc::downgrade(&shutdown),
            closing,
        );
        service.start();
        Ok(AgentRuntime {
            service,
            shutdown: shutdown.clone(),
        })
    }
    .await;
    if result.is_err() {
        // Preparation can fail before a backend launch was accepted.
        for backend in resources.backends() {
            if backend.startup.borrow().is_none() && get(&backend.environment)?.is_none() {
                backend.startup.send_replace(Some(Err(IvyError::new(
                    "agent_start_failed",
                    "Backend startup was not admitted.",
                ))));
            }
        }
        if let Ok(run) = shutdown.close("agent_start_failed".into()) {
            let _ = run.wait().await;
        }
    }
    result
}
