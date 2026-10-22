//! Real upgraded TCP I/O for the Hive WebSocket owner. The receive-protocol
//! owner reads `Stream` and owns frame admission, automatic replies and closing
//! handshakes. HTTP upgrade/authentication and that protocol are separate.
use crate::{
    server_peers::{PeerSocket, UnhandledKind},
    ws_send::Socket as SendSocket,
};
use futures_util::task::AtomicWaker;
use ivy_contracts::{IvyError, IvyResult};
use std::{
    borrow::Cow,
    collections::VecDeque,
    future::poll_fn,
    io,
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll},
};
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    net::{
        TcpStream,
        tcp::{OwnedReadHalf, OwnedWriteHalf},
    },
};
use tokio_tungstenite::tungstenite::protocol::frame::{
    Frame,
    coding::{Data, OpCode},
};

pub type Closed = Box<dyn FnOnce() -> IvyResult<()> + Send>;
pub type Unhandled = Arc<dyn Fn(UnhandledKind, IvyError) + Send + Sync>;
pub struct Owners {
    /// Install the existing exact-peer/generation cleanup and connection lease.
    pub closed: Closed,
    pub unhandled: Unhandled,
}
struct Write {
    bytes: Vec<u8>,
    offset: usize,
}
struct State {
    reader: Option<OwnedReadHalf>,
    writer: Option<OwnedWriteHalf>,
    queued: VecDeque<Write>,
    buffered: usize,
    write_failed: bool,
    terminating: bool,
    read_ended: bool,
    ending: bool,
    write_ended: bool,
    closed: bool,
}
struct Inner {
    state: Mutex<State>,
    drive: AtomicWaker,
    read: AtomicWaker,
    flush: AtomicWaker,
}
#[derive(Clone)]
pub struct Socket(Arc<Inner>);
/// The protocol owner's actual duplex stream. Its writes (including automatic
/// pong/close frames) use the same queue as synchronous Hive response sends.
pub struct Stream {
    socket: Socket,
    eof: bool,
}

/// The caller runs this on the one network executor and supplies an upgraded
/// TCP stream whose HTTP head is retained separately. Initial write readiness
/// is established before installing the peer, without a detached setup task.
/// `connected` installs the actual peer synchronously and returns its cleanup.
/// Dropping a socket clone or a flush observer does not cancel retained I/O.
pub async fn attach(
    stream: TcpStream,
    connected: impl FnOnce(Socket) -> Owners,
) -> IvyResult<Stream> {
    stream.set_nodelay(true).map_err(|_| IvyError::internal())?;
    stream.writable().await.map_err(|_| IvyError::internal())?;
    let (reader, writer) = stream.into_split();
    let socket = Socket(Arc::new(Inner {
        state: Mutex::new(State {
            reader: Some(reader),
            writer: Some(writer),
            queued: VecDeque::new(),
            buffered: 0,
            write_failed: false,
            terminating: false,
            read_ended: false,
            ending: false,
            write_ended: false,
            closed: false,
        }),
        drive: AtomicWaker::new(),
        read: AtomicWaker::new(),
        flush: AtomicWaker::new(),
    }));
    let owners = connected(socket.clone());
    let retained = socket.clone();
    tokio::spawn(async move {
        poll_fn(|cx| retained.poll_drive(cx)).await;
        // Peer callbacks run after the native close transition, outside every
        // transport lock. Preserve callback failures through the runtime owner.
        if let Err(error) = (owners.closed)() {
            (owners.unhandled)(UnhandledKind::Callback, error);
        }
    });
    Ok(Stream { socket, eof: false })
}

impl Socket {
    fn write(&self, bytes: Cow<'_, [u8]>) -> io::Result<usize> {
        let mut state = self.0.state.lock().unwrap();
        if state.closed || state.terminating || state.ending {
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        if state.write_failed {
            return Ok(bytes.len());
        }
        let mut offset = 0;
        if state.queued.is_empty() {
            loop {
                match state.writer.as_ref().unwrap().try_write(&bytes) {
                    Ok(size) if size == bytes.len() => return Ok(size),
                    Ok(0) => {
                        state.write_failed = true;
                        drop(state);
                        self.0.drive.wake();
                        return Ok(bytes.len());
                    }
                    Ok(size) => {
                        offset = size;
                        break;
                    }
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                    // Node's socket write reports the I/O error through the
                    // completion callback; send() has already returned.
                    Err(_) => {
                        // A synchronously failed write has no outstanding
                        // native write charge. The completion still closes
                        // asynchronously, after this network turn returns.
                        state.write_failed = true;
                        drop(state);
                        self.0.drive.wake();
                        return Ok(bytes.len());
                    }
                }
            }
        }
        // Retain the entire outstanding write's charge until it completes,
        // including a prefix already accepted by the operating system.
        let length = bytes.len();
        state.buffered += length;
        state.queued.push_back(Write {
            bytes: bytes.into_owned(),
            offset,
        });
        drop(state);
        self.0.drive.wake();
        Ok(length)
    }
    fn frame(&mut self, frame: Frame) -> IvyResult<()> {
        let mut bytes = Vec::with_capacity(frame.len());
        frame.format(&mut bytes).map_err(|_| IvyError::internal())?;
        // Callers use the OPEN admission fence. An intervening asynchronous
        // close is an I/O completion failure and retains its close notification.
        if self.write(Cow::Owned(bytes)).is_err() {
            self.terminate()?;
        }
        Ok(())
    }
    fn poll_drive(&self, cx: &mut Context<'_>) -> Poll<()> {
        self.0.drive.register(cx.waker());
        let mut state = self.0.state.lock().unwrap();
        if state.write_failed {
            state.terminating = true;
        }
        if !state.terminating {
            loop {
                let Some(write) = state.queued.front() else {
                    break;
                };
                let result = state
                    .writer
                    .as_ref()
                    .unwrap()
                    .try_write(&write.bytes[write.offset..]);
                match result {
                    Ok(0) => {
                        state.terminating = true;
                        break;
                    }
                    Ok(size) => {
                        let write = state.queued.front_mut().unwrap();
                        write.offset += size;
                        if write.offset == write.bytes.len() {
                            let complete = state.queued.pop_front().unwrap();
                            state.buffered -= complete.bytes.len();
                        }
                    }
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        match state.writer.as_ref().unwrap().as_ref().poll_write_ready(cx) {
                            Poll::Pending => break,
                            Poll::Ready(Ok(())) => continue,
                            Poll::Ready(Err(_)) => {
                                state.terminating = true;
                                break;
                            }
                        }
                    }
                    Err(_) => {
                        state.terminating = true;
                        break;
                    }
                }
            }
            if state.queued.is_empty() {
                self.0.flush.wake();
                if state.ending && !state.write_ended {
                    match Pin::new(state.writer.as_mut().unwrap()).poll_shutdown(cx) {
                        Poll::Ready(Ok(())) => state.write_ended = true,
                        Poll::Ready(Err(_)) => state.terminating = true,
                        Poll::Pending => {}
                    }
                }
            }
        }
        if state.terminating || (state.read_ended && state.write_ended) {
            state.closed = true;
            state.queued.clear();
            state.buffered = 0;
            state.reader.take();
            state.writer.take();
            drop(state);
            self.0.read.wake();
            self.0.flush.wake();
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    }
    fn ended(&self) {
        let mut state = self.0.state.lock().unwrap();
        state.read_ended = true;
        state.ending = true;
        drop(state);
        self.0.drive.wake();
    }
}
impl SendSocket for Socket {
    fn is_open(&self) -> bool {
        let state = self.0.state.lock().unwrap();
        !state.closed && !state.terminating && !state.ending
    }
    fn buffered_amount(&self) -> f64 {
        self.0.state.lock().unwrap().buffered as f64
    }
    fn send_text(&mut self, text: String) -> IvyResult<()> {
        self.frame(Frame::message(text, OpCode::Data(Data::Text), true))
    }
    fn terminate(&mut self) -> IvyResult<()> {
        let mut state = self.0.state.lock().unwrap();
        if !state.closed {
            state.terminating = true;
        }
        drop(state);
        self.0.drive.wake();
        self.0.read.wake();
        Ok(())
    }
}
impl PeerSocket for Socket {
    fn ping(&mut self) -> IvyResult<()> {
        if self.is_open() {
            self.frame(Frame::ping(Vec::new()))?;
        }
        Ok(())
    }
}
impl AsyncRead for Stream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        self.socket.0.read.register(cx.waker());
        if self.eof {
            return Poll::Ready(Ok(()));
        }
        let before = buf.filled().len();
        let result = {
            let mut state = self.socket.0.state.lock().unwrap();
            if state.closed || state.terminating {
                return Poll::Ready(Err(io::ErrorKind::ConnectionReset.into()));
            }
            Pin::new(state.reader.as_mut().unwrap()).poll_read(cx, buf)
        };
        match &result {
            Poll::Ready(Ok(())) if before == buf.filled().len() && buf.remaining() != 0 => {
                self.eof = true;
                self.socket.ended();
            }
            Poll::Ready(Err(_)) => {
                let _ = self.socket.terminate();
            }
            _ => {}
        }
        result
    }
}
impl AsyncWrite for Stream {
    fn poll_write(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        Poll::Ready(self.socket.write(Cow::Borrowed(bytes)))
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.socket.0.flush.register(cx.waker());
        let state = self.socket.0.state.lock().unwrap();
        if state.closed || state.terminating {
            Poll::Ready(Err(io::ErrorKind::BrokenPipe.into()))
        } else if state.write_failed {
            Poll::Pending
        } else if state.queued.is_empty() {
            Poll::Ready(Ok(()))
        } else {
            Poll::Pending
        }
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.socket.0.flush.register(cx.waker());
        let mut state = self.socket.0.state.lock().unwrap();
        state.ending = true;
        let result = if state.closed || state.terminating {
            Poll::Ready(Err(io::ErrorKind::BrokenPipe.into()))
        } else if state.write_ended {
            Poll::Ready(Ok(()))
        } else {
            Poll::Pending
        };
        drop(state);
        self.socket.0.drive.wake();
        result
    }
}
impl Drop for Stream {
    fn drop(&mut self) {
        // A completed read half leaves accepted writes to drain. Abandoning the
        // actual receive owner closes the connection, not just its observer.
        if !self.eof {
            let _ = self.socket.terminate();
        }
    }
}
