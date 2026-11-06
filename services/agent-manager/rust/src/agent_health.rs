//! Ordered AgentManager health writes retain accepted work and journal warnings.
use crate::{
    native_journal::NativeJournal,
    service_work::{AgentRun, run::Budget},
};
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString};
use ivy_host_runtime::health::{HealthFile, truncate_details};
use ivy_sdk::service::ServiceFuture;
use std::sync::{Arc, Mutex};
pub type HealthRun = AgentRun<()>;
struct Inner {
    file: Arc<HealthFile>,
    journals: Mutex<Vec<Arc<Mutex<NativeJournal>>>>,
    pending: Mutex<Option<HealthRun>>,
    budget: Budget,
}
#[derive(Clone)]
pub struct AgentHealth(Arc<Inner>);
impl AgentHealth {
    pub fn new(file: Arc<HealthFile>, journal: Arc<Mutex<NativeJournal>>) -> Self {
        Self::with_journals(file, vec![journal])
    }
    pub(crate) fn host(file: Arc<HealthFile>) -> Self {
        Self::with_journals(file, Vec::new())
    }
    fn with_journals(file: Arc<HealthFile>, journals: Vec<Arc<Mutex<NativeJournal>>>) -> Self {
        Self(Arc::new(Inner {
            file,
            journals: Mutex::new(journals),
            pending: Mutex::new(None),
            budget: Budget::default(),
        }))
    }
    pub(crate) fn register(&self, journal: Arc<Mutex<NativeJournal>>) -> IvyResult<()> {
        self.0
            .journals
            .lock()
            .map_err(|_| IvyError::internal())?
            .push(journal);
        Ok(())
    }
    /// Capture the warning at admission, then serialize writes even when a
    /// previous write failed. The awaiting caller does not own cancellation.
    pub fn write(
        &self,
        ready: bool,
        generation: Option<u64>,
        message: JsString,
    ) -> IvyResult<HealthRun> {
        let mut message = truncate_details(message);
        for journal in self
            .0
            .journals
            .lock()
            .map_err(|_| IvyError::internal())?
            .iter()
        {
            let status = journal.lock().map_err(|_| IvyError::internal())?.status();
            let before = message.0.len();
            message = health_message(message, &status)?;
            if message.0.len() != before {
                break;
            }
        }
        let message = truncate_details(message);
        let mut pending = self.0.pending.lock().map_err(|_| IvyError::internal())?;
        let input = message.0.capacity() * 2 + 64;
        let budget = self.0.budget.clone();
        let admission = match budget.reserve(true, input) {
            Ok(admission) => admission,
            Err(_) if pending.as_ref().is_some_and(HealthRun::cache_only) => {
                // A completed result retained only for drain must not fill the
                // call limit after its caller has consumed or dropped it.
                *pending = None;
                budget.reserve(true, input)?
            }
            Err(error) => return Err(error),
        };
        let previous = pending.take();
        let (mut sender, run) = HealthRun::channel(admission.call.clone());
        *pending = Some(run.clone());
        drop(pending);
        let file = self.0.file.clone();
        tokio::spawn(async move {
            let _input = admission;
            if let Some(previous) = previous {
                let _ = previous.completed().await;
            }
            let result = file.write(ready, message, generation).await;
            sender.send(budget.finish_unit(result, true));
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
