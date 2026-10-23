use super::{HostActions, close};
use crate::{
    IvyError, IvyJson as J, IvyResult, host_config,
    journal::HostJournal,
    node_path,
    node_runtime::NodeRuntime,
    preparations,
    prepare::{self, PreparationRuntime},
    source,
    storage_retention::{self, RetentionBindings},
};
use ivy_contracts::JsString;
use std::{path::PathBuf, sync::Arc};

impl HostActions {
    async fn preparation_journal(&self, config_path: PathBuf) -> IvyResult<(J, HostJournal)> {
        let config = host_config::host_config(config_path).await?;
        let journal = HostJournal::open_with_clock(config.clone(), self.clock.clone()).await?;
        Ok((config, journal))
    }

    /// Capture once before handing the fixed snapshot to the preparation owner.
    /// The caller supplies the build capabilities, ordering and transport.
    pub async fn prepare(
        &self,
        config_path: PathBuf,
        component: JsString,
        original: JsString,
        runtime: PreparationRuntime,
    ) -> IvyResult<J> {
        let this = self.clone();
        tokio::spawn(async move {
            let (config, journal) = this.preparation_journal(config_path).await?;
            let result = async {
                for (key, value) in [("component", &component), ("source", &original)] {
                    if value.0.is_empty() {
                        return Err(IvyError::new(
                            "invalid_arguments",
                            format!("Missing --{key}."),
                        ));
                    }
                }
                let paths = node_path::Context::current().map_err(|_| IvyError::internal())?;
                let original = paths.resolve(&[original]);
                let snapshot =
                    source::capture_source(original, config.clone(), this.distribution.clone())
                        .await?;
                prepare::prepare_candidate(snapshot, component, config, this.distribution, runtime)
                    .await
            }
            .await;
            close(journal, result).await
        })
        .await
        .map_err(|_| IvyError::internal())?
    }

    pub async fn preparation_inventory(&self, config_path: PathBuf) -> IvyResult<J> {
        let this = self.clone();
        tokio::spawn(async move {
            let (config, journal) = this.preparation_journal(config_path).await?;
            let result = preparations::preparation_inventory(config).await;
            close(journal, result).await
        })
        .await
        .map_err(|_| IvyError::internal())?
    }

    /// Match the local command's complete inventory and measured reclaimed bytes.
    /// Missing/ambiguous records stay retained by the existing compaction policy.
    pub async fn compact_preparations(
        &self,
        config_path: PathBuf,
        node: impl Into<Option<NodeRuntime>>,
    ) -> IvyResult<J> {
        let node = node.into();
        let this = self.clone();
        tokio::spawn(async move {
            let (config, journal) = this.preparation_journal(config_path).await?;
            let result = preparations::compact_preparations(config, None, true, node).await;
            close(journal, result).await
        })
        .await
        .map_err(|_| IvyError::internal())?
    }

    /// A manual collection returns the removed count and the persisted final
    /// status. The collection owner retains locks and cleanup after observer loss.
    pub async fn collect_storage(
        &self,
        config_path: PathBuf,
        bindings: Arc<RetentionBindings>,
    ) -> IvyResult<J> {
        let this = self.clone();
        tokio::spawn(async move {
            let (config, journal) = this.preparation_journal(config_path).await?;
            let result = async {
                let removed =
                    storage_retention::collect_host_storage(config, None, None, bindings).await?;
                let status = journal
                    .storage_retention_status()
                    .await?
                    .ok_or_else(IvyError::internal)?;
                Ok(J::object([("removed", removed), ("status", status)]))
            }
            .await;
            close(journal, result).await
        })
        .await
        .map_err(|_| IvyError::internal())?
    }
}
