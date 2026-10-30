//! Management WebSocket policy shared by Hive and its service connections.
use futures_util::{Sink, SinkExt};
use ivy_contracts::limits::{MANAGEMENT_FRAME_BYTES, MANAGEMENT_SOCKET_BUFFER_BYTES};
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
