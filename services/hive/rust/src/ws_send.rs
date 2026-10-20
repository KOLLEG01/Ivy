//! WebSocket send policy. The socket owner retains flushing, asynchronous send
//! error termination, and the actual ready-state/buffer accounting.
use crate::rpc;
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, Outcome,
    json::stringify,
    limits::{MANAGEMENT_FRAME_BYTES, MANAGEMENT_SOCKET_BUFFER_BYTES},
};

pub trait Socket {
    fn is_open(&self) -> bool;
    fn buffered_amount(&self) -> f64;
    /// The socket owner must terminate on an asynchronous send error. A
    /// synchronous failure propagates after the caller's prior effects.
    fn send_text(&mut self, text: String) -> IvyResult<()>;
    fn terminate(&mut self) -> IvyResult<()>;
}

pub fn encode(value: &IvyJson) -> IvyResult<String> {
    // encodeJson is JSON.stringify plus the wire bound. Domain/schema
    // validation is a separate operation at its original admission boundary.
    stringify(value, MANAGEMENT_FRAME_BYTES)
}

pub fn send(
    socket: &mut (impl Socket + ?Sized),
    value: &IvyJson,
    encoded: Option<&str>,
) -> IvyResult<()> {
    if !socket.is_open() {
        return Ok(());
    }
    let text = match encoded
        .map(str::to_owned)
        .map(Ok)
        .unwrap_or_else(|| encode(value))
    {
        Ok(text) => text,
        Err(_) => {
            let id = rpc::valid_request_id(value.get("id"));
            if id.is_null() {
                return socket.terminate();
            }
            encode(&rpc::failure(
                &IvyError::new(
                    "content_too_large",
                    "Response exceeds the transport byte limit.",
                )
                .with_outcome(Outcome::Unknown),
                id,
            ))?
        }
    };
    if socket.buffered_amount() + text.len() as f64 > MANAGEMENT_SOCKET_BUFFER_BYTES as f64 {
        return socket.terminate();
    }
    socket.send_text(text)
}
