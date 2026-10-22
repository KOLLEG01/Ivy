use super::{App, failure};
use futures_util::{SinkExt, StreamExt};
use ivy_contracts::{IvyError, IvyJson, IvyResult};
use ivy_hive::{
    server_peers::{Peer, PeerSocket, Peers},
    ws_send::Socket as SocketPort,
};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use tokio::sync::{mpsc, watch};
use tokio_tungstenite::{
    WebSocketStream,
    tungstenite::{Message, protocol::Role},
};

struct Socket {
    outgoing: mpsc::UnboundedSender<Message>,
    open: Arc<AtomicBool>,
    buffered: Arc<AtomicUsize>,
}
impl SocketPort for Socket {
    fn is_open(&self) -> bool {
        self.open.load(Ordering::Acquire)
    }
    fn buffered_amount(&self) -> f64 {
        self.buffered.load(Ordering::Acquire) as f64
    }
    fn send_text(&mut self, text: String) -> IvyResult<()> {
        let bytes = text.len();
        self.buffered.fetch_add(bytes, Ordering::AcqRel);
        if self.outgoing.send(Message::Text(text.into())).is_err() {
            self.buffered.fetch_sub(bytes, Ordering::AcqRel);
            self.open.store(false, Ordering::Release);
            return Err(IvyError::new("service_unavailable", "WebSocket is closed."));
        }
        Ok(())
    }
    fn terminate(&mut self) -> IvyResult<()> {
        self.open.store(false, Ordering::Release);
        let _ = self.outgoing.send(Message::Close(None));
        Ok(())
    }
}
impl PeerSocket for Socket {
    fn ping(&mut self) -> IvyResult<()> {
        self.outgoing
            .send(Message::Ping(Vec::new().into()))
            .map_err(failure)
    }
}
struct Connected {
    peers: Peers,
    peer: Peer,
    open: Arc<AtomicBool>,
}
impl Drop for Connected {
    fn drop(&mut self) {
        self.open.store(false, Ordering::Release);
        if let Err(error) = self.peers.closed(&self.peer) {
            eprintln!("Hive peer cleanup: {error:?}");
        }
    }
}
pub(super) async fn run(
    app: Arc<App>,
    upgraded: hyper::upgrade::OnUpgrade,
    context: IvyJson,
    mut stopping: watch::Receiver<bool>,
) {
    let result = async {
        let upgraded = upgraded.await.map_err(failure)?;
        let stream = WebSocketStream::from_raw_socket(hyper_util::rt::TokioIo::new(upgraded), Role::Server, None).await;
        let (mut writer, mut reader) = stream.split();
        let (outgoing, mut receiver) = mpsc::unbounded_channel();
        let open = Arc::new(AtomicBool::new(true)); let buffered = Arc::new(AtomicUsize::new(0));
        let peers = app.server.network.peers.clone();
        let peer = peers.connected(Box::new(Socket { outgoing, open: open.clone(), buffered: buffered.clone() }), context);
        let connected = Connected { peers, peer, open };
        loop {
            tokio::select! {
                _ = stopping.changed() => { let _ = writer.send(Message::Close(None)).await; break; },
                message = receiver.recv() => match message {
                    Some(message) => {
                        let bytes = match &message { Message::Text(text) => text.len(), _ => 0 };
                        let closing = matches!(message, Message::Close(_));
                        tokio::select! { _ = stopping.changed() => break, sent = writer.send(message) => sent.map_err(failure)?, }
                        buffered.fetch_sub(bytes, Ordering::AcqRel);
                        if closing { break; }
                    }
                    None => break,
                },
                message = reader.next() => match message {
                    Some(Ok(Message::Text(text))) => connected.peers.message(&connected.peer, text.as_bytes(), false)?,
                    Some(Ok(Message::Binary(bytes))) => connected.peers.message(&connected.peer, &bytes, true)?,
                    Some(Ok(Message::Pong(_))) => connected.peers.pong(&connected.peer),
                    Some(Ok(Message::Close(_))) => { let _ = writer.close().await; break; },
                    None => break,
                    Some(Err(error)) => return Err(failure(error)),
                    _ => {},
                },
            }
        }
        Ok::<_, IvyError>(())
    }.await;
    if let Err(error) = result {
        eprintln!("Hive WebSocket: {error:?}");
    }
}
