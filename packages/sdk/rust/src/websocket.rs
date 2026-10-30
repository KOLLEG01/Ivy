//! Management WebSocket policy shared by Hive and its service connections.
use futures_util::{Sink, SinkExt};
use ivy_contracts::limits::{
    CONNECTION_IN_FLIGHT_REQUESTS, MANAGEMENT_FRAME_BYTES, MANAGEMENT_SOCKET_BUFFER_BYTES,
};
use std::sync::{Arc, OnceLock};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc};
use tokio_tungstenite::tungstenite::{
    Message,
    protocol::{
        WebSocketConfig,
        frame::{
            Frame,
            coding::{Data, OpCode},
        },
    },
};

pub const FRAGMENT_BYTES: usize = 64 * 1024;
const QUEUED_MESSAGES: usize = 2 * CONNECTION_IN_FLIGHT_REQUESTS;
const OUTBOUND_BYTES: usize = 2 * MANAGEMENT_SOCKET_BUFFER_BYTES;

/// Shared retained-send capacity, independent of accepted invocation capacity.
#[derive(Clone)]
pub struct SendBudget(Arc<Semaphore>);
impl Default for SendBudget {
    fn default() -> Self {
        Self(Arc::new(Semaphore::new(OUTBOUND_BYTES)))
    }
}
pub(crate) fn service_budget() -> &'static SendBudget {
    static BUDGET: OnceLock<SendBudget> = OnceLock::new();
    BUDGET.get_or_init(Default::default)
}
#[derive(Debug, PartialEq, Eq)]
pub enum SendError {
    Closed,
    Full,
}
pub struct Outgoing {
    sender: mpsc::Sender<Pending>,
    local: Arc<Semaphore>,
    shared: SendBudget,
}
/// Keep this reservation through flush, or until the actual I/O owner drops.
pub struct Pending {
    message: Option<Message>,
    _local: OwnedSemaphorePermit,
    _shared: OwnedSemaphorePermit,
}
impl Pending {
    pub fn take_message(&mut self) -> Message {
        self.message.take().expect("Send message already taken")
    }
}
impl SendBudget {
    pub fn channel(&self) -> (Outgoing, mpsc::Receiver<Pending>) {
        let (sender, receiver) = mpsc::channel(QUEUED_MESSAGES);
        (
            Outgoing {
                sender,
                local: Arc::new(Semaphore::new(MANAGEMENT_SOCKET_BUFFER_BYTES)),
                shared: self.clone(),
            },
            receiver,
        )
    }
}
impl Outgoing {
    pub fn text(&self, text: String) -> Result<(), SendError> {
        let capacity = text.capacity();
        self.enqueue(Message::Text(text.into()), capacity)
    }
    pub fn ping(&self) -> Result<(), SendError> {
        self.enqueue(Message::Ping(Vec::new().into()), 0)
    }
    pub fn buffered_bytes(&self) -> usize {
        MANAGEMENT_SOCKET_BUFFER_BYTES - self.local.available_permits()
    }
    fn enqueue(&self, message: Message, capacity: usize) -> Result<(), SendError> {
        if self.sender.is_closed() {
            return Err(SendError::Closed);
        }
        let bytes = capacity.saturating_add(std::mem::size_of::<Pending>());
        if bytes > MANAGEMENT_SOCKET_BUFFER_BYTES {
            return Err(SendError::Full);
        }
        let local = self
            .local
            .clone()
            .try_acquire_many_owned(bytes as u32)
            .map_err(|_| SendError::Full)?;
        let shared = self
            .shared
            .0
            .clone()
            .try_acquire_many_owned(bytes as u32)
            .map_err(|_| SendError::Full)?;
        self.sender
            .try_send(Pending {
                message: Some(message),
                _local: local,
                _shared: shared,
            })
            .map_err(|error| match error {
                mpsc::error::TrySendError::Closed(_) => SendError::Closed,
                mpsc::error::TrySendError::Full(_) => SendError::Full,
            })
    }
}

pub fn config() -> WebSocketConfig {
    WebSocketConfig::default()
        .read_buffer_size(16 * 1024)
        .write_buffer_size(0)
        .max_write_buffer_size(MANAGEMENT_SOCKET_BUFFER_BYTES)
        .max_message_size(Some(MANAGEMENT_FRAME_BYTES))
        .max_frame_size(Some(MANAGEMENT_FRAME_BYTES))
}

/// Keep the codec's reusable output buffer small after large messages.
/// Finish one call before starting another data message. On cancellation or
/// error, the caller closes the transport rather than replaying a partial send.
pub async fn send<S: Sink<Message> + Unpin>(
    sink: &mut S,
    message: Message,
) -> Result<(), S::Error> {
    let (payload, opcode) = match message {
        Message::Text(text) if text.len() > FRAGMENT_BYTES => (text.into(), Data::Text),
        Message::Binary(bytes) if bytes.len() > FRAGMENT_BYTES => (bytes, Data::Binary),
        message => return sink.send(message).await,
    };
    for start in (0..payload.len()).step_by(FRAGMENT_BYTES) {
        let end = (start + FRAGMENT_BYTES).min(payload.len());
        let opcode = if start == 0 { opcode } else { Data::Continue };
        sink.send(Message::Frame(Frame::message(
            payload.slice(start..end),
            OpCode::Data(opcode),
            end == payload.len(),
        )))
        .await?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "../../../../tests/rust/sdk-websocket-queue.rs"]
mod queue_tests;
