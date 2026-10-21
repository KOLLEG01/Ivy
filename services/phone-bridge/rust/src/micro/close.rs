//! A device owns its one started disposal, independently of observers.
use super::MicroInputResult;
use std::{future::Future, sync::Arc};
use tokio::sync::watch;

pub type MicroCloseResult = Result<(), Arc<dyn std::error::Error + Send + Sync>>;
#[derive(Clone)]
pub struct MicroClose(watch::Receiver<Option<MicroCloseResult>>);
impl MicroClose {
    pub fn completed(result: MicroCloseResult) -> Self {
        Self(watch::channel(Some(result)).1)
    }
    pub fn start(
        work: impl Future<Output = MicroCloseResult> + Send + 'static,
    ) -> MicroInputResult<Self> {
        let runtime = tokio::runtime::Handle::try_current()?;
        let (result, receiver) = watch::channel(None);
        runtime.spawn(async move {
            let outcome = tokio::spawn(work).await.unwrap_or_else(|_| Err(unknown()));
            result.send_replace(Some(outcome));
        });
        Ok(Self(receiver))
    }
    pub fn result(&self) -> Option<MicroCloseResult> {
        self.0.borrow().clone()
    }
    pub fn same_work(&self, other: &Self) -> bool {
        self.0.same_channel(&other.0)
    }
    pub async fn wait(mut self) -> MicroCloseResult {
        loop {
            if let Some(result) = self.result() {
                return result;
            }
            if self.0.changed().await.is_err() {
                return Err(unknown());
            }
        }
    }
}
fn unknown() -> Arc<dyn std::error::Error + Send + Sync> {
    Arc::new(std::io::Error::other("Micro cleanup outcome unknown."))
}
