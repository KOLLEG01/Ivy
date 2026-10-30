//! The production WebSocket owner, including retained sends and exact-peer cleanup.
use crate::{
    server_peers::{Peer, PeerSocket, Peers},
    ws_send::Socket as SocketPort,
};
use futures_util::{SinkExt, StreamExt};
use ivy_contracts::{
    IvyError, IvyJson, IvyResult,
    limits::{CONNECTION_IN_FLIGHT_REQUESTS, MANAGEMENT_SOCKET_BUFFER_BYTES},
};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    sync::{OwnedSemaphorePermit, Semaphore, mpsc, watch},
};
use tokio_tungstenite::{
    WebSocketStream,
    tungstenite::{Message, protocol::Role},
};
use tokio_util::sync::CancellationToken;

const QUEUED_MESSAGES: usize = 2 * CONNECTION_IN_FLIGHT_REQUESTS;
const OUTBOUND_BYTES: usize = 2 * MANAGEMENT_SOCKET_BUFFER_BYTES;

/// All connections share this retained-payload budget, independent of DB admission.
#[derive(Clone)]
pub struct Sockets(Arc<Semaphore>);
impl Default for Sockets {
    fn default() -> Self {
        Self(Arc::new(Semaphore::new(OUTBOUND_BYTES)))
    }
}
struct State {
    closed: CancellationToken,
    buffered: AtomicUsize,
}
struct Queued {
    message: Option<Message>,
    state: Arc<State>,
    bytes: usize,
    _permit: OwnedSemaphorePermit,
}
impl Drop for Queued {
    fn drop(&mut self) {
        self.state.buffered.fetch_sub(self.bytes, Ordering::AcqRel);
    }
}
struct Socket {
    outgoing: mpsc::Sender<Queued>,
    state: Arc<State>,
    budget: Arc<Semaphore>,
}
impl Socket {
    fn new(budget: &Sockets) -> (Self, mpsc::Receiver<Queued>) {
        let (outgoing, receiver) = mpsc::channel(QUEUED_MESSAGES);
        (
            Self {
                outgoing,
                state: Arc::new(State {
                    closed: CancellationToken::new(),
                    buffered: AtomicUsize::new(0),
                }),
                budget: budget.0.clone(),
            },
            receiver,
        )
    }
    fn enqueue(&self, message: Message, capacity: usize) {
        if self.state.closed.is_cancelled() {
            return;
        }
        let bytes = capacity.saturating_add(std::mem::size_of::<Queued>());
        if bytes > MANAGEMENT_SOCKET_BUFFER_BYTES {
            self.state.closed.cancel();
            return;
        }
        let Ok(permit) = self.budget.clone().try_acquire_many_owned(bytes as u32) else {
            self.state.closed.cancel();
            return;
        };
        if self
            .state
            .buffered
            .try_update(Ordering::AcqRel, Ordering::Acquire, |held| {
                held.checked_add(bytes)
                    .filter(|sum| *sum <= MANAGEMENT_SOCKET_BUFFER_BYTES)
            })
            .is_err()
        {
            self.state.closed.cancel();
            return;
        }
        let queued = Queued {
            message: Some(message),
            state: self.state.clone(),
            bytes,
            _permit: permit,
        };
        if self.outgoing.try_send(queued).is_err() {
            self.state.closed.cancel();
        }
    }
}
impl SocketPort for Socket {
    fn is_open(&self) -> bool {
        !self.state.closed.is_cancelled()
    }
    fn buffered_amount(&self) -> f64 {
        self.state.buffered.load(Ordering::Acquire) as f64
    }
    fn send_text(&mut self, text: String) -> IvyResult<()> {
        let capacity = text.capacity();
        self.enqueue(Message::Text(text.into()), capacity);
        Ok(())
    }
    fn terminate(&mut self) -> IvyResult<()> {
        self.state.closed.cancel();
        Ok(())
    }
}
impl PeerSocket for Socket {
    fn ping(&mut self) -> IvyResult<()> {
        self.enqueue(Message::Ping(Vec::new().into()), 0);
        Ok(())
    }
}
struct Connected {
    peers: Peers,
    peer: Peer,
    state: Arc<State>,
}
impl Drop for Connected {
    fn drop(&mut self) {
        self.state.closed.cancel();
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
        let state = socket.state.clone();
        // Declare cleanup before the actual transport owner: I/O closes first,
        // including when the connection future is dropped during a blocked send.
        let peer = peers.connected(Box::new(socket), context);
        let connected = Connected { peers, peer, state };
        let mut in_flight: Option<Queued> = None;
        let config = ivy_sdk::websocket::config();
        let stream = WebSocketStream::from_raw_socket(transport, Role::Server, Some(config)).await;
        let (mut writer, mut reader) = stream.split();
        let closing = {
            let write = async {
                while let Some(queued) = receiver.recv().await {
                    in_flight = Some(queued);
                    let message = in_flight.as_mut().unwrap().message.take().unwrap();
                    ivy_sdk::websocket::send(&mut writer, message)
                        .await
                        .map_err(failure)?;
                    in_flight.take();
                }
                Ok::<bool, IvyError>(false)
            };
            let read = async {
                while let Some(message) = reader.next().await {
                    if connected.state.closed.is_cancelled() {
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
                _ = connected.state.closed.cancelled() => Ok(false),
                result = write => result,
                result = read => result,
            }
        }?;
        if closing {
            tokio::select! {
                biased;
                _ = stopping.changed() => {},
                _ = connected.state.closed.cancelled() => {},
                sent = writer.flush() => { let _ = sent; },
            }
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "../../../../tests/rust/hive_ws_socket_queue.rs"]
mod queue_tests;
