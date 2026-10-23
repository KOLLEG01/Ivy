//! Shared access to one transport journal; no database guard crosses network I/O.
use super::journal_io::{JournalIo, StorageUnavailable};
use ivy_contracts::{IvyJson as Json, IvyResult};
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub struct Journal(Arc<Mutex<Option<JournalIo>>>);
impl Journal {
    pub fn new(io: JournalIo) -> Self {
        Self(Arc::new(Mutex::new(Some(io))))
    }
    pub fn access<T>(&self, action: impl FnOnce(&mut JournalIo) -> IvyResult<T>) -> IvyResult<T> {
        let mut owner = self.0.lock().map_err(|_| StorageUnavailable)?;
        action(owner.as_mut().ok_or(StorageUnavailable)?)
    }
    pub fn get(&self, key: &str) -> IvyResult<Json> {
        self.access(|j| j.get(key))
    }
    pub fn set(&self, key: &str, value: &Json) -> IvyResult<()> {
        self.access(|j| j.set(key, value))
    }
    pub async fn flush(&self) -> IvyResult<()> {
        let journal = self.clone();
        tokio::task::spawn_blocking(move || {
            journal.access(|j| {
                j.flush()?;
                Ok(())
            })
        })
        .await
        .map_err(|_| StorageUnavailable)?
    }
    pub async fn close(&self) -> IvyResult<()> {
        let journal = self.clone();
        tokio::task::spawn_blocking(move || {
            let io = journal.0.lock().map_err(|_| StorageUnavailable)?.take();
            if let Some(io) = io {
                io.close()?;
            }
            Ok(())
        })
        .await
        .map_err(|_| StorageUnavailable)?
    }
}
