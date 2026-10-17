//! Serialized database work runs off the asynchronous I/O executor. A command is
//! synchronous and finishes before the caller may start network or process work.
use crate::store::CollectorStore;
use ivy_contracts::{IvyError, IvyResult};
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub struct StoreOwner(Arc<Mutex<CollectorStore>>);

impl StoreOwner {
    pub fn new(store: CollectorStore) -> Self {
        Self(Arc::new(Mutex::new(store)))
    }

    pub async fn call<T: Send + 'static>(
        &self,
        work: impl FnOnce(&CollectorStore) -> IvyResult<T> + Send + 'static,
    ) -> IvyResult<T> {
        let owner = self.0.clone();
        tokio::task::spawn_blocking(move || {
            let store = owner.lock().map_err(|_| IvyError::internal())?;
            work(&store)
        })
        .await
        .map_err(|_| IvyError::internal())?
    }
}
