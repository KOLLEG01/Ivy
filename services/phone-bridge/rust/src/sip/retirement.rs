//! One host owns uncertain originals. Capacity remains charged until actual
//! retirement; failure never authorizes another operation or frees its binding.
use super::{host::Shared, incoming::IncomingActor};
use rsipstack::transaction::Transaction;
use std::sync::{Arc, Mutex};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

pub(super) const RESPONSE_OWNERS: usize = 16;
// At most 32 public calls, with room for their original ACK retirement work.
const INCOMING_OWNERS: usize = 64;

pub(super) struct Retirements {
    responses: Arc<Semaphore>,
    incoming: Arc<Semaphore>,
    failed_responses: Mutex<Vec<(Transaction, OwnedSemaphorePermit)>>,
    failed_dialogs: Mutex<Vec<Transaction>>,
    failed_incoming: Mutex<Vec<Box<IncomingActor>>>,
    failed_host: Mutex<Option<Shared>>,
}
impl Retirements {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            responses: Arc::new(Semaphore::new(RESPONSE_OWNERS)),
            incoming: Arc::new(Semaphore::new(INCOMING_OWNERS)),
            failed_responses: Mutex::new(Vec::new()),
            failed_dialogs: Mutex::new(Vec::new()),
            failed_incoming: Mutex::new(Vec::new()),
            failed_host: Mutex::new(None),
        })
    }
    pub fn response_slot(&self) -> Option<OwnedSemaphorePermit> {
        self.responses.clone().try_acquire_owned().ok()
    }
    pub fn incoming_slot(&self) -> Option<OwnedSemaphorePermit> {
        self.incoming.clone().try_acquire_owned().ok()
    }
    pub fn response_failed(&self, original: Transaction, slot: OwnedSemaphorePermit) {
        self.failed_responses.lock().unwrap().push((original, slot));
    }
    pub fn dialog_failed(&self, original: Transaction) {
        // Each admitted incoming owner has at most its initial exchange and
        // four accepted remote offers. Its lease survives an uncertain result.
        self.failed_dialogs.lock().unwrap().push(original);
    }
    pub fn incoming_failed(&self, original: IncomingActor) {
        self.failed_incoming
            .lock()
            .unwrap()
            .push(Box::new(original));
    }
    pub fn host_failed(&self, owner: Shared) {
        // The host and its retirement book retain each other after failed
        // close. This explicit native-exit fence needs no suspended task.
        *self.failed_host.lock().unwrap() = Some(owner);
    }
    pub fn unresolved(&self) -> bool {
        !self.failed_responses.lock().unwrap().is_empty()
            || !self.failed_dialogs.lock().unwrap().is_empty()
            || !self.failed_incoming.lock().unwrap().is_empty()
    }
    #[cfg(test)]
    pub fn usage(&self) -> (usize, usize, usize, usize, usize, bool) {
        (
            RESPONSE_OWNERS - self.responses.available_permits(),
            INCOMING_OWNERS - self.incoming.available_permits(),
            self.failed_responses.lock().unwrap().len(),
            self.failed_dialogs.lock().unwrap().len(),
            self.failed_incoming.lock().unwrap().len(),
            self.failed_host.lock().unwrap().is_some(),
        )
    }
}
