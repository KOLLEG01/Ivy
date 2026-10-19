//! Agent service composition after instance validation and runtime reset.
use crate::{
    agent_health::AgentHealth,
    browser_notices::{BrowserNoticeSettings, BrowserNotices, FormQuery},
    capabilities::{CapabilityManagement, SqliteCapabilityStore, initialize_profile},
    control::AgentControl,
    directories::DirectoryComparator,
    environment_defaults::load_environment_defaults,
    environment_manager::{EnvironmentManagerSettings, ManagedEnvironment},
    instructions_source::EnglishHintComparator,
    interactions::{Limits as InteractionLimits, NativeInteractions},
    inventory::NativeInventory,
    inventory_manager::{InventoryConnection, InventoryManager},
    local_state::{Limits as StateLimits, LocalAgentState},
    native_connection::AgentNativeConnection,
    native_journal::{Limits as JournalLimits, NativeJournal, Owner},
    native_launch::{
        NativeHomeInputs, NativeProcessOptions, managed_environment_before_launch,
        start_native_process,
    },
    native_management::{NativeManagement, NativeManagementOwner},
    native_operations::{NativeOperations, ReceiptWork, SqliteOperationStore},
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
    shutdown::{AgentShutdown, ShutdownPhases, ShutdownRun},
    skills_files::SkillsComparator,
    staged_files::{CanonicalBase64Decoder, StagedFiles},
    status::{AgentStatus, StatusConfig, StatusState},
};
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString, agent_schema::validate_agent, hash::hash_json,
    json::stringify,
};
use ivy_host_runtime::{config::InstanceConfig, health::HealthFile, node_path};
use ivy_sdk::{
    rpc::RpcClient,
    service::{ServiceClient, ServiceOptions, StateObserver, ToolHandler},
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

/// Required runtime interfaces have no locale, codec or transport fallback.
/// The executable supplies qualified Shared implementations and a bootstrap
/// Hive client with the selected runtime's trust/proxy policy.
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
/// publication. Accepted invocations use the same work owner as shutdown.
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
                .map(|(name, handler)| (name, work.handler(handler)))
                .collect(),
            native: self
                .native
                .json_handlers()
                .into_iter()
                .map(|(name, handler)| (name, work.handler(handler)))
                .collect(),
        }
    }
}

fn text(value: &IvyJson, key: &str) -> IvyResult<JsString> {
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
fn now() -> JsString {
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
    identity.insert(
        "nativeVersion",
        value
            .required("settings")?
            .required("nativeVersion")?
            .clone(),
    );
    let (reconcile, readiness) = reconciliation.dependencies();
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
    options.heartbeat = Duration::from_secs(2);
    options.on_state = Some(service_state_observer(health, closing));
    Ok(options)
}

type Slot<T> = Arc<Mutex<Option<T>>>;
fn slot<T>() -> Slot<T> {
    Arc::new(Mutex::new(None))
}
fn get<T: Clone>(slot: &Slot<T>) -> IvyResult<Option<T>> {
    Ok(slot.lock().map_err(|_| IvyError::internal())?.clone())
}
fn put<T>(slot: &Slot<T>, value: T) -> IvyResult<()> {
    *slot.lock().map_err(|_| IvyError::internal())? = Some(value);
    Ok(())
}
struct Resources {
    native: Slot<Arc<AgentNativeConnection>>,
    service: Slot<Arc<ServiceClient>>,
    environment: Slot<ManagedEnvironment>,
    inventory: Slot<InventoryManager>,
    locations: Slot<Arc<ProjectLocations>>,
    receipts: ReceiptWork,
    work: AgentWork,
    state: Arc<Mutex<LocalAgentState>>,
    health: AgentHealth,
    journal: Arc<Mutex<NativeJournal>>,
    stop_timer: watch::Sender<bool>,
}
impl Resources {
    fn shutdown(self: &Arc<Self>, closing: Arc<AtomicBool>) -> AgentShutdown {
        let resources = self.clone();
        let native = self.clone();
        let work = self.clone();
        let service = self.clone();
        let locations = self.clone();
        let state = self.clone();
        let health = self.clone();
        let final_health = self.clone();
        let journal = self.clone();
        let environment_step = |kind: u8| -> Arc<crate::shutdown::ShutdownStep> {
            let resources = self.clone();
            Arc::new(move || {
                let environment = get(&resources.environment);
                Box::pin(async move {
                    if let Some(environment) = environment? {
                        let steps = environment.shutdown_steps();
                        match kind {
                            0 => (steps.instructions)().await,
                            1 => (steps.mcp)().await,
                            _ => (steps.skills)().await,
                        }
                    } else {
                        Ok(())
                    }
                })
            })
        };
        AgentShutdown::new(
            ShutdownPhases {
                stop_timer: Arc::new(move || {
                    resources.stop_timer.send_replace(true);
                }),
                native: Arc::new(move || {
                    // Signal the native owner at phase admission, before its waiter.
                    let run = get(&native.native).map(|owner| owner.map(|owner| owner.close()));
                    Box::pin(async move {
                        if let Some(run) = run? {
                            run.await?;
                        }
                        Ok(())
                    })
                }),
                work: Arc::new(move || {
                    let invocations = work.work.drain_snapshot();
                    let receipts = work.receipts.clone();
                    let inventory = get(&work.inventory);
                    Box::pin(async move {
                        let _ = tokio::join!(invocations, receipts.drain(), async {
                            if let Some(inventory) = inventory? {
                                inventory.drain().await?;
                            }
                            Ok::<_, IvyError>(())
                        });
                        Ok(())
                    })
                }),
                service: Arc::new(move || {
                    let service = get(&service.service);
                    Box::pin(async move {
                        if let Some(service) = service? {
                            service.stop().await;
                        }
                        Ok(())
                    })
                }),
                instructions: environment_step(0),
                mcp: environment_step(1),
                skills: environment_step(2),
                locations: Arc::new(move || {
                    let result = get(&locations.locations)
                        .and_then(|owner| owner.map_or(Ok(()), |owner| owner.close()));
                    Box::pin(async move { result })
                }),
                state: Arc::new(move || {
                    let state = state.state.clone();
                    Box::pin(async move {
                        tokio::task::spawn_blocking(move || {
                            state
                                .lock()
                                .map_err(|_| IvyError::internal())?
                                .close_shared()
                        })
                        .await
                        .map_err(|_| IvyError::internal())?
                    })
                }),
                pending_health: Arc::new(move || health.health.drain_snapshot()),
                final_health: Arc::new(move |code| {
                    let health = final_health.health.file().clone();
                    let mut message = JsString::from("AgentManager stopped: ");
                    message.0.extend(code.0);
                    message.0.push(46);
                    Box::pin(async move { health.write(false, message, None).await })
                }),
                journal: Arc::new(move || {
                    journal
                        .journal
                        .lock()
                        .map_err(|_| IvyError::internal())?
                        .close();
                    Ok(())
                }),
            },
            closing,
        )
    }
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

/// Called only after the original configuration/runtime-reset and explicit
/// migration entry has completed. This boundary does not replace those steps.
/// Startup remains owned if its waiter is dropped; an abandoned successful
/// startup closes through the same resource owner instead of leaking a service.
pub fn start_prepared_agent(
    config: InstanceConfig,
    bindings: AgentRuntimeBindings,
) -> impl std::future::Future<Output = IvyResult<AgentRuntime>> + Send {
    let (sender, receiver) = oneshot::channel();
    tokio::spawn(async move {
        if let Err(Ok(runtime)) = sender.send(start(config, bindings).await) {
            if let Ok(run) = runtime.close() {
                let _ = run.wait().await;
            }
        }
    });
    async move { receiver.await.map_err(|_| IvyError::internal())? }
}
async fn start(config: InstanceConfig, bindings: AgentRuntimeBindings) -> IvyResult<AgentRuntime> {
    let value = config.json();
    let settings = value.required("settings")?.clone();
    validate_agent("Settings", &settings)?;
    let credential = value
        .get("credential")
        .and_then(IvyJson::as_js_string)
        .filter(|v| !v.0.is_empty())
        .cloned();
    if value.get("componentId") != Some(&"agent-manager".into()) || credential.is_none() {
        return Err(IvyError::new(
            "invalid_arguments",
            "AgentManager requires its own component identity and Hive credential.",
        ));
    }
    let service_node = text(value, "serviceNodeId")?;
    let host = text(value, "hostId")?;
    let limits = settings.required("limits")?;
    let journal = Arc::new(Mutex::new(NativeJournal::new(
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
    )?));
    let range = journal
        .lock()
        .map_err(|_| IvyError::internal())?
        .reserve_notification_sequence();
    let notifications = Arc::new(Mutex::new(NativeNotifications::new_js(
        service_node.clone(),
        text(&settings, "nativeVersion")?,
        number(&range, "start")? as u64,
        number(&range, "end")? as u64,
        number(limits, "maxNotificationBytes")?,
    )));
    let policy = Arc::new(Mutex::new(NativeNoticePolicy::default()));
    let health = AgentHealth::new(Arc::new(HealthFile::new(config.clone())), journal.clone());
    let filename = config.data_root().join("journal/agent-runtime.sqlite");
    let state_service = service_node.to_utf8()?;
    let state_limits = StateLimits {
        max_operations: number(limits, "maxOperations")? as i64,
        max_journal_bytes: number(limits, "maxJournalBytes")? as i64,
    };
    let state = Arc::new(Mutex::new(
        tokio::task::spawn_blocking(move || {
            LocalAgentState::open(&filename, state_service, state_limits)
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
    let failure_health = health.clone();
    let receipts = operations.reconcile_terminals(Arc::new(move |error| {
        let mut message = JsString::from("Hive receipt could not be saved: ");
        message.0.extend(error.code_js().0);
        let _ = failure_health.write(false, None, message);
    }));
    let work = AgentWork::default();
    let closing = Arc::new(AtomicBool::new(false));
    let service_link = ServiceLink::default();
    let (stop_timer, _) = watch::channel(false);
    let resources = Arc::new(Resources {
        native: slot(),
        service: slot(),
        environment: slot(),
        inventory: slot(),
        locations: slot(),
        receipts,
        work: work.clone(),
        state: state.clone(),
        health: health.clone(),
        journal: journal.clone(),
        stop_timer,
    });
    let shutdown = Arc::new(resources.shutdown(closing.clone()));
    let result = async {
        health
            .write(
                false,
                None,
                "Starting the pinned public native process in its own home.".into(),
            )?
            .wait()
            .await?;
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
            english_hint_comparator: bindings.english_hint_comparator,
            skills_comparator: bindings.skills_comparator,
        });
        put(&resources.environment, environment.clone())?;
        let events = ServiceEvents::new(ServiceEventSettings {
            service_node_id: service_node.clone(),
            service: service_link.clone(),
            closing: closing.clone(),
            operations: operations.clone(),
            notifications: notifications.clone(),
            policy: policy.clone(),
            query: bindings.form_query,
            work: work.clone(),
        });
        let paths = node_path::Context {
            style: bindings.home.paths.style,
            cwd: bindings.home.paths.cwd.clone(),
            drive_cwds: bindings.home.paths.drive_cwds.clone(),
        };
        let account_home = bindings.home.account_home.clone();
        let begin = events.clone();
        let native = Arc::new(
            start_native_process(NativeProcessOptions {
                artifact_root: artifact_root.clone(),
                data_root: data_root.clone(),
                settings: settings.clone(),
                client_version: text(value, "version")?,
                hive_credential: credential,
                home_inputs: bindings.home,
                before_launch: Some(managed_environment_before_launch(
                    environment.clone(),
                    bindings.bootstrap,
                )),
                begin_epoch: Arc::new(move |epoch| begin.begin_epoch(epoch)),
            })
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
                    let snapshot = inventory.project_snapshot()?;
                    let id = reply
                        .required("result")?
                        .required("project")?
                        .required("id")?;
                    snapshot
                        .required("projects")?
                        .required("projects")?
                        .as_array()
                        .ok_or_else(IvyError::internal)?
                        .iter()
                        .find(|project| project.get("nativeId") == Some(id))
                        .cloned()
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
                Some(config.artifact_root().join("dist/native/ivy-job.exe")),
            )),
        ));
        put(&resources.locations, locations.clone())?;
        environment.attach_after_launch(Some(&home))?;
        events.attach_native(native.rpc.clone())?;
        let fail = Arc::downgrade(&shutdown);
        let rpc = native.rpc.clone();
        tokio::spawn(async move {
            let code = rpc.closed().await;
            if let Some(shutdown) = fail.upgrade() {
                let _ = shutdown.close(code.into());
            }
        });
        let inventory_source = Arc::new(NativeInventory::new(
            native.catalog.clone(),
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
        let status_service = service_link.clone();
        let status_closing = closing.clone();
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
                    ready: status_service.ready(),
                    inventory_code: status_inventory.code()?,
                    capabilities: status_capabilities.profile()?,
                    instructions: environment.as_ref().map(|s| s.instructions.clone()),
                    mcp: environment.as_ref().map(|s| s.mcp.clone()),
                    skills: environment.map(|s| s.skills),
                })
            }),
        ));
        let native_management = NativeManagement::new(
            NativeManagementOwner {
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
            NativeInteractions::new(native.epoch.clone(), InteractionLimits::default()),
        )?;
        let registry = native_management.registry().clone();
        let reconciliation = AgentReconciliation::new(ReconciliationSettings {
            service_node_id: service_node.clone(),
            registry: registry.clone(),
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
            compare_directories: bindings.directory_comparator,
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
                bindings.canonical_base64,
            )),
        }
        .compose(&work);
        let service = Arc::new(ServiceClient::new(agent_service_options(
            &config,
            registry,
            handlers,
            &reconciliation,
            health.clone(),
            closing.clone(),
        )?)?);
        service_link.bind(&service)?;
        put(&resources.service, service.clone())?;
        control_timer(
            health,
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
        if let Ok(run) = shutdown.close("agent_start_failed".into()) {
            let _ = run.wait().await;
        }
    }
    result
}
fn control_timer(
    health: AgentHealth,
    mut stopped: watch::Receiver<bool>,
    shutdown: std::sync::Weak<AgentShutdown>,
    closing: Arc<AtomicBool>,
) {
    let file = health.file().clone();
    let control = AgentControl::new(
        Arc::new(move || {
            let file = file.clone();
            Box::pin(async move { Ok(file.control().await?.is_some()) })
        }),
        Arc::new(move |code| {
            if let Some(shutdown) = shutdown.upgrade() {
                let _ = shutdown.close(code);
            }
        }),
        closing,
    );
    tokio::spawn(async move {
        let period = Duration::from_millis(500);
        let mut timer = tokio::time::interval_at(tokio::time::Instant::now() + period, period);
        timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            if *stopped.borrow() {
                break;
            }
            tokio::select! {
                biased;
                _ = stopped.changed() => break,
                _ = timer.tick() => {
                    control.tick();
                }
            }
        }
    });
}
