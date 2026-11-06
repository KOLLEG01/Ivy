//! Backend resources, late handler admission and original-outcome queries.
use crate::{
    backend_readiness::BackendReadiness,
    environment_manager::ManagedEnvironment,
    interactions::NativeInteractions,
    inventory_manager::InventoryManager,
    local_state::LocalAgentState,
    native_connection::AgentNativeConnection,
    native_handshake::NativeMode,
    native_journal::NativeJournal,
    native_operations::{NativeOperations, ReceiptWork},
    notice_policy::NativeNoticePolicy,
    notifications::{NativeNotifications, notification_handler},
    project_locations::ProjectLocations,
    service_events::ServiceEvents,
    service_kernel::{HandlerSet, now, text},
    service_work::AgentWork,
};
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString, hash::hash_json, native_backend::BackendIdentity,
};
use ivy_host_runtime::config::InstanceConfig;
use ivy_sdk::service::{ServiceFuture, ToolHandler};
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::sync::watch;

pub(crate) type Slot<T> = Arc<Mutex<Option<T>>>;
pub(crate) fn slot<T>() -> Slot<T> {
    Arc::new(Mutex::new(None))
}
pub(crate) fn get<T: Clone>(slot: &Slot<T>) -> IvyResult<Option<T>> {
    Ok(slot.lock().map_err(|_| IvyError::internal())?.clone())
}
pub(crate) fn put<T>(slot: &Slot<T>, value: T) -> IvyResult<()> {
    *slot.lock().map_err(|_| IvyError::internal())? = Some(value);
    Ok(())
}
pub(crate) struct BackendResources {
    pub(crate) config: Arc<InstanceConfig>,
    pub(crate) backend: Option<BackendIdentity>,
    pub(crate) native: Slot<Arc<AgentNativeConnection>>,
    pub(crate) events: Slot<ServiceEvents>,
    pub(crate) environment: Slot<ManagedEnvironment>,
    pub(crate) inventory: Slot<InventoryManager>,
    pub(crate) locations: Slot<Arc<ProjectLocations>>,
    pub(crate) receipts: ReceiptWork,
    pub(crate) work: AgentWork,
    pub(crate) state: Arc<Mutex<LocalAgentState>>,
    pub(crate) operations: NativeOperations,
    pub(crate) notifications: Arc<Mutex<NativeNotifications>>,
    pub(crate) policy: Arc<Mutex<NativeNoticePolicy>>,
    pub(crate) journal: Arc<Mutex<NativeJournal>>,
    pub(crate) readiness: BackendReadiness,
    pub(crate) closing: Arc<AtomicBool>,
    pub(crate) retiring: AtomicBool,
    pub(crate) interactions: Slot<NativeInteractions>,
    pub(crate) handlers: Slot<HandlerSet>,
    pub(crate) failure: Arc<Mutex<Option<JsString>>>,
    pub(crate) startup: watch::Sender<Option<IvyResult<()>>>,
}
impl BackendResources {
    pub(crate) fn ensure_starting(&self) -> IvyResult<()> {
        if self.retiring.load(Ordering::Acquire) {
            Err(IvyError::new(
                "native_owner_stopping",
                "Backend startup is retiring.",
            ))
        } else {
            Ok(())
        }
    }
    pub(crate) fn started(&self) -> ServiceFuture<()> {
        let mut state = self.startup.subscribe();
        Box::pin(async move {
            loop {
                if let Some(result) = state.borrow_and_update().clone() {
                    return result;
                }
                state.changed().await.map_err(|_| IvyError::internal())?;
            }
        })
    }
    pub(crate) fn retire_admission(self: &Arc<Self>) -> ServiceFuture<()> {
        self.retiring.store(true, Ordering::Release);
        let owner = self.clone();
        let native_work = Box::pin(async move {
            // A startup accepted before close owns its exact native cleanup.
            let _ = owner.started().await;
            if let Some(native) = get(&owner.native)? {
                if native.mode != NativeMode::ExternalProxy {
                    if let Some(events) = get(&owner.events)? {
                        events.wait_idle().await?;
                    }
                }
            }
            Ok(())
        });
        self.work.close_after(native_work)
    }
    pub(crate) fn environment_step(&self, kind: u8) -> ServiceFuture<()> {
        let environment = get(&self.environment);
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
    }
}
pub(crate) fn backend_handlers(
    owner: Arc<BackendResources>,
    registry: &IvyJson,
    catalog: Arc<IvyJson>,
) -> IvyResult<HandlerSet> {
    let mut management = HashMap::new();
    let catalog_hash = hash_json(&catalog)?;
    let service_node = text(owner.config.json(), "serviceNodeId")?;
    let namespaces = registry
        .required("namespaces")?
        .as_array()
        .ok_or_else(IvyError::internal)?;
    let agent = namespaces
        .iter()
        .find(|v| v.get("namespace") == Some(&"agent".into()))
        .ok_or_else(IvyError::internal)?;
    for definition in agent
        .required("tools")?
        .as_array()
        .ok_or_else(IvyError::internal)?
    {
        let name = format!("agent.{}", definition.required("name")?.as_str()?);
        let key = JsString::from(name.as_str());
        let owner = owner.clone();
        let catalog = catalog.clone();
        let catalog_hash = catalog_hash.clone();
        let service_node = service_node.clone();
        let method = key.clone();
        let handler: ToolHandler = Arc::new(move |args, context| {
            let selected = (|| {
                context.throw_if_aborted()?;
                let query = [
                    "agent.status",
                    "agent.catalog",
                    "agent.storageIdentity",
                    "agent.discover",
                    "agent.frameLimits",
                    "agent.operation",
                    "agent.interaction",
                    "agent.inputs",
                    "agent.inputDefinition",
                    "agent.notifications",
                    "agent.capabilities",
                ]
                .iter()
                .any(|name| method.eq_str(name));
                if !query {
                    owner.readiness.check(&context)?;
                }
                Ok(owner
                    .handlers
                    .lock()
                    .map_err(|_| IvyError::internal())?
                    .as_ref()
                    .and_then(|handlers| handlers.management.get(&method).cloned()))
            })();
            match selected {
                Ok(Some(handler)) => handler(args, context),
                Err(error) => Box::pin(async move { Err(error) }),
                Ok(None) => {
                    let owner = owner.clone();
                    let catalog = catalog.clone();
                    let catalog_hash = catalog_hash.clone();
                    let service_node = service_node.clone();
                    let method = method.clone();
                    Box::pin(async move {
                        if method.eq_str("agent.status") {
                            return starting_status(&owner, &catalog_hash);
                        }
                        if method.eq_str("agent.catalog") {
                            return Ok(catalog.as_ref().clone());
                        }
                        if method.eq_str("agent.storageIdentity") {
                            return owner.operations.storage_identity();
                        }
                        if method.eq_str("agent.frameLimits") {
                            return crate::native_management::frame_limits(
                                &service_node,
                                &catalog,
                                &catalog_hash,
                                &owner.journal,
                            );
                        }
                        if method.eq_str("agent.operation") {
                            return crate::native_management::read_operation(
                                &owner.operations,
                                &service_node,
                                &args,
                                context,
                            )
                            .await;
                        }
                        if method.eq_str("agent.inputs") {
                            return owner
                                .journal
                                .lock()
                                .map_err(|_| IvyError::internal())?
                                .inputs(&args);
                        }
                        if method.eq_str("agent.notifications") {
                            return notification_handler(
                                owner.notifications.clone(),
                                owner.journal.clone(),
                            )(args, context)
                            .await;
                        }
                        Err(IvyError::new(
                            "native_unavailable",
                            "The selected native backend has not finished startup.",
                        ))
                    })
                }
            }
        });
        management.insert(key, handler);
    }
    let mut native = HashMap::new();
    for definition in catalog
        .required("clientRequests")?
        .as_array()
        .ok_or_else(IvyError::internal)?
    {
        let name = JsString::from(format!(
            "{}.{}",
            owner.backend.as_ref().map_or("codex", BackendIdentity::id),
            definition.required("method")?.as_str()?
        ));
        let method = name.clone();
        let owner = owner.clone();
        let handler: ToolHandler = Arc::new(move |args, context| {
            let selected = (|| {
                context.throw_if_aborted()?;
                owner.readiness.check(&context)?;
                owner
                    .handlers
                    .lock()
                    .map_err(|_| IvyError::internal())?
                    .as_ref()
                    .and_then(|handlers| handlers.native.get(&method).cloned())
                    .ok_or_else(|| {
                        IvyError::new(
                            "native_unavailable",
                            "The selected native backend is unavailable.",
                        )
                    })
            })();
            match selected {
                Ok(handler) => handler(args, context),
                Err(error) => Box::pin(async move { Err(error) }),
            }
        });
        native.insert(name, handler);
    }
    Ok(HandlerSet { management, native })
}
fn starting_status(owner: &BackendResources, catalog_hash: &str) -> IvyResult<IvyJson> {
    let value = owner.config.json();
    let settings = value.required("settings")?;
    let journal = owner.journal.lock().map_err(|_| IvyError::internal())?;
    let code = owner
        .failure
        .lock()
        .map_err(|_| IvyError::internal())?
        .clone();
    Ok(IvyJson::object([
        ("serviceNodeId", value.required("serviceNodeId")?.clone()),
        ("hostId", value.required("hostId")?.clone()),
        (
            "serverType",
            if owner.backend.as_ref().is_some_and(|v| {
                v.provider() == ivy_contracts::native_backend::NativeProvider::Claude
            }) {
                "claude"
            } else {
                "codex"
            }
            .into(),
        ),
        ("nativeVersion", settings.required("nativeVersion")?.clone()),
        (
            "nativeExecutableHash",
            settings.required("nativeExecutableHash")?.clone(),
        ),
        ("catalogHash", catalog_hash.into()),
        (
            "epoch",
            journal
                .epoch()
                .cloned()
                .map(IvyJson::String)
                .unwrap_or(IvyJson::Null),
        ),
        ("pid", IvyJson::Null),
        (
            "state",
            if code.is_some() {
                "failed"
            } else if owner.retiring.load(Ordering::Acquire) {
                "stopping"
            } else {
                "starting"
            }
            .into(),
        ),
        ("observedAt", IvyJson::String(now())),
        ("code", code.map(IvyJson::String).unwrap_or(IvyJson::Null)),
        ("initialized", IvyJson::Null),
        (
            "pendingInputs",
            IvyJson::Number(journal.pending_input_count() as f64),
        ),
        ("operations", journal.status()),
        ("observedMethods", journal.observed_methods()),
    ]))
}
