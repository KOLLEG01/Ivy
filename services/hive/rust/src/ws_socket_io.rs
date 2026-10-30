//! The production WebSocket owner, including retained sends and exact-peer cleanup.
use crate::{
    server_peers::{Peer, PeerSocket, Peers},
    ws_send::Socket as SocketPort,
};
use futures_util::{SinkExt, StreamExt};
use ivy_contracts::{IvyError, IvyJson, IvyResult};
use ivy_sdk::websocket::{Outgoing, Pending, SendBudget};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    sync::{mpsc, watch},
};
use tokio_tungstenite::{
    WebSocketStream,
    tungstenite::{Message, protocol::Role},
};
use tokio_util::sync::CancellationToken;

/// All connections share this retained-payload budget, independent of DB admission.
#[derive(Clone, Default)]
pub struct Sockets(SendBudget);
struct Socket {
    outgoing: Outgoing,
    closed: CancellationToken,
}
impl Socket {
    fn new(budget: &Sockets) -> (Self, mpsc::Receiver<Pending>) {
        let (outgoing, receiver) = budget.0.channel();
        (
            Self {
                outgoing,
                closed: CancellationToken::new(),
            },
            receiver,
        )
    }
}
impl SocketPort for Socket {
    fn is_open(&self) -> bool {
        !self.closed.is_cancelled()
    }
    fn buffered_amount(&self) -> f64 {
        self.outgoing.buffered_bytes() as f64
    }
    fn send_text(&mut self, text: String) -> IvyResult<()> {
        if !self.closed.is_cancelled() && self.outgoing.text(text).is_err() {
            self.closed.cancel();
        }
        Ok(())
    }
    fn terminate(&mut self) -> IvyResult<()> {
        self.closed.cancel();
        Ok(())
    }
}
impl PeerSocket for Socket {
    fn ping(&mut self) -> IvyResult<()> {
        if !self.closed.is_cancelled() && self.outgoing.ping().is_err() {
            self.closed.cancel();
        }
        Ok(())
    }
}
struct Connected {
    peers: Peers,
    peer: Peer,
    closed: CancellationToken,
}
impl Drop for Connected {
    fn drop(&mut self) {
        self.closed.cancel();
        if let Err(error) = self.peers.closed(&self.peer) {
            eprintln!("Hive peer cleanup: {error:?}");
        }
    }
}
fn failure(error: impl std::fmt::Display) -> IvyError {
    IvyError::new("internal_error", error.to_string())
}

impl Sockets {
    pub async fn run<S: AsyncRead + AsyncWrite + Unpin>(
        &self,
        transport: S,
        peers: Peers,
        context: IvyJson,
        mut stopping: watch::Receiver<bool>,
    ) -> IvyResult<()> {
        if *stopping.borrow() {
            return Ok(());
        }
        let (socket, mut receiver) = Socket::new(self);
        let closed = socket.closed.clone();
        // Declare cleanup before the actual transport owner: I/O closes first,
        // including when the connection future is dropped during a blocked send.
        let peer = peers.connected(Box::new(socket), context);
        let connected = Connected {
            peers,
            peer,
            closed,
        };
        let mut in_flight: Option<Pending> = None;
        let config = ivy_sdk::websocket::config();
        let stream = WebSocketStream::from_raw_socket(transport, Role::Server, Some(config)).await;
        let (mut writer, mut reader) = stream.split();
        let closing = {
            let write = async {
                while let Some(queued) = receiver.recv().await {
                    in_flight = Some(queued);
                    let message = in_flight.as_mut().unwrap().take_message();
                    ivy_sdk::websocket::send(&mut writer, message)
                        .await
                        .map_err(failure)?;
                    in_flight.take();
                }
                Ok::<bool, IvyError>(false)
            };
            let read = async {
                while let Some(message) = reader.next().await {
                    if connected.closed.is_cancelled() {
                        return Ok(false);
                    }
                    match message {
                        Ok(Message::Text(text)) => {
                            connected
                                .peers
                                .message(&connected.peer, text.as_bytes(), false)?
                        }
                        Ok(Message::Binary(bytes)) => {
                            connected.peers.message(&connected.peer, &bytes, true)?
                        }
                        Ok(Message::Pong(_)) => connected.peers.pong(&connected.peer),
                        Ok(Message::Close(_)) => return Ok(true),
                        Err(error) => return Err(failure(error)),
                        _ => {}
                    }
                }
                Ok::<bool, IvyError>(false)
            };
            // Both halves keep progressing under backpressure. Neither task is
            // detached; closing cancels the actual I/O owner, not accepted DB work.
            tokio::select! {
                biased;
                _ = stopping.changed() => Ok(false),
                _ = connected.closed.cancelled() => Ok(false),
                result = write => result,
                result = read => result,
            }
        }?;
        if closing {
            tokio::select! {
                biased;
                _ = stopping.changed() => {},
                _ = connected.closed.cancelled() => {},
                sent = writer.flush() => { let _ = sent; },
            }
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "../../../../tests/rust/hive_ws_socket_queue.rs"]
mod queue_tests;
