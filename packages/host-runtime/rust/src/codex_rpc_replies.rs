//! Accepted requests retain count admission through their original receiver.
//! Reply payloads have a separate bounded lifetime after durable resolution.
use ivy_contracts::{IvyError, IvyJson, IvyResult, Outcome};
use std::sync::Arc;
use tokio::sync::{OwnedSemaphorePermit, Semaphore, oneshot};

const REPLY_BYTES: usize = 64 * 1024 * 1024;
const CONTROL_BYTES: usize = 8 * 1024 * 1024;
pub(super) type Call = OwnedSemaphorePermit;
pub(super) type Answer = oneshot::Sender<Completion>;
pub(super) struct Completion {
    pub result: IvyResult<IvyJson>,
    _call: Option<Call>,
    _bytes: Option<OwnedSemaphorePermit>,
}
impl Completion {
    /// Pre-admission failures own no native call or retained reply payload.
    pub fn rejected(error: IvyError) -> Self {
        Self {
            result: Err(error),
            _call: None,
            _bytes: None,
        }
    }
}
pub(super) struct Budget {
    calls: Arc<Semaphore>,
    bytes: Arc<Semaphore>,
}
impl Budget {
    pub fn new() -> Self {
        Self {
            calls: Arc::new(Semaphore::new(64)),
            bytes: Arc::new(Semaphore::new(REPLY_BYTES)),
        }
    }
    pub fn reserve_call(&self, control: bool) -> IvyResult<Call> {
        let capacity = || {
            IvyError::new(
                "native_capacity",
                "Native request capacity is full; foreground slots remain reserved.",
            )
        };
        if !control && self.calls.available_permits() <= 8 {
            return Err(capacity());
        }
        self.calls
            .clone()
            .try_acquire_owned()
            .map_err(|_| capacity())
    }
    pub fn finish(&self, response: Answer, result: IvyResult<IvyJson>, call: Call, control: bool) {
        // Durable hooks and validation have already run, even for lost waiters.
        if response.is_closed() {
            return;
        }
        // Preserve small local close/hook errors even while reply payloads fill
        // the pool. The 64 accepted-call guards bound these to 256 KiB in total.
        if result
            .as_ref()
            .is_err_and(|error| error.retained_bytes() <= 4096)
        {
            let _ = response.send(Completion {
                result,
                _call: Some(call),
                _bytes: None,
            });
            return;
        }
        let size = match &result {
            Ok(reply) => reply.retained_bytes(),
            Err(error) => error.retained_bytes(),
        }
        .saturating_add(std::mem::size_of::<Completion>())
        .saturating_add(256);
        let retained = if size <= REPLY_BYTES {
            self.bytes
                .clone()
                .try_acquire_many_owned(size as u32)
                .ok()
                .filter(|_| control || self.bytes.available_permits() >= CONTROL_BYTES)
        } else {
            None
        };
        let result = if retained.is_some() {
            result
        } else {
            let outcome = result
                .as_ref()
                .err()
                .and_then(|error| error.outcome.literal())
                .unwrap_or(Outcome::Unknown);
            // At most 64 accepted completions can hold these small refusals.
            // Never evict a known durable receipt or replay its native request.
            Err(IvyError::new(
                "native_capacity",
                "Native reply retention is full; reconcile the original operation.",
            )
            .with_outcome(outcome))
        };
        let _ = response.send(Completion {
            result,
            _call: Some(call),
            _bytes: retained,
        });
    }
}
