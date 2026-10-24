use crate::{
    configuration::Configuration,
    connection::{self, Admission},
    health::HealthReporter,
    shutdown::{Cleanup, Shutdown},
    topology::Owners,
};
use ivy_contracts::{IvyError, IvyJson, IvyResult, core_schema::validate_host};
use ivy_host_runtime::{
    config::{self, InstanceConfig},
    health::HealthFile,
    host_config::host_config,
    journal::HostJournal,
    node_path::to_native,
    node_runtime::NodeRuntime,
    runtime_maintenance::runtime_reset_active,
};
use ivy_sdk::service::ServiceClient;
use std::{path::PathBuf, sync::Arc, time::Duration};
use tokio::sync::{Mutex, watch};

pub struct Manager {
    pub service: ServiceClient,
    configuration: Arc<Configuration>,
    shutdown: Arc<Shutdown>,
}

async fn startup_health(config: InstanceConfig, journal: &HostJournal) -> IvyResult<HealthFile> {
    let health = HealthFile::new(config.clone());
    if health.launch_id().is_some_and(|id| !id.0.is_empty()) {
        return Ok(health);
    }
    // Independently started Windows bootstrap tasks have no launch environment.
    // Use their retained target so health remains bound to the installed launch.
    let source = config.json();
    let launch = if let Some(target) = journal
        .target(&source.required("instanceId")?.as_str()?)
        .await?
    {
        let candidate = journal
            .candidate(&target.required("candidateId")?.as_str()?)
            .await?;
        if target.get("instanceId") != source.get("instanceId")
            || target.get("desired") != Some(&"running".into())
            || ["componentId", "buildId", "artifactRoot"]
                .iter()
                .any(|key| candidate.get(key) != source.get(key))
        {
            return Err(IvyError::new(
                "target_conflict",
                "ServiceManager launch does not match its selected release.",
            ));
        }
        target
            .required("revision")?
            .as_js_string()
            .ok_or_else(IvyError::internal)?
            .clone()
    } else {
        health.boot_id().into()
    };
    Ok(HealthFile::with_launch_id(config, Some(launch)))
}

impl Manager {
    pub async fn from_path(path: PathBuf, node: impl Into<Option<NodeRuntime>>) -> IvyResult<Self> {
        let config = config::native_instance_config(path).await?;
        connection::validate_config(&config)?;
        let host_path = to_native(
            config
                .json()
                .required("settings")?
                .required("hostConfigPath")?
                .as_js_string()
                .ok_or_else(IvyError::internal)?,
        );
        let accepted = host_config(host_path).await?;
        let node = match node.into() {
            Some(node) => Some(node),
            None => {
                let configured = accepted
                    .required("executables")?
                    .get("node")
                    .and_then(IvyJson::as_js_string)
                    .map(to_native);
                NodeRuntime::discover_optional(configured.as_deref()).await?
            }
        };
        let reset_active = runtime_reset_active(&accepted).await?;
        let bootstrap_root = config.artifact_root().to_path_buf();
        Self::start(config, accepted, reset_active, node, bootstrap_root).await
    }
    /// The host loader supplies its normalized accepted HostConfig and actual
    /// runtime-reset observation. InstanceConfig comes from the common loader.
    /// Requiring these inputs keeps host path/default/reset policy in one place.
    pub async fn start(
        config: InstanceConfig,
        accepted: IvyJson,
        reset_active: bool,
        node: impl Into<Option<NodeRuntime>>,
        bootstrap_root: PathBuf,
    ) -> IvyResult<Self> {
        connection::validate_config(&config)?;
        validate_host("HostConfig", &accepted)?;
        if reset_active {
            return Err(IvyError::new(
                "maintenance_active",
                "Service ownership remains stopped for an incomplete runtime reset.",
            ));
        }
        let source = config.json();
        let configured = accepted
            .required("instances")?
            .as_array()
            .ok_or_else(IvyError::internal)?
            .iter()
            .any(|instance| {
                ["instanceId", "serviceNodeId", "componentId"]
                    .iter()
                    .all(|key| instance.get(key) == source.get(key))
            });
        if accepted.get("hostId") != source.get("hostId") || !configured {
            return Err(IvyError::new(
                "target_conflict",
                "ServiceManager is not configured on its claimed local host.",
            ));
        }
        let journal = HostJournal::open(accepted.clone()).await?;
        let health_file = match startup_health(config.clone(), &journal).await {
            Ok(health) => Arc::new(health),
            Err(error) => {
                let _ = journal.close().await;
                return Err(error);
            }
        };
        let health = HealthReporter::new(health_file.clone());
        let admission = Admission::default();
        let owners = Arc::new(Mutex::new(Owners::new(bootstrap_root.clone(), node)));
        let configuration = Arc::new(Configuration {
            path: to_native(
                source
                    .required("settings")?
                    .required("hostConfigPath")?
                    .as_js_string()
                    .ok_or_else(IvyError::internal)?,
            ),
            host_id: source.required("hostId")?.clone(),
            journal: journal.clone(),
            owners: owners.clone(),
            admission: admission.clone(),
            source: Mutex::new(None),
        });
        let reload = configuration.callback(true);
        let refresh = configuration.callback(false);
        let options = connection::options(
            &config,
            journal.clone(),
            health.clone(),
            crate::handlers::mutations(reload.clone(), bootstrap_root),
            admission.clone(),
            refresh.clone(),
        );
        let service = match options.and_then(ServiceClient::new) {
            Ok(value) => value,
            Err(error) => {
                let _ = journal.close().await;
                return Err(error);
            }
        };
        let (control, mut control_signal) = watch::channel(false);
        let cleanup = Cleanup {
            owners: vec![Box::pin({
                let owners = owners.clone();
                async move { owners.lock().await.close().await }
            })],
            stop_service: Box::pin({
                let service = service.clone();
                let admission = admission.clone();
                async move {
                    service.stop().await;
                    admission.drain().await;
                    Ok(())
                }
            }),
            drain_health: Box::pin({
                let health = health.clone();
                async move { health.flush().await }
            }),
            final_health: Box::pin({
                let health = health.clone();
                async move { health.finalize().await }
            }),
            close_journal: Box::pin({
                let journal = journal.clone();
                async move { journal.close().await }
            }),
        };
        let shutdown = Arc::new(Shutdown::new(
            {
                let admission = admission.clone();
                let health = health.clone();
                let control = control.clone();
                move || {
                    admission.stop();
                    health.seal();
                    control.send_replace(true);
                }
            },
            cleanup,
        ));
        let started = async {
            owners
                .lock()
                .await
                .reconcile(if cfg!(windows) { "win32" } else { "linux" }, &accepted)
                .await?;
            health
                .write(false, None, "Connecting and reconciling host state.".into())
                .await
        }
        .await;
        if let Err(error) = started {
            let _ = shutdown.close().await;
            return Err(error);
        }
        tokio::spawn({
            let shutdown = shutdown.clone();
            async move {
                let mut next_refresh = tokio::time::Instant::now() + Duration::from_secs(2);
                loop {
                    tokio::select! { _ = control_signal.changed() => break, _ = tokio::time::sleep(Duration::from_millis(500)) => () }
                    if health_file.control().await.ok().flatten().is_some() {
                        let _ = shutdown.close().await;
                        break;
                    }
                    if tokio::time::Instant::now() >= next_refresh {
                        // Offline local ownership must follow accepted file edits
                        // too. Failed input keeps the prior owners and is retried;
                        // connected report callbacks surface the same error.
                        let _ = refresh().await;
                        next_refresh = tokio::time::Instant::now() + Duration::from_secs(2);
                    }
                }
            }
        });
        service.start();
        Ok(Self {
            service,
            configuration,
            shutdown,
        })
    }
    /// Called after authoritative local configuration adoption; hints never call
    /// this directly. Unchanged owners retain their accepted target policy.
    pub async fn reconcile_accepted(&self, accepted: IvyJson) -> IvyResult<()> {
        self.configuration.adopt(accepted).await
    }
    pub async fn close(&self) -> IvyResult<()> {
        self.shutdown.close().await
    }
    pub async fn completion(&self) -> IvyResult<()> {
        self.shutdown.completion().await
    }
}
