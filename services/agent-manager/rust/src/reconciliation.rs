//! Generation reconciliation and independent environment/inventory readiness.
use crate::{
    agent_health::AgentHealth,
    inventory::InventoryClock,
    inventory_manager::{InventoryConnection, InventoryManager},
    local_state::LocalAgentState,
    native_journal::{JournalClock, NativeJournal},
};
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString, Outcome, hash::hash_json};
use ivy_host_runtime::codex_rpc::NativeRpc;
use ivy_sdk::{
    rpc::{RequestOptions, RpcClient},
    service::{ConnectionDependency, ServiceConnection},
};
use std::{
    future::Future,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

pub type EnvironmentWait = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;
/// Each concrete manager starts or joins its own application immediately. Its
/// wait handle cannot fail: application failures are retained in manager status.
pub type EnvironmentRefresh = dyn Fn(Arc<dyn RpcClient>) -> EnvironmentWait + Send + Sync;
pub struct EnvironmentSynchronizers {
    pub instructions: Arc<EnvironmentRefresh>,
    pub mcp: Arc<EnvironmentRefresh>,
    pub skills: Arc<EnvironmentRefresh>,
}
impl EnvironmentSynchronizers {
    fn synchronize(&self, connection: Arc<dyn RpcClient>) -> EnvironmentWait {
        let instructions = (self.instructions)(connection.clone());
        let mcp = (self.mcp)(connection.clone());
        let skills = (self.skills)(connection);
        Box::pin(async move {
            tokio::join!(instructions, mcp, skills);
        })
    }
}

/// The pre-ready call must fail at Hive's readiness fence, before any provider
/// invocation. Success and every other error prove no compatible transport.
pub struct NativeTransportProbe {
    arguments: IvyJson,
}
impl NativeTransportProbe {
    pub fn new(service_node_id: JsString, registry: &IvyJson) -> IvyResult<Self> {
        let probe = registry
            .get("namespaces")
            .and_then(IvyJson::as_array)
            .and_then(|namespaces| namespaces.first())
            .and_then(|namespace| namespace.get("tools"))
            .and_then(IvyJson::as_array)
            .and_then(|tools| {
                tools
                    .iter()
                    .find(|tool| tool.get("name") == Some(&"account/rateLimits/read".into()))
            })
            .ok_or_else(|| {
                IvyError::new(
                    "native_catalog_mismatch",
                    "Pinned native catalog lacks the null-argument transport probe.",
                )
            })?;
        let mut qualified = JsString::from("codex.");
        qualified.0.extend(
            probe
                .required("name")?
                .as_js_string()
                .ok_or_else(IvyError::internal)?
                .0
                .iter()
                .copied(),
        );
        Ok(Self {
            arguments: IvyJson::object([
                ("qualifiedName", IvyJson::String(qualified)),
                ("serviceNodeId", IvyJson::String(service_node_id)),
                ("expectedDefinitionHash", hash_json(probe)?.into()),
                ("arguments", IvyJson::Null),
            ]),
        })
    }
    pub async fn verify(&self, connection: &dyn RpcClient) -> IvyResult<()> {
        match connection
            .request(
                "tools.call",
                self.arguments.clone(),
                RequestOptions::default(),
            )
            .await
        {
            Err(error)
                if error.code_js() == "service_not_ready".into()
                    && error.outcome == Outcome::NotExecuted =>
            {
                Ok(())
            }
            _ => Err(IvyError::new(
                "hive_native_json_unsupported",
                "Hive did not verify unchanged native JSON transport before readiness.",
            )),
        }
    }
}

pub struct ReconciliationOwner {
    pub rpc: NativeRpc,
    pub epoch: JsString,
    pub closing: Arc<AtomicBool>,
}
pub struct ReconciliationSettings {
    pub service_node_id: JsString,
    pub registry: IvyJson,
    pub owner: ReconciliationOwner,
    pub journal: Arc<Mutex<NativeJournal>>,
    pub state: Arc<Mutex<LocalAgentState>>,
    pub inventory: InventoryManager,
    /// Some contains the complete manager trio; partial environment ownership
    /// never triggers synchronization, as in the original service.
    pub environment: Option<EnvironmentSynchronizers>,
    pub environment_at: f64,
    pub health: AgentHealth,
}
struct State {
    connection: Option<InventoryConnection>,
    environment_at: f64,
}
struct Inner {
    settings: ReconciliationSettings,
    probe: NativeTransportProbe,
    state: Mutex<State>,
    now: Arc<InventoryClock>,
    iso_now: Arc<JournalClock>,
}
#[derive(Clone)]
pub struct AgentReconciliation(Arc<Inner>);
impl AgentReconciliation {
    pub fn new(settings: ReconciliationSettings) -> IvyResult<Self> {
        Self::with_clock(
            settings,
            Arc::new(|| chrono::Utc::now().timestamp_millis() as f64),
            Arc::new(|| {
                chrono::Utc::now()
                    .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
                    .into()
            }),
        )
    }
    pub fn with_clock(
        settings: ReconciliationSettings,
        now: Arc<InventoryClock>,
        iso_now: Arc<JournalClock>,
    ) -> IvyResult<Self> {
        let probe =
            NativeTransportProbe::new(settings.service_node_id.clone(), &settings.registry)?;
        let state = State {
            connection: None,
            environment_at: settings.environment_at,
        };
        Ok(Self(Arc::new(Inner {
            settings,
            probe,
            state: Mutex::new(state),
            now,
            iso_now,
        })))
    }
    pub fn connection(&self) -> IvyResult<Option<InventoryConnection>> {
        Ok(self
            .0
            .state
            .lock()
            .map_err(|_| IvyError::internal())?
            .connection
            .clone())
    }
    pub fn available(&self) -> IvyResult<()> {
        let owner = &self.0.settings.owner;
        if !owner.closing.load(Ordering::Acquire)
            && owner.rpc.connected()
            && self
                .0
                .settings
                .journal
                .lock()
                .map_err(|_| IvyError::internal())?
                .epoch()
                == Some(&owner.epoch)
        {
            Ok(())
        } else {
            Err(IvyError::new(
                "native_unavailable",
                "The current native owner is unavailable.",
            ))
        }
    }
    pub async fn reconcile(&self, connection: InventoryConnection) -> IvyResult<()> {
        self.0
            .state
            .lock()
            .map_err(|_| IvyError::internal())?
            .connection = Some(connection.clone());
        let status = connection
            .client
            .request(
                "system.status",
                IvyJson::object::<&str>([]),
                RequestOptions::default(),
            )
            .await?;
        self.0
            .settings
            .state
            .lock()
            .map_err(|_| IvyError::internal())?
            .set_runtime_epoch(
                status
                    .required("runtimeEpoch")?
                    .as_js_string()
                    .ok_or_else(IvyError::internal)?
                    .clone(),
            )?;
        self.0.settings.inventory.bind(connection.clone())?;
        self.available()?;
        self.0.probe.verify(connection.client.as_ref()).await?;
        if let Some(environment) = &self.0.settings.environment {
            environment.synchronize(connection.client.clone()).await;
            self.0
                .state
                .lock()
                .map_err(|_| IvyError::internal())?
                .environment_at = (self.0.now)();
        }
        // Each manager owns its accepted work and retains its own failure code.
        // These independent refreshes do not gate readiness.
        let _ = self
            .0
            .settings
            .inventory
            .refresh_threads(connection.clone());
        let _ = self.0.settings.inventory.refresh_projects(connection, true);
        Ok(())
    }
    pub async fn readiness(
        &self,
        connection: InventoryConnection,
        generation: u64,
    ) -> IvyResult<IvyJson> {
        if let Some(environment) = &self.0.settings.environment {
            let refresh = {
                let mut state = self.0.state.lock().map_err(|_| IvyError::internal())?;
                if (self.0.now)() - state.environment_at >= 10_000.0 {
                    state.environment_at = (self.0.now)();
                    true
                } else {
                    false
                }
            };
            if refresh {
                tokio::spawn(environment.synchronize(connection.client.clone()));
            }
        }
        let _ = self.0.settings.inventory.refresh_due(connection);
        let ready = !self.0.settings.owner.closing.load(Ordering::Acquire)
            && self.0.settings.owner.rpc.connected();
        let code = self.0.settings.inventory.code()?;
        let now = (self.0.iso_now)();
        self.0
            .settings
            .health
            .write(
                ready,
                Some(generation),
                if ready {
                    "Native owner available; inventory refresh runs independently."
                } else {
                    "Native owner is unavailable."
                }
                .into(),
            )?
            .wait()
            .await?;
        Ok(readiness_snapshot(
            &self.0.settings.service_node_id,
            ready,
            code,
            now,
        ))
    }
    /// These retain the actual shared ServiceConnection and its generation.
    pub fn dependencies(&self) -> (ConnectionDependency<()>, ConnectionDependency<IvyJson>) {
        let reconcile = self.clone();
        let readiness = self.clone();
        (
            Arc::new(move |connection: ServiceConnection| {
                let owner = reconcile.clone();
                Box::pin(async move {
                    owner
                        .reconcile(InventoryConnection::from_connection(connection))
                        .await
                })
            }),
            Arc::new(move |connection: ServiceConnection| {
                let owner = readiness.clone();
                Box::pin(async move {
                    let generation = connection.generation();
                    owner
                        .readiness(InventoryConnection::from_connection(connection), generation)
                        .await
                })
            }),
        )
    }
}

pub fn readiness_snapshot(
    service_node_id: &JsString,
    ready: bool,
    code: Option<JsString>,
    now: JsString,
) -> IvyJson {
    let has_code = code.as_ref().is_some_and(|code| !code.0.is_empty());
    IvyJson::object([
        ("ready", IvyJson::Bool(ready)),
        (
            "diagnostics",
            IvyJson::Array(vec![IvyJson::object([
                (
                    "code",
                    IvyJson::String(code.unwrap_or_else(|| "native_inventory_unavailable".into())),
                ),
                (
                    "resource",
                    IvyJson::object([("serviceNodeId", IvyJson::String(service_node_id.clone()))]),
                ),
                ("source", IvyJson::String(service_node_id.clone())),
                (
                    "severity",
                    if ready {
                        if has_code { "warning" } else { "info" }
                    } else {
                        "error"
                    }
                    .into(),
                ),
                (
                    "status",
                    if has_code || !ready {
                        "current"
                    } else {
                        "resolved"
                    }
                    .into(),
                ),
                ("firstObservedAt", IvyJson::String(now.clone())),
                ("lastObservedAt", IvyJson::String(now)),
                (
                    "message",
                    if has_code {
                        "Native inventory refresh failed; the prior complete snapshot is retained."
                    } else if ready {
                        "Native owner and inventory are available."
                    } else {
                        "Native owner or complete inventory is unavailable."
                    }
                    .into(),
                ),
            ])]),
        ),
    ])
}
