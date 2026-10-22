//! Follow the local configuration published by HostExecutor, never a Hive hint.
use crate::{connection::Admission, topology::Owners};
use ivy_contracts::{IvyError, IvyJson, IvyResult};
use ivy_host_runtime::{config, host_config::checked_host_config, journal::HostJournal};
use ivy_sdk::service::ServiceFuture;
use std::{path::PathBuf, sync::Arc};
use tokio::sync::Mutex;

pub type Reload = Arc<dyn Fn() -> ServiceFuture<IvyJson> + Send + Sync>;

pub(crate) struct Configuration {
    pub path: PathBuf,
    pub host_id: IvyJson,
    pub journal: HostJournal,
    pub owners: Arc<Mutex<Owners>>,
    pub admission: Admission,
    // Serialize reload/adoption; cache only a successfully applied input. A failed
    // owner addition is retried without changing the frozen policy of old owners.
    pub source: Mutex<Option<IvyJson>>,
}
impl Configuration {
    async fn apply(&self, next: IvyJson) -> IvyResult<IvyJson> {
        self.admission.check()?;
        if next.get("hostId") != Some(&self.host_id) {
            return Err(IvyError::new(
                "target_conflict",
                "Reloaded host configuration belongs to another host.",
            ));
        }
        self.journal.use_configuration(next.clone()).await?;
        self.admission.check()?;
        self.owners
            .lock()
            .await
            .reconcile(if cfg!(windows) { "win32" } else { "linux" }, &next)
            .await?;
        Ok(next)
    }
    async fn reload(&self, validate_unchanged: bool) -> IvyResult<IvyJson> {
        let mut source = self.source.lock().await;
        self.admission.check()?;
        let input = config::json_file(self.path.clone(), config::CONFIGURATION_BYTES).await?;
        if !validate_unchanged && source.as_ref() == Some(&input) {
            return self.journal.config().await;
        }
        let next = checked_host_config(input.clone(), self.path.clone()).await?;
        let next = self.apply(next).await?;
        *source = Some(input);
        Ok(next)
    }
    pub async fn adopt(&self, next: IvyJson) -> IvyResult<()> {
        let mut source = self.source.lock().await;
        self.apply(next).await?;
        *source = None;
        Ok(())
    }
    pub fn callback(self: &Arc<Self>, validate_unchanged: bool) -> Reload {
        let state = self.clone();
        Arc::new(move || {
            let state = state.clone();
            Box::pin(async move {
                tokio::spawn(async move { state.reload(validate_unchanged).await })
                    .await
                    .map_err(|_| IvyError::internal())?
            })
        })
    }
}
