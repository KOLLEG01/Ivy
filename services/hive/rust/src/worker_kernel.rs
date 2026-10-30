//! Native storage domains bound to their owning database thread.
use crate::{
    kernel::Kernel,
    storage::Notification,
    storage::{BackupTask, Completion, Dispatch, Payload, StorageService},
    worker_actions::WorkerActions,
};
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString};
#[path = "worker_startup.rs"]
mod startup;

pub struct KernelWorker {
    kernel: Option<Kernel>,
    actions: WorkerActions,
    notifications: Vec<Notification>,
    fatal: Option<IvyError>,
}
impl KernelWorker {
    /// Actions and Kernel must come from the same initialized storage owner.
    /// No production runtime, crypto or temporary-root fallback is supplied.
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
    fn publish(&mut self, payload: &Payload) -> IvyResult<()> {
        let kernel = self.kernel.as_mut().ok_or_else(IvyError::internal)?;
        self.actions
            .publish(kernel, payload, |value| self.notifications.push(value))
    }
}
impl StorageService for KernelWorker {
    fn close(&mut self) -> IvyResult<()> {
        self.kernel.take().ok_or_else(IvyError::internal)?.close()
    }
    fn handle(&mut self, payload: Payload) -> IvyResult<IvyJson> {
        if let Some(error) = &self.fatal {
            return Err(error.clone());
        }
        let kernel = self.kernel.as_mut().ok_or_else(IvyError::internal)?;
        let result = self.actions.handle(kernel, &payload);
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
    fn backup(&mut self, destination: JsString) -> IvyResult<BackupTask> {
        if let Some(error) = &self.fatal {
            return Err(error.clone());
        }
        let kernel = self.kernel.as_ref().ok_or_else(IvyError::internal)?;
        let task = kernel.parts.store.backup_to(destination, || Ok(()));
        Ok(Box::pin(async move {
            task.await.map(|snapshot| snapshot.to_json())
        }))
    }
    fn notifications(&mut self) -> Vec<Notification> {
        std::mem::take(&mut self.notifications)
    }
    fn dispatch(&mut self, payload: Payload) -> Dispatch {
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
