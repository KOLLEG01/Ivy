use super::{HostActions, close};
use crate::{
    IvyError, IvyJson as J, IvyResult, bootstrap_maintenance, config, host_config,
    journal::HostJournal, node_path,
};
use ivy_contracts::JsString;
use std::path::PathBuf;

impl HostActions {
    async fn bootstrap_journal(&self, path: PathBuf) -> IvyResult<(J, HostJournal)> {
        let config = host_config::host_config(path).await?;
        let journal = HostJournal::open_with_clock(config.clone(), self.clock.clone()).await?;
        Ok((config, journal))
    }

    pub async fn inspect_bootstrap(&self, config_path: PathBuf) -> IvyResult<J> {
        let this = self.clone();
        tokio::spawn(async move {
            let (_, journal) = this.bootstrap_journal(config_path.clone()).await?;
            let result = this.maintenance.inspect(config_path).await;
            close(journal, result).await
        })
        .await
        .map_err(|_| IvyError::internal())?
    }

    /// The request file is read after the local command's configuration/journal
    /// admission, before the maintenance owner admits its exact transaction.
    pub async fn update_bootstrap(
        &self,
        config_path: PathBuf,
        request_path: JsString,
    ) -> IvyResult<J> {
        let this = self.clone();
        tokio::spawn(async move {
            let (_, journal) = this.bootstrap_journal(config_path.clone()).await?;
            let result = async {
                required(&request_path, "request")?;
                let paths = node_path::Context::current().map_err(|_| IvyError::internal())?;
                let path = node_path::to_native(&paths.resolve(&[request_path]));
                let request = config::json_file(path, config::CONFIGURATION_BYTES).await?;
                this.maintenance.update(config_path, request).await
            }
            .await;
            close(journal, result).await
        })
        .await
        .map_err(|_| IvyError::internal())?
    }

    pub async fn bootstrap_status(
        &self,
        config_path: PathBuf,
        operation: JsString,
    ) -> IvyResult<J> {
        let this = self.clone();
        tokio::spawn(async move {
            let (config, journal) = this.bootstrap_journal(config_path).await?;
            let result = async {
                required(&operation, "operation-id")?;
                bootstrap_maintenance::bootstrap_status(&config, &operation).await
            }
            .await;
            close(journal, result).await
        })
        .await
        .map_err(|_| IvyError::internal())?
    }

    /// Returns the installer's complete CliOutput. The caller keeps the existing
    /// output/exit policy rather than wrapping this result as ordinary data.
    pub async fn install_bootstrap(
        &self,
        config_path: PathBuf,
        candidate: JsString,
    ) -> IvyResult<J> {
        let this = self.clone();
        tokio::spawn(async move {
            let (_, journal) = this.bootstrap_journal(config_path.clone()).await?;
            let result = async {
                required(&candidate, "candidate")?;
                let plan = this
                    .installation
                    .plan(config_path, candidate, None, &[])
                    .await?;
                this.installation.install(plan, false).await
            }
            .await;
            close(journal, result).await
        })
        .await
        .map_err(|_| IvyError::internal())?
    }
}

fn required(value: &JsString, key: &str) -> IvyResult<()> {
    if value.0.is_empty() {
        Err(IvyError::new(
            "invalid_arguments",
            format!("Missing --{key}."),
        ))
    } else {
        Ok(())
    }
}
