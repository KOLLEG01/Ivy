//! Shared admission for retained call inputs, unread replies and action caches.
//! This accounts owned boundary data, not the allocator's transient working heap.
use ivy_contracts::{IvyError, IvyJson as Json, IvyResult, Outcome};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};

pub const MAX_WORK_ITEMS: usize = 128;
pub const MAX_RETAINED_BYTES: usize = 64 * 1024 * 1024;

#[derive(Default)]
struct Usage {
    calls: usize,
    bytes: usize,
}

#[derive(Clone, Default)]
pub struct WorkBudget(Arc<Mutex<Usage>>);

pub(crate) fn limit() -> IvyError {
    IvyError::new(
        "limit_exceeded",
        "TaskBoard invocation capacity is exhausted; reconcile any accepted original action.",
    )
}

impl WorkBudget {
    pub fn retained_usage(&self) -> (usize, usize) {
        let usage = self.0.lock().unwrap();
        (usage.calls, usage.bytes)
    }

    fn reserve(&self, bytes: usize, call: bool) -> IvyResult<Arc<Reservation>> {
        let mut usage = self.0.lock().unwrap();
        if (call && usage.calls >= MAX_WORK_ITEMS)
            || bytes > MAX_RETAINED_BYTES.saturating_sub(usage.bytes)
        {
            return Err(limit());
        }
        usage.calls += usize::from(call);
        usage.bytes += bytes;
        Ok(Arc::new(Reservation {
            budget: self.clone(),
            bytes: AtomicUsize::new(bytes),
            call,
        }))
    }

    pub(crate) fn call(&self, bytes: usize) -> IvyResult<Arc<Reservation>> {
        self.reserve(bytes, true)
    }

    pub(crate) fn cache(&self, bytes: usize) -> IvyResult<Arc<Reservation>> {
        self.reserve(bytes, false)
    }
}

/// Owners and their reply observer share one charge until the last owner drops.
pub(crate) struct Reservation {
    budget: WorkBudget,
    bytes: AtomicUsize,
    call: bool,
}

impl Reservation {
    pub(crate) fn bytes(&self) -> usize {
        self.bytes.load(Ordering::Relaxed)
    }

    pub(crate) fn resize(&self, bytes: usize) -> IvyResult<()> {
        let mut usage = self.budget.0.lock().unwrap();
        let remaining = usage.bytes - self.bytes();
        if bytes > MAX_RETAINED_BYTES.saturating_sub(remaining) {
            return Err(limit());
        }
        usage.bytes = remaining + bytes;
        self.bytes.store(bytes, Ordering::Relaxed);
        Ok(())
    }

    pub(crate) fn reply(&self, result: IvyResult<Json>, overhead: usize) -> IvyResult<Json> {
        let bytes = overhead.saturating_add(match &result {
            Ok(value) => value.retained_bytes(),
            Err(error) => error.retained_bytes(),
        });
        if self.resize(bytes).is_err() {
            // The original handler may have committed. Its durable outcome
            // remains authoritative; capacity loss cannot grant a fresh retry.
            Err(limit().with_outcome(Outcome::Unknown))
        } else {
            result
        }
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        let mut usage = self.budget.0.lock().unwrap();
        usage.calls -= usize::from(self.call);
        usage.bytes -= self.bytes();
    }
}
