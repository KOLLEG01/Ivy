//! Production service composition. The local reconciliation owner retains its
//! journal work; HostExecutor supplies instance, health and control file I/O.
use crate::{
    assignment_prompt::load_notification_policy,
    config::{
        load_minimum_message_age_minutes, load_triage_configuration, service_configuration_path,
        startup_settings,
    },
    generation::ServiceEvent,
    reconcile::{ReadinessState, ReconcileConfig, Reconciliation},
    registry::service_registry,
    service_handlers::registered_handlers,
};
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString};
use ivy_host_runtime::{
    config::{InstanceConfig, native_instance_config},
    health::HealthFile,
};
use ivy_sdk::{
    clock::{Clock, SystemClock, date_to_iso},
    service::{ServiceClient, ServiceOptions},
};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::{
    sync::{mpsc, oneshot, watch},
    task::JoinHandle,
    time::{Instant, MissedTickBehavior},
};

/// Retained native catalogs occupy the same dist/specs path as the original
/// compiled loader. The instance's artifact root also contains non-dist data.
pub fn retained_native_contracts(
    artifact_root: &Path,
) -> ivy_contracts::checked_native_contract::CheckedNativeContracts {
    ivy_contracts::checked_native_contract::CheckedNativeContracts::new(artifact_root.join("dist"))
}

/// Select the preflight helper without requiring unused workloads at startup.
/// Host process admission checks the file when an assignment actually runs it.
pub fn packaged_job_launcher(artifact_root: &Path) -> IvyResult<Option<PathBuf>> {
    #[cfg(windows)]
    {
        let target = match std::env::consts::ARCH {
            "x86_64" => "win32-x64",
            "aarch64" => "win32-arm64",
            _ => {
                return Err(IvyError::new(
                    "unsupported_platform",
                    "Unsupported Windows architecture.",
                ));
            }
        };
        Ok(Some(
            artifact_root
                .join("dist/native")
                .join(target)
                .join("ivy-job.exe"),
        ))
    }
    #[cfg(not(windows))]
    {
        let _ = artifact_root;
        Ok(None)
    }
}

enum HealthUpdate {
    Write {
        ready: bool,
        details: JsString,
        generation: Option<u64>,
        reply: oneshot::Sender<IvyResult<()>>,
    },
    Finish(oneshot::Sender<IvyResult<()>>),
}
#[derive(Clone)]
struct HealthUpdates(mpsc::UnboundedSender<HealthUpdate>);
impl HealthUpdates {
    fn enqueue(
        &self,
        ready: bool,
        details: &str,
        generation: Option<u64>,
    ) -> IvyResult<oneshot::Receiver<IvyResult<()>>> {
        let (reply, receiver) = oneshot::channel();
        self.0
            .send(HealthUpdate::Write {
                ready,
                details: details.into(),
                generation,
                reply,
            })
            .map_err(|_| IvyError::internal())?;
        Ok(receiver)
    }
    async fn write(&self, ready: bool, details: &str, generation: Option<u64>) -> IvyResult<()> {
        self.enqueue(ready, details, generation)?
            .await
            .map_err(|_| IvyError::internal())?
    }
    async fn finish(&self) -> IvyResult<()> {
        let (reply, receiver) = oneshot::channel();
        self.0
            .send(HealthUpdate::Finish(reply))
            .map_err(|_| IvyError::internal())?;
        receiver.await.map_err(|_| IvyError::internal())?
    }
}
async fn health_updates(
    health: Arc<HealthFile>,
    mut receiver: mpsc::UnboundedReceiver<HealthUpdate>,
) {
    while let Some(update) = receiver.recv().await {
        match update {
            HealthUpdate::Write {
                ready,
                details,
                generation,
                reply,
            } => {
                // Each admitted write survives a dropped observer. A failed
                // write does not prevent the next accepted update from running.
                let result = health.write(ready, details, generation).await;
                let _ = reply.send(result);
            }
            HealthUpdate::Finish(reply) => {
                let result = health.write(false, "Secretary stopped.".into(), None).await;
                let _ = reply.send(result);
                return;
            }
        }
    }
}

#[derive(Clone)]
pub struct SecretaryService {
    pub service: ServiceClient,
    pub readiness: ReadinessState,
    close: mpsc::UnboundedSender<()>,
    completion: watch::Receiver<Option<IvyResult<()>>>,
}
impl SecretaryService {
    /// Closing remains owned by the local task when a caller abandons its wait.
    pub async fn close(&self) -> IvyResult<()> {
        let _ = self.close.send(());
        self.closed().await
    }
    pub async fn closed(&self) -> IvyResult<()> {
        let mut receiver = self.completion.clone();
        loop {
            let result = receiver.borrow_and_update().clone();
            if let Some(result) = result {
                return result;
            }
            receiver.changed().await.map_err(|_| IvyError::internal())?;
        }
    }
}

fn connection_diagnostic(code: JsString) {
    let value = IvyJson::object([
        ("code", IvyJson::String(code)),
        ("phase", "hive-connection".into()),
    ]);
    if let Ok(line) = ivy_contracts::json::canonical(&value, usize::MAX) {
        eprintln!("{line}");
    }
}
pub(crate) fn service_identity(value: &IvyJson) -> IvyResult<IvyJson> {
    Ok(IvyJson::object([
        ("serviceNodeId", value.required("serviceNodeId")?.clone()),
        ("serviceName", "secretary".into()),
        ("instanceMode", "singleton".into()),
        ("hostId", value.required("hostId")?.clone()),
        ("version", value.required("version")?.clone()),
        ("buildId", value.required("buildId")?.clone()),
        ("hiveProtocol", IvyJson::Number(1.0)),
    ]))
}
async fn shutdown_owner(
    service: ServiceClient,
    actor: JoinHandle<()>,
    events: mpsc::UnboundedSender<ServiceEvent>,
    health: Arc<HealthFile>,
    writes: HealthUpdates,
    health_worker: JoinHandle<()>,
    mut close: mpsc::UnboundedReceiver<()>,
    completion: watch::Sender<Option<IvyResult<()>>>,
) {
    let interval = Duration::from_millis(500);
    let mut control = tokio::time::interval_at(Instant::now() + interval, interval);
    control.set_missed_tick_behavior(MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            biased;
            _ = close.recv() => break,
            _ = control.tick() => {
                let checked = tokio::select! {
                    biased;
                    _ = close.recv() => break,
                    result = health.control() => result,
                };
                if matches!(checked, Ok(Some(_))) { break; }
            }
        }
    }
    service.stop().await;
    let _ = events.send(ServiceEvent::Stop);
    let joined = actor.await.map_err(|_| IvyError::internal());
    let stopped = writes.finish().await;
    let health_joined = health_worker.await.map_err(|_| IvyError::internal());
    completion.send_replace(Some(joined.and(stopped).and(health_joined)));
}

/// Call within a running LocalSet. Scheduling and inbox APIs are native and do
/// not need the Node runtime used by optional JavaScript workloads.
pub async fn start(path: impl Into<PathBuf>) -> IvyResult<SecretaryService> {
    let config = native_instance_config(path).await?;
    let job_launcher = packaged_job_launcher(config.artifact_root())?;
    start_configured(config, job_launcher).await
}
async fn start_configured(
    config: InstanceConfig,
    job_launcher: Option<PathBuf>,
) -> IvyResult<SecretaryService> {
    let value = config.json();
    let settings = startup_settings(value)?.clone();
    let data_root = value
        .required("dataRoot")?
        .as_js_string()
        .ok_or_else(IvyError::internal)?;
    let service_path = PathBuf::from(service_configuration_path(data_root)?.to_utf8()?);
    let minimum_message_age_minutes =
        load_minimum_message_age_minutes(service_path.clone()).await?;
    // Loading validates configured dormant capability without starting it or
    // broadening the service's active registration.
    let _triage = load_triage_configuration(service_path).await?;
    let service_node_id = value
        .required("serviceNodeId")?
        .as_js_string()
        .cloned()
        .ok_or_else(IvyError::internal)?;
    let root = settings
        .required("identity")?
        .required("scope")?
        .required("rootObjectId")?
        .clone();
    let registry = service_registry(&root)?;
    let readiness = ReadinessState::default();
    let owner = Reconciliation {
        readiness: readiness.clone(),
        config: ReconcileConfig {
            settings,
            service_node_id: service_node_id.clone(),
            host_id: value
                .required("hostId")?
                .as_js_string()
                .cloned()
                .ok_or_else(IvyError::internal)?,
            data_root: config.data_root().into(),
            distribution_root: config.artifact_root().into(),
            job_launcher,
            minimum_message_age_minutes,
            notification_policy: load_notification_policy(&config.artifact_root().join("dist"))?,
        },
    };
    let identity = service_identity(value)?;
    let credential = value.required("credential")?.as_str()?;
    let endpoint = value.required("publicBaseUrl")?.as_str()?;
    let health = Arc::new(HealthFile::new(config));
    let (updates, pending_updates) = mpsc::unbounded_channel();
    let writes = HealthUpdates(updates);
    let (invocations, mut pending_invocations) = mpsc::unbounded_channel();
    let (events, mut pending_events) = mpsc::unbounded_channel();
    let reconcile_events = events.clone();
    let mut options = ServiceOptions::new(
        endpoint,
        Arc::new(move || {
            let credential = credential.clone();
            Box::pin(async move { Ok(credential) })
        }),
        identity,
        Arc::new(move || {
            let registry = registry.clone();
            Box::pin(async move { Ok(registry) })
        }),
        Arc::new(move |connection| {
            let result = reconcile_events
                .send(ServiceEvent::Reconcile(connection))
                .map_err(|_| IvyError::internal());
            Box::pin(async move { result })
        }),
    );
    options.heartbeat = Duration::from_millis(2000);
    options.handlers = registered_handlers(invocations);
    let observed_state = readiness.clone();
    let readiness_writes = writes.clone();
    options.readiness = Some(Arc::new(move |connection| {
        let state = observed_state.current();
        let generation = connection.generation();
        let (ready, detail) = state.health_state(generation);
        let pending = readiness_writes.enqueue(ready, detail, Some(generation));
        let node = service_node_id.clone();
        Box::pin(async move {
            pending?.await.map_err(|_| IvyError::internal())??;
            // The timestamp follows the completed health write, while the
            // ready/issue decision remains the callback's original snapshot.
            let at =
                date_to_iso(SystemClock.utc_millis() as f64).map_err(|_| IvyError::internal())?;
            Ok(state.observe(generation, &node, &at).heartbeat)
        })
    }));
    let state_events = events.clone();
    let state_writes = writes.clone();
    options.on_state = Some(Arc::new(move |state| {
        if matches!(state.status, "offline" | "stopped" | "connecting") {
            if state.status == "offline" {
                if let Some(code) = &state.code {
                    connection_diagnostic(code.clone());
                }
            }
            let _ = state_writes.enqueue(
                false,
                "Secretary Hive connection is unavailable; original actions remain saved.",
                None,
            );
        }
        let _ = state_events.send(ServiceEvent::State(state));
    }));
    let service = ServiceClient::new(options)?;
    let health_worker = tokio::spawn(health_updates(health.clone(), pending_updates));
    let actor = tokio::task::spawn_local(async move {
        owner
            .run(&mut pending_invocations, &mut pending_events)
            .await;
    });
    let (close, close_requests) = mpsc::unbounded_channel();
    let (complete, completion) = watch::channel(None);
    tokio::task::spawn_local(shutdown_owner(
        service.clone(),
        actor,
        events,
        health,
        writes.clone(),
        health_worker,
        close_requests,
        complete,
    ));
    let running = SecretaryService {
        service,
        readiness,
        close,
        completion,
    };
    if let Err(error) = writes
        .write(
            false,
            "Connecting Secretary and recovering original actions.",
            None,
        )
        .await
    {
        running.close().await?;
        return Err(error);
    }
    running.service.start();
    Ok(running)
}
