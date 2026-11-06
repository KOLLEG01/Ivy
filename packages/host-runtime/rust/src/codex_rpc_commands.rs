//! Bound retained command payloads before the actor can accept or prepare them.
//! The existing channel and transport/reply budgets keep their own lifetimes.
use super::{Command, CommandMessage, Priority};
use ivy_contracts::{IvyError, IvyResult, JsString};
use std::sync::Arc;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

const COMMAND_BYTES: usize = 64 * 1024 * 1024;
const SINGLE_BYTES: usize = 48 * 1024 * 1024;
const CONTROL_BYTES: usize = 8 * 1024 * 1024;
const COMMANDS: usize = 64;
const CONTROLS: usize = 8;

pub(super) struct Reservation {
    _calls: OwnedSemaphorePermit,
    _data_calls: Option<OwnedSemaphorePermit>,
    _bytes: OwnedSemaphorePermit,
    _data_bytes: Option<OwnedSemaphorePermit>,
}
pub(super) struct Budget {
    calls: Arc<Semaphore>,
    data_calls: Arc<Semaphore>,
    bytes: Arc<Semaphore>,
    data_bytes: Arc<Semaphore>,
}
pub(super) fn capacity() -> IvyError {
    IvyError::new(
        "native_capacity",
        "Native command retention is full; no new command was queued.",
    )
}
fn string_bytes(value: &JsString) -> usize {
    value
        .0
        .capacity()
        .saturating_mul(std::mem::size_of::<u16>())
}
impl Budget {
    pub fn new() -> Self {
        Self {
            calls: Arc::new(Semaphore::new(COMMANDS)),
            data_calls: Arc::new(Semaphore::new(COMMANDS - CONTROLS)),
            bytes: Arc::new(Semaphore::new(COMMAND_BYTES)),
            data_bytes: Arc::new(Semaphore::new(COMMAND_BYTES - CONTROL_BYTES)),
        }
    }
    pub fn reserve(&self, command: &Command) -> IvyResult<Reservation> {
        let (control, payload) = match command {
            Command::Request(method, params, options, _) => (
                options.priority == Priority::Interaction,
                params
                    .retained_bytes()
                    .saturating_add(string_bytes(method))
                    .saturating_add(options.request_id.as_ref().map_or(0, string_bytes)),
            ),
            Command::Notify(method, params, _) => (
                true,
                params.retained_bytes().saturating_add(string_bytes(method)),
            ),
            Command::Answer(method, id, reply, _, _) => (
                true,
                reply
                    .retained_bytes()
                    .saturating_add(id.retained_bytes())
                    .saturating_add(string_bytes(method)),
            ),
            Command::Close(_) => unreachable!("close carries no external payload"),
        };
        // Include command/options/channel metadata and callback handles. Opaque
        // callback captures cannot be measured here; transferred values can.
        let size = payload
            .saturating_add(std::mem::size_of::<CommandMessage>())
            .saturating_add(256);
        if size > SINGLE_BYTES {
            return Err(capacity());
        }
        let calls = self
            .calls
            .clone()
            .try_acquire_owned()
            .map_err(|_| capacity())?;
        let data_calls = if control {
            None
        } else {
            Some(
                self.data_calls
                    .clone()
                    .try_acquire_owned()
                    .map_err(|_| capacity())?,
            )
        };
        let bytes = self
            .bytes
            .clone()
            .try_acquire_many_owned(size as u32)
            .map_err(|_| capacity())?;
        let data_bytes = if control {
            None
        } else {
            Some(
                self.data_bytes
                    .clone()
                    .try_acquire_many_owned(size as u32)
                    .map_err(|_| capacity())?,
            )
        };
        Ok(Reservation {
            _calls: calls,
            _data_calls: data_calls,
            _bytes: bytes,
            _data_bytes: data_bytes,
        })
    }
}
