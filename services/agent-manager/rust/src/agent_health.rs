//! Ordered AgentManager health writes retain accepted work and journal warnings.
use crate::native_journal::NativeJournal;
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString};
use ivy_host_runtime::health::HealthFile;
use ivy_sdk::service::ServiceFuture;
use std::sync::{Arc, Mutex};
use tokio::sync::watch;

#[derive(Clone)]
pub struct HealthRun(watch::Receiver<Option<IvyResult<()>>>);
impl HealthRun {
    pub async fn wait(mut self) -> IvyResult<()> {
        loop {
            if let Some(result) = self.0.borrow_and_update().clone() {
                return result;
            }
            self.0.changed().await.map_err(|_| IvyError::internal())?;
        }
    }
}
struct Inner {
    file: Arc<HealthFile>,
    journal: Arc<Mutex<NativeJournal>>,
    pending: Mutex<Option<HealthRun>>,
}
#[derive(Clone)]
pub struct AgentHealth(Arc<Inner>);
impl AgentHealth {
    pub fn new(file: Arc<HealthFile>, journal: Arc<Mutex<NativeJournal>>) -> Self {
        Self(Arc::new(Inner {
            file,
            journal,
            pending: Mutex::new(None),
        }))
    }
    /// Capture the warning at admission, then serialize writes even when a
    /// previous write failed. The awaiting caller does not own cancellation.
    pub fn write(
        &self,
        ready: bool,
        generation: Option<u64>,
        message: JsString,
    ) -> IvyResult<HealthRun> {
        let status = self
            .0
            .journal
            .lock()
            .map_err(|_| IvyError::internal())?
            .status();
        let message = health_message(message, &status)?;
        let mut pending = self.0.pending.lock().map_err(|_| IvyError::internal())?;
        let previous = pending.clone();
        let (sender, receiver) = watch::channel(None);
        let run = HealthRun(receiver);
        *pending = Some(run.clone());
        let file = self.0.file.clone();
        tokio::spawn(async move {
            if let Some(previous) = previous {
                let _ = previous.wait().await;
            }
            let result = file.write(ready, message, generation).await;
            sender.send_replace(Some(result));
        });
        Ok(run)
    }
    /// Like awaiting healthWork, this captures only the last admitted write.
    pub fn drain_snapshot(&self) -> ServiceFuture<()> {
        let pending = self
            .0
            .pending
            .lock()
            .map(|pending| pending.clone())
            .map_err(|_| IvyError::internal());
        Box::pin(async move {
            if let Some(pending) = pending? {
                pending.wait().await?;
            }
            Ok(())
        })
    }
    pub fn file(&self) -> &Arc<HealthFile> {
        &self.0.file
    }
}

pub fn health_message(mut message: JsString, status: &IvyJson) -> IvyResult<JsString> {
    let number = |name| {
        status
            .required(name)?
            .as_f64()
            .ok_or_else(IvyError::internal)
    };
    if number("retained")? >= number("maximum")? * 0.8
        || number("bytes")? >= number("maximumBytes")? * 0.8
    {
        message
            .0
            .extend(" Connection memory near capacity.".encode_utf16());
    }
    Ok(message)
}
