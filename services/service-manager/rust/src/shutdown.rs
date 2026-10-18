//! Shutdown owns its cleanup task, so cancellation of a caller cannot interrupt it.
use ivy_contracts::IvyResult;
use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
};
use tokio::sync::watch;

pub type CleanupAction = Pin<Box<dyn Future<Output = IvyResult<()>> + Send + 'static>>;

/// The resource owner supplies lazy actions. Owner order is construction order.
pub struct Cleanup {
    pub owners: Vec<CleanupAction>,
    pub stop_service: CleanupAction,
    pub drain_health: CleanupAction,
    pub final_health: CleanupAction,
    pub close_journal: CleanupAction,
}

impl Cleanup {
    async fn run(self) -> IvyResult<()> {
        let mut first_error = None;
        for action in self.owners.into_iter().rev().chain([
            self.stop_service,
            self.drain_health,
            self.final_health,
            self.close_journal,
        ]) {
            if let Err(error) = action.await {
                first_error.get_or_insert(error);
            }
        }
        first_error.map_or(Ok(()), Err)
    }
}

type Start = Box<dyn FnOnce() + Send + 'static>;

pub struct Shutdown {
    pending: Mutex<Option<(Start, Cleanup)>>,
    result: watch::Sender<Option<Arc<IvyResult<()>>>>,
}

impl Shutdown {
    /// `stop_admission` synchronously fences new work and cancels the control timer.
    /// It must not panic. Cleanup actions report failures through their Result.
    pub fn new(stop_admission: impl FnOnce() + Send + 'static, cleanup: Cleanup) -> Self {
        let (result, _) = watch::channel(None);
        Self {
            pending: Mutex::new(Some((Box::new(stop_admission), cleanup))),
            result,
        }
    }

    pub async fn close(&self) -> IvyResult<()> {
        {
            let mut pending = self.pending.lock().unwrap_or_else(|p| p.into_inner());
            if let Some((stop_admission, cleanup)) = pending.take() {
                stop_admission();
                let result = self.result.clone();
                tokio::spawn(async move {
                    result.send_replace(Some(Arc::new(cleanup.run().await)));
                });
            }
        }
        self.completion().await
    }

    /// Wait for control-file or signal-initiated cleanup without starting it.
    pub async fn completion(&self) -> IvyResult<()> {
        let mut receiver = self.result.subscribe();
        loop {
            if let Some(result) = receiver.borrow_and_update().clone() {
                return (*result).clone();
            }
            // The sender in self remains alive for the lifetime of this borrow.
            receiver
                .changed()
                .await
                .expect("shutdown result sender is alive");
        }
    }
}
