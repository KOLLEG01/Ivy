//! Same-owner worker binding over initialized native domains. Startup providers,
//! temporary UI ownership and the process/retirement topology stay with startup.
use crate::{
    kernel::Kernel,
    worker_actions::WorkerActions,
    worker_protocol::Notification,
    worker_server::{BackupTask, Completion, Dispatch, Progress, WorkerService},
};
use ivy_contracts::{IvyError, IvyJson, IvyResult};

pub struct KernelWorker {
    kernel: Option<Kernel>,
    actions: WorkerActions,
    notifications: Vec<Notification>,
    fatal: Option<IvyError>,
}
impl KernelWorker {
    /// Actions and Kernel must come from the same initialized storage owner.
    /// No production runtime, crypto, retention or temporary-root fallback is
    /// supplied; the unfinished retention action remains unbound in Actions.
    pub fn new(kernel: Kernel, actions: WorkerActions) -> Self {
        Self {
            kernel: Some(kernel),
            actions,
            notifications: vec![],
            fatal: None,
        }
    }
    pub fn kernel(&self) -> Option<&Kernel> {
        self.kernel.as_ref()
    }
    pub fn kernel_mut(&mut self) -> Option<&mut Kernel> {
        self.kernel.as_mut()
    }
    fn publish(&mut self, payload: &IvyJson) -> IvyResult<()> {
        let kernel = self.kernel.as_mut().ok_or_else(IvyError::internal)?;
        self.actions
            .publish(kernel, payload, |value| self.notifications.push(value))
    }
    fn close(&mut self) -> IvyResult<IvyJson> {
        let mut kernel = self.kernel.take().ok_or_else(IvyError::internal)?;
        let changes = kernel.parts.store.take_changes();
        let result = kernel
            .close()
            .map(|_| IvyJson::object([("closed", true.into())]));
        // The original handle closes SQLite before publishing events. Changes
        // can still be posted, but both subsequent head queries fail and reject
        // the chain without a close reply. Preserve this observed behavior;
        // correcting it requires a separate compatibility decision.
        if !changes.is_empty() {
            self.notifications.push(Notification::Changes(changes));
        }
        self.fatal = Some(IvyError::internal());
        result
    }
}
impl WorkerService for KernelWorker {
    fn handle(&mut self, payload: IvyJson) -> IvyResult<IvyJson> {
        if let Some(error) = &self.fatal {
            return Err(error.clone());
        }
        if payload.get("action") == Some(&IvyJson::from("close")) {
            return self.close();
        }
        let kernel = self.kernel.as_mut().ok_or_else(IvyError::internal)?;
        let result = self
            .actions
            .handle(kernel, &payload)
            .unwrap_or_else(|| Err(IvyError::internal()));
        match result {
            Ok(value) => match self.publish(&payload) {
                Ok(()) => Ok(value),
                Err(error) => {
                    if let Err(fatal) = self.publish(&payload) {
                        self.fatal = Some(fatal);
                    }
                    Err(error)
                }
            },
            Err(error) => {
                if let Err(fatal) = self.publish(&payload) {
                    self.fatal = Some(fatal);
                }
                Err(error)
            }
        }
    }
    fn backup(&mut self, payload: IvyJson, progress: Progress) -> IvyResult<BackupTask> {
        if let Some(error) = &self.fatal {
            return Err(error.clone());
        }
        let kernel = self.kernel.as_ref().ok_or_else(IvyError::internal)?;
        let destination = payload
            .get("destination")
            .and_then(IvyJson::as_js_string)
            .cloned()
            .ok_or_else(IvyError::internal)?;
        let task = kernel.parts.store.backup_to(destination, move || {
            progress();
            Ok(())
        });
        Ok(Box::pin(async move {
            task.await.map(|snapshot| snapshot.to_json())
        }))
    }
    fn notifications(&mut self) -> Vec<Notification> {
        std::mem::take(&mut self.notifications)
    }
    fn dispatch(&mut self, payload: IvyJson) -> Dispatch {
        let result = self.handle(payload);
        Dispatch {
            notifications: self.notifications(),
            completion: match &self.fatal {
                Some(error) => Completion::Failed(error.clone()),
                None => Completion::Reply(result),
            },
        }
    }
}
