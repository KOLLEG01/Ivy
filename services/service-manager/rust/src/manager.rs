use crate::{
    connection::{self, Admission},
    health::HealthReporter,
    shutdown::{Cleanup, Shutdown},
    topology::Owners,
};
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString, core_schema::validate_host};
use ivy_host_runtime::{
    config::InstanceConfig, health::HealthFile, journal::HostJournal, node_runtime::NodeRuntime,
};
use ivy_sdk::service::{ServiceClient, ToolHandler};
use std::{collections::HashMap, path::PathBuf, sync::Arc, time::Duration};
use tokio::sync::{Mutex, watch};

pub struct Manager {
    pub service: ServiceClient,
    journal: HostJournal,
    owners: Arc<Mutex<Owners>>,
    admission: Admission,
    shutdown: Arc<Shutdown>,
}
impl Manager {
    /// The host loader supplies its normalized accepted HostConfig and actual
    /// runtime-reset observation. InstanceConfig comes from the common loader.
    /// Requiring these inputs keeps host path/default/reset policy in one place.
    pub async fn start(
        config: InstanceConfig,
        accepted: IvyJson,
        reset_active: bool,
        node: NodeRuntime,
        bootstrap_root: PathBuf,
        mutations: HashMap<JsString, ToolHandler>,
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
        let health_file = Arc::new(HealthFile::new(config.clone()));
        let health = HealthReporter::new(health_file.clone());
        let admission = Admission::default();
        let options = connection::options(
            &config,
            journal.clone(),
            health.clone(),
            mutations,
            admission.clone(),
        );
        let service = match options.and_then(ServiceClient::new) {
            Ok(value) => value,
            Err(error) => {
                let _ = journal.close().await;
                return Err(error);
            }
        };
        let owners = Arc::new(Mutex::new(Owners::new(bootstrap_root, node)));
        let (control, mut control_signal) = watch::channel(false);
        let cleanup = Cleanup {
            owners: vec![Box::pin({
                let owners = owners.clone();
                async move { owners.lock().await.close().await }
            })],
            stop_service: Box::pin({
                let service = service.clone();
                async move {
                    service.stop().await;
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
                loop {
                    tokio::select! { _ = control_signal.changed() => break, _ = tokio::time::sleep(Duration::from_millis(500)) => () }
                    if health_file.control().await.ok().flatten().is_some() {
                        let _ = shutdown.close().await;
                        break;
                    }
                }
            }
        });
        service.start();
        Ok(Self {
            service,
            journal,
            owners,
            admission,
            shutdown,
        })
    }
    /// Called after authoritative local configuration adoption; hints never call
    /// this directly. Unchanged owners retain their frozen legacy fallback.
    pub async fn reconcile_accepted(&self, accepted: IvyJson) -> IvyResult<()> {
        self.admission.check()?;
        self.journal.use_configuration(accepted.clone()).await?;
        self.admission.check()?;
        self.owners
            .lock()
            .await
            .reconcile(if cfg!(windows) { "win32" } else { "linux" }, &accepted)
            .await
    }
    pub async fn close(&self) -> IvyResult<()> {
        self.shutdown.close().await
    }
}
