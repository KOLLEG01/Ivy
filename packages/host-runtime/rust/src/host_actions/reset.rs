use super::HostActions;
use crate::{
    IvyError, IvyJson as J, IvyResult, host_config,
    journal::HostJournal,
    layout::StorageContext,
    node_path,
    node_runtime::NodeRuntime,
    runtime_reset::{self, ResetHost, ResetHostPhase, ResetPhaseArguments},
};
use std::{path::PathBuf, sync::Arc};

impl HostActions {
    async fn reset_configuration(&self, path: PathBuf) -> IvyResult<J> {
        let config = host_config::host_config(path).await?;
        // The local CLI closes its request journal before entering reset. No
        // request connection may survive into writer shutdown or destructive work.
        HostJournal::open_with_clock(config.clone(), self.clock.clone())
            .await?
            .close()
            .await?;
        Ok(config)
    }

    /// The local storage plan only. The service/CLI assembly must additionally
    /// admit actual UI bundles before exposing the public reset preview.
    pub async fn reset_plan(
        &self,
        config_path: PathBuf,
        storage: Arc<StorageContext>,
    ) -> IvyResult<J> {
        let this = self.clone();
        tokio::spawn(async move {
            let config = this.reset_configuration(config_path.clone()).await?;
            runtime_reset::local_runtime_reset_plan(
                config,
                node_path::from_native(&config_path),
                this.distribution,
                storage,
            )
            .await
        })
        .await
        .map_err(|_| IvyError::internal())?
    }

    /// Compose the five existing Host phases with their actual planning,
    /// identity, administrator, stop and recovery owners. Clear and the complete
    /// coordinator require the service's real Hive/UI assembly separately.
    pub async fn reset_host_phase(
        &self,
        config_path: PathBuf,
        phase: ResetHostPhase,
        args: ResetPhaseArguments,
        node: impl Into<Option<NodeRuntime>>,
        storage: Arc<StorageContext>,
    ) -> IvyResult<J> {
        let node = node.into();
        let this = self.clone();
        tokio::spawn(async move {
            let config = this.reset_configuration(config_path.clone()).await?;
            ResetHost::new(this.distribution, this.job_launcher, this.clock)?
                .run_phase(
                    config,
                    node_path::from_native(&config_path),
                    phase,
                    args,
                    node,
                    storage,
                )
                .await
        })
        .await
        .map_err(|_| IvyError::internal())?
    }
}
