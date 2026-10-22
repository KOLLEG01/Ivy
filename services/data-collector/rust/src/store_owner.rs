//! Serialized database work runs off the asynchronous I/O executor. A command is
//! synchronous and finishes before the caller may start network or process work.
use crate::store::CollectorStore;
use ivy_contracts::{IvyError, IvyResult};
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub struct StoreOwner(Arc<Mutex<Option<CollectorStore>>>);

impl StoreOwner {
    pub fn new(store: CollectorStore) -> Self {
        Self(Arc::new(Mutex::new(Some(store))))
    }

    pub async fn call<T: Send + 'static>(
        &self,
        work: impl FnOnce(&CollectorStore) -> IvyResult<T> + Send + 'static,
    ) -> IvyResult<T> {
        let owner = self.0.clone();
        tokio::task::spawn_blocking(move || {
            let store = owner.lock().map_err(|_| IvyError::internal())?;
            work(store.as_ref().ok_or_else(|| {
                IvyError::new("service_unavailable", "Collector storage is closed.")
            })?)
        })
        .await
        .map_err(|_| IvyError::internal())?
    }

    /// Retire the connection after the lifecycle has drained accepted work.
    /// Cloned owners cannot keep the SQLite writer open after shutdown.
    pub async fn close(&self) -> IvyResult<()> {
        let owner = self.0.clone();
        tokio::task::spawn_blocking(move || {
            owner.lock().map_err(|_| IvyError::internal())?.take();
            Ok(())
        })
        .await
        .map_err(|_| IvyError::internal())?
    }
}
