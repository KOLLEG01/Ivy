//! WebSocket over the official proxy's existing byte pipes. Never owns a daemon.
use super::codex_rpc::{NATIVE_FRAME_BYTES, NATIVE_REQUEST_FRAME_BYTES};
use futures_util::{SinkExt, StreamExt};
use ivy_contracts::{IvyError, IvyResult};
use std::{
    future::Future,
    io,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context, Poll},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    sync::{mpsc, oneshot, watch},
    task::AbortHandle,
};
use tokio_tungstenite::{
    WebSocketStream, client_async_with_config,
    tungstenite::{
        Message,
        protocol::{
            CloseFrame, WebSocketConfig,
            frame::{
                Frame,
                coding::{CloseCode, Data, OpCode},
            },
        },
    },
};

struct Pipes<R, W> {
    read: R,
    write: W,
}
impl<R: AsyncRead + Unpin, W: Unpin> AsyncRead for Pipes<R, W> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.read).poll_read(cx, buffer)
    }
}
impl<R: Unpin, W: AsyncWrite + Unpin> AsyncWrite for Pipes<R, W> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.write).poll_write(cx, bytes)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.write).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.write).poll_shutdown(cx)
    }
}
fn invalid() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "Invalid or unavailable native proxy frame.",
    )
}
struct Write {
    bytes: Vec<u8>,
    reply: oneshot::Sender<bool>,
}
struct Lifetime {
    abort: AbortHandle,
    close: mpsc::UnboundedSender<()>,
}
impl Drop for Lifetime {
    fn drop(&mut self) {
        self.abort.abort();
    }
}

/// One complete LF-terminated JSONL frame per write, as supplied by NativeRpc.
/// A completed write means WebSocket flush, not a native operation receipt.
pub struct ProxyInput {
    writes: mpsc::UnboundedSender<Write>,
    pending: Option<(usize, oneshot::Receiver<bool>)>,
    closing: Arc<AtomicBool>,
    _lifetime: Arc<Lifetime>,
}
impl AsyncWrite for ProxyInput {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        if self.pending.is_none() {
            if bytes.len() > NATIVE_REQUEST_FRAME_BYTES + 1
                || bytes.last() != Some(&b'\n')
                || self.closing.load(Ordering::Acquire)
            {
                return Poll::Ready(Err(invalid()));
            }
            let (reply, receiver) = oneshot::channel();
            if self
                .writes
                .send(Write {
                    bytes: bytes[..bytes.len() - 1].to_vec(),
                    reply,
                })
                .is_err()
            {
                return Poll::Ready(Err(invalid()));
            }
            self.pending = Some((bytes.len(), receiver));
        }
        let (length, receiver) = self.pending.as_mut().unwrap();
        match Pin::new(receiver).poll(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(result) => {
                let length = *length;
                self.pending = None;
                Poll::Ready(if result == Ok(true) {
                    Ok(length)
                } else {
                    Err(invalid())
                })
            }
        }
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let Some((_, receiver)) = self.pending.as_mut() else {
            return Poll::Ready(Ok(()));
        };
        match Pin::new(receiver).poll(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(result) => {
                self.pending = None;
                Poll::Ready(if matches!(result, Ok(true)) {
                    Ok(())
                } else {
                    Err(invalid())
                })
            }
        }
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.poll_flush(cx)
    }
}

pub struct ProxyOutput {
    frames: mpsc::Receiver<Vec<u8>>,
    current: Vec<u8>,
    offset: usize,
    ended: watch::Receiver<Option<bool>>,
    closing: Arc<AtomicBool>,
    error_reported: bool,
    _lifetime: Arc<Lifetime>,
}
impl AsyncRead for ProxyOutput {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if buffer.remaining() == 0 {
            return Poll::Ready(Ok(()));
        }
        if self.offset == self.current.len() {
            match self.frames.poll_recv(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Some(frame)) => {
                    self.current = frame;
                    self.offset = 0;
                }
                Poll::Ready(None) => {
                    let clean = (*self.ended.borrow())
                        .unwrap_or_else(|| self.closing.load(Ordering::Acquire));
                    if !clean && !self.error_reported {
                        self.error_reported = true;
                        return Poll::Ready(Err(invalid()));
                    }
                    return Poll::Ready(Ok(()));
                }
            }
        }
        let count = buffer.remaining().min(self.current.len() - self.offset);
        buffer.put_slice(&self.current[self.offset..self.offset + count]);
        self.offset += count;
        if self.offset == self.current.len() {
            self.current = Vec::new();
            self.offset = 0;
        }
        Poll::Ready(Ok(()))
    }
}
#[derive(Clone)]
pub struct ProxyControl {
    closing: Arc<AtomicBool>,
    ended: watch::Receiver<Option<bool>>,
    lifetime: Arc<Lifetime>,
}
impl ProxyControl {
    /// Idempotent, bounded close handshake. Stops only these pipes and I/O tasks.
    pub async fn close(&self) {
        if !self.closing.swap(true, Ordering::AcqRel) {
            let _ = self.lifetime.close.send(());
        }
        let mut ended = self.ended.clone();
        let wait = async {
            while ended.borrow().is_none() {
                if ended.changed().await.is_err() {
                    break;
                }
            }
        };
        if tokio::time::timeout(Duration::from_millis(1250), wait)
            .await
            .is_err()
        {
            self.lifetime.abort.abort();
        }
    }
}
pub struct ProxyTransport {
    pub input: ProxyInput,
    pub output: ProxyOutput,
    pub control: ProxyControl,
}
impl ProxyTransport {
    pub async fn start<W, R>(input: W, output: R) -> IvyResult<Self>
    where
        W: AsyncWrite + Unpin + Send + 'static,
        R: AsyncRead + Unpin + Send + 'static,
    {
        let config = WebSocketConfig::default()
            .write_buffer_size(0)
            .max_write_buffer_size(NATIVE_REQUEST_FRAME_BYTES + 1024)
            .max_message_size(Some(NATIVE_FRAME_BYTES))
            .max_frame_size(Some(NATIVE_FRAME_BYTES));
        let (socket, _) = tokio::time::timeout(
            Duration::from_secs(30),
            client_async_with_config(
                "ws://localhost/",
                Pipes {
                    read: output,
                    write: input,
                },
                Some(config),
            ),
        )
        .await
        // The original WebSocket error/timeout rejects with a non-Ivy Error;
        // preserve IvyError.from's public classification instead of inventing a
        // transport-specific rejection for an HTTP/upgrade failure.
        .map_err(|_| IvyError::internal())?
        .map_err(|_| IvyError::internal())?;
        let (writes, write_rx) = mpsc::unbounded_channel();
        let (frames, frame_rx) = mpsc::channel(1);
        let (close, close_rx) = mpsc::unbounded_channel();
        let (ended_tx, ended) = watch::channel(None);
        let closing = Arc::new(AtomicBool::new(false));
        let flag = closing.clone();
        let task = tokio::spawn(async move {
            let clean = run(socket, write_rx, frames.clone(), close_rx).await;
            flag.store(true, Ordering::Release);
            let _ = ended_tx.send(Some(clean));
            drop(frames);
        });
        let lifetime = Arc::new(Lifetime {
            abort: task.abort_handle(),
            close,
        });
        Ok(Self {
            input: ProxyInput {
                writes,
                pending: None,
                closing: closing.clone(),
                _lifetime: lifetime.clone(),
            },
            output: ProxyOutput {
                frames: frame_rx,
                current: vec![],
                offset: 0,
                ended: ended.clone(),
                closing: closing.clone(),
                error_reported: false,
                _lifetime: lifetime.clone(),
            },
            control: ProxyControl {
                closing,
                ended,
                lifetime,
            },
        })
    }
}
async fn graceful<S: AsyncRead + AsyncWrite + Unpin>(socket: &mut WebSocketStream<S>) {
    let _ = tokio::time::timeout(Duration::from_secs(1), async {
        let _ = socket
            .send(Message::Close(Some(CloseFrame {
                code: CloseCode::Normal,
                reason: "ivy_agent_stopping".into(),
            })))
            .await;
        while let Some(message) = socket.next().await {
            if matches!(message, Ok(Message::Close(_)) | Err(_)) {
                break;
            }
        }
    })
    .await;
}
async fn run<S: AsyncRead + AsyncWrite + Unpin>(
    mut socket: WebSocketStream<S>,
    mut writes: mpsc::UnboundedReceiver<Write>,
    frames: mpsc::Sender<Vec<u8>>,
    mut close: mpsc::UnboundedReceiver<()>,
) -> bool {
    let mut pending: Option<Vec<u8>> = None;
    loop {
        tokio::select! {
            biased;
            _=close.recv()=>{graceful(&mut socket).await;return true;},
            write=writes.recv()=>{
                let Some(write)=write else {graceful(&mut socket).await;return true;};
                // Buffer bytes sent as a text frame preserve the original transport's
                // handling of malformed UTF-8: the peer rejects it, not a local codec.
                let success=socket.send(Message::Frame(Frame::message(write.bytes,OpCode::Data(Data::Text),true))).await.is_ok();
                let _=write.reply.send(success);if !success{return false;}
            },
            permit=frames.reserve(),if pending.is_some()=>{
                let Ok(permit)=permit else {graceful(&mut socket).await;return true;};
                permit.send(pending.take().unwrap());
            },
            next=socket.next(),if pending.is_none()=>{
                match next {
                    Some(Ok(Message::Text(text)))=>{let mut bytes=text.as_bytes().to_vec();bytes.push(b'\n');pending=Some(bytes);},
                    Some(Ok(Message::Close(_)))=>{let _=tokio::time::timeout(Duration::from_secs(1),socket.flush()).await;return true;},
                    Some(Ok(Message::Ping(_)))=>{if socket.flush().await.is_err(){return false;}},
                    Some(Ok(Message::Pong(_)))=>{},
                    Some(Ok(_))|Some(Err(_))=>return false,
                    None=>return true,
                }
            }
        }
    }
}
