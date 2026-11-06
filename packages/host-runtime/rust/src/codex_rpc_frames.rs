//! In-process protocol values. Reservations cover retained allocations, including
//! spare UTF-16/container capacity, through the receiving owner's processing.
use super::{Incoming, unavailable};
use ivy_contracts::{IvyError, IvyJson, IvyResult};
use std::sync::Arc;
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc};

const QUEUE_BYTES: usize = 64 * 1024 * 1024;
const FRAME_BYTES: usize = 48 * 1024 * 1024;
const FRAME_COUNT: usize = 128;

pub struct NativeFrame {
    pub(super) value: IvyJson,
    _retained: OwnedSemaphorePermit,
}
impl NativeFrame {
    /// The existing per-frame reservation also includes the frame's owner.
    pub const MAXIMUM_VALUE_BYTES: usize = FRAME_BYTES - std::mem::size_of::<Self>();
}
impl std::ops::Deref for NativeFrame {
    type Target = IvyJson;
    fn deref(&self) -> &IvyJson {
        &self.value
    }
}
fn capacity() -> IvyError {
    IvyError::new(
        "native_capacity",
        "In-process native retained-value capacity is full.",
    )
}
fn reserve(value: IvyJson, bytes: &Arc<Semaphore>, maximum: usize) -> IvyResult<NativeFrame> {
    let size = value
        .retained_bytes()
        .saturating_add(std::mem::size_of::<NativeFrame>());
    if size > maximum {
        return Err(capacity());
    }
    let retained = bytes
        .clone()
        .try_acquire_many_owned(size as u32)
        .map_err(|_| capacity())?;
    Ok(NativeFrame {
        value,
        _retained: retained,
    })
}

#[derive(Clone)]
pub struct NativeFrameSender {
    incoming: mpsc::Sender<Incoming>,
    bytes: Arc<Semaphore>,
}
impl NativeFrameSender {
    /// Called by the local protocol owner; never parks that owner's control loop.
    pub fn try_send(&self, value: IvyJson) -> IvyResult<()> {
        let frame = reserve(value, &self.bytes, FRAME_BYTES)?;
        self.incoming
            .try_send(Incoming::Local(frame))
            .map_err(|e| match e {
                mpsc::error::TrySendError::Full(_) => capacity(),
                mpsc::error::TrySendError::Closed(_) => unavailable(),
            })
    }
}
pub struct NativeFrameReceiver(mpsc::Receiver<NativeFrame>);
impl NativeFrameReceiver {
    pub async fn recv(&mut self) -> Option<NativeFrame> {
        self.0.recv().await
    }
}
pub(super) struct Writer {
    sender: mpsc::Sender<NativeFrame>,
    bytes: Arc<Semaphore>,
}
impl Writer {
    pub fn closed(&self) -> bool {
        self.sender.is_closed()
    }
    pub fn writable(&self, control: bool) -> bool {
        self.sender.capacity() > if control { 0 } else { 8 }
            && (control || self.bytes.available_permits() >= 8 * 1024 * 1024)
    }
    pub fn prepare(
        &self,
        value: IvyJson,
        maximum_wire_bytes: usize,
        control: bool,
    ) -> IvyResult<NativeFrame> {
        // This boundary transfers values, not encoded text. Keep separate heap
        // bounds while reserving the final eight MiB for native controls/replies.
        let frame = reserve(
            value,
            &self.bytes,
            maximum_wire_bytes.saturating_mul(2).min(FRAME_BYTES),
        )?;
        if !control && self.bytes.available_permits() < 8 * 1024 * 1024 {
            return Err(capacity());
        }
        Ok(frame)
    }
    pub fn send(&self, frame: NativeFrame) -> IvyResult<()> {
        self.sender.try_send(frame).map_err(|e| match e {
            mpsc::error::TrySendError::Full(_) => capacity(),
            mpsc::error::TrySendError::Closed(_) => unavailable(),
        })
    }
}
pub(super) fn channels() -> (
    Writer,
    NativeFrameReceiver,
    NativeFrameSender,
    mpsc::Receiver<Incoming>,
) {
    let (sender, receiver) = mpsc::channel(FRAME_COUNT);
    let (incoming, received) = mpsc::channel(FRAME_COUNT);
    (
        Writer {
            sender,
            bytes: Arc::new(Semaphore::new(QUEUE_BYTES)),
        },
        NativeFrameReceiver(receiver),
        NativeFrameSender {
            incoming,
            bytes: Arc::new(Semaphore::new(QUEUE_BYTES)),
        },
        received,
    )
}
