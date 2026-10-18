//! Node's request-head view and receive deadlines around Hyper's frame parser.
use hyper::{
    HeaderMap,
    header::{HeaderName, HeaderValue},
};
use std::{
    collections::VecDeque,
    io,
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll},
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    net::TcpStream,
    sync::Notify,
};

const HEADER_BYTES: usize = 16_384;
const HEADER_PAIRS: usize = 1000;
// Node counts target/name/value bytes, excluding line and field separators.
// Hyper sees at most the retained prefix plus framing fields below.
const HEAD_WIRE_BYTES: usize = HEADER_BYTES * 4;

#[derive(Clone, Copy)]
enum BudgetPart {
    Method,
    Target,
    Version,
    Name,
    BeforeValue,
    Value,
}
struct Budget {
    part: BudgetPart,
    bytes: usize,
}
impl Budget {
    fn new() -> Self {
        Self {
            part: BudgetPart::Method,
            bytes: 0,
        }
    }
    fn push(&mut self, byte: u8) -> bool {
        match self.part {
            BudgetPart::Method => {
                if byte == b' ' {
                    self.part = BudgetPart::Target;
                }
            }
            BudgetPart::Target => {
                if byte == b' ' {
                    self.part = BudgetPart::Version;
                } else {
                    self.bytes += 1;
                }
            }
            BudgetPart::Version => {
                if byte == b'\n' {
                    self.part = BudgetPart::Name;
                }
            }
            BudgetPart::Name => match byte {
                b':' => self.part = BudgetPart::BeforeValue,
                b'\r' | b'\n' => {}
                _ => self.bytes += 1,
            },
            BudgetPart::BeforeValue => match byte {
                b' ' | b'\t' => {}
                b'\r' => {}
                b'\n' => self.part = BudgetPart::Name,
                _ => {
                    self.bytes += 1;
                    self.part = BudgetPart::Value;
                }
            },
            BudgetPart::Value => match byte {
                b'\r' => {}
                b'\n' => self.part = BudgetPart::Name,
                _ => self.bytes += 1,
            },
        }
        self.bytes >= HEADER_BYTES
    }
}
enum Phase {
    Head,
    Fixed(u64),
    ChunkLine,
    ChunkData(u64),
    ChunkEnd(u8),
    Trailers,
    Failed,
}
struct Receiving {
    started: Instant,
    headers_complete: bool,
}
pub struct Capture {
    phase: Phase,
    head: Vec<u8>,
    line: Vec<u8>,
    budget: Budget,
    current: Option<Receiving>,
    views: VecDeque<HeaderMap>,
    rejected: Option<u16>,
    response_started: bool,
    pub changed: Arc<Notify>,
}
fn leading_ows(value: &[u8]) -> &[u8] {
    let index = value
        .iter()
        .position(|b| !matches!(b, b' ' | b'\t'))
        .unwrap_or(value.len());
    &value[index..]
}
fn trim_ows(value: &[u8]) -> &[u8] {
    let value = leading_ows(value);
    let end = value
        .iter()
        .rposition(|b| !matches!(b, b' ' | b'\t'))
        .map_or(0, |i| i + 1);
    &value[..end]
}
impl Capture {
    pub fn new() -> Self {
        Self {
            phase: Phase::Head,
            head: Vec::new(),
            line: Vec::new(),
            budget: Budget::new(),
            current: None,
            views: VecDeque::new(),
            rejected: None,
            response_started: false,
            changed: Arc::new(Notify::new()),
        }
    }
    pub fn view(&mut self) -> Option<HeaderMap> {
        self.views.pop_front()
    }
    pub fn rejection(&self) -> Option<u16> {
        self.rejected
    }
    pub fn expired(&self) -> bool {
        self.current.as_ref().is_some_and(|r| {
            r.started.elapsed() >= Duration::from_secs(if r.headers_complete { 15 } else { 10 })
        })
    }
    pub fn can_report_timeout(&self) -> bool {
        !self.response_started
    }
    fn reject(&mut self, status: u16) {
        self.rejected = Some(status);
        self.phase = Phase::Failed;
        self.changed.notify_one();
    }
    fn complete(&mut self) {
        self.current = None;
        self.phase = Phase::Head;
        self.budget = Budget::new();
    }
    fn finish_head(&mut self, output: &mut Vec<u8>) -> Result<(), ()> {
        let mut lines = self
            .head
            .split(|b| *b == b'\n')
            .map(|l| l.strip_suffix(b"\r").unwrap_or(l));
        let first = lines.next().ok_or(())?;
        let mut encoded = Vec::with_capacity(self.head.len());
        encoded.extend_from_slice(first);
        encoded.extend_from_slice(b"\r\n");
        let mut view = HeaderMap::new();
        let mut length = None;
        let mut chunked = false;
        let mut framing: Vec<(HeaderName, Vec<u8>)> = Vec::new();
        for (index, line) in lines.take_while(|l| !l.is_empty()).enumerate() {
            let colon = line.iter().position(|b| *b == b':').ok_or(())?;
            let name = HeaderName::from_bytes(&line[..colon]).map_err(|_| ())?;
            let value = trim_ows(&line[colon + 1..]);
            let header = HeaderValue::from_bytes(value).map_err(|_| ())?;
            if index < HEADER_PAIRS {
                view.append(name.clone(), header);
            }
            let is_framing = matches!(
                name.as_str(),
                "content-length" | "transfer-encoding" | "connection" | "upgrade"
            );
            if index < HEADER_PAIRS && !is_framing {
                encoded.extend_from_slice(&line[..colon]);
                encoded.extend_from_slice(b": ");
                encoded.extend_from_slice(value);
                encoded.extend_from_slice(b"\r\n");
            }
            if name == hyper::header::CONTENT_LENGTH {
                if length.is_some() {
                    return Err(());
                }
                let parsed = std::str::from_utf8(value)
                    .map_err(|_| ())?
                    .parse::<u64>()
                    .map_err(|_| ())?;
                // Leave duplicate/framing validation to Hyper, including equal duplicates.
                length = Some(parsed);
            }
            if name == hyper::header::TRANSFER_ENCODING {
                chunked = value
                    .rsplit(|b| *b == b',')
                    .next()
                    .is_some_and(|v| trim_ows(v).eq_ignore_ascii_case(b"chunked"));
            }
            if is_framing {
                if let Some((_, prior)) = framing.iter_mut().find(|(key, _)| *key == name) {
                    prior.extend_from_slice(b", ");
                    prior.extend_from_slice(value);
                } else {
                    framing.push((name, value.to_vec()));
                }
            }
        }
        for (name, value) in framing {
            encoded.extend_from_slice(name.as_str().as_bytes());
            encoded.extend_from_slice(b": ");
            encoded.extend(value);
            encoded.extend_from_slice(b"\r\n");
        }
        encoded.extend_from_slice(b"\r\n");
        self.views.push_back(view);
        output.extend(encoded);
        self.head.clear();
        self.current.as_mut().ok_or(())?.headers_complete = true;
        if chunked {
            self.phase = Phase::ChunkLine;
        } else if let Some(length) = length.filter(|n| *n > 0) {
            self.phase = Phase::Fixed(length);
        } else {
            self.complete();
        }
        Ok(())
    }
    fn feed(&mut self, mut bytes: &[u8]) -> Vec<u8> {
        let mut output = Vec::new();
        while !bytes.is_empty() {
            match self.phase {
                Phase::Failed => break,
                Phase::Head => {
                    let byte = bytes[0];
                    bytes = &bytes[1..];
                    // Node ignores leading empty lines before beginning a message.
                    if self.head.is_empty() && matches!(byte, b'\r' | b'\n') {
                        continue;
                    }
                    if self.current.is_none() {
                        self.current = Some(Receiving {
                            started: Instant::now(),
                            headers_complete: false,
                        });
                        self.response_started = false;
                    }
                    self.head.push(byte);
                    if self.budget.push(byte) || self.head.len() > HEAD_WIRE_BYTES {
                        self.reject(431);
                        break;
                    }
                    if self.head.ends_with(b"\r\n\r\n") || self.head.ends_with(b"\n\n") {
                        if self.finish_head(&mut output).is_err() {
                            self.reject(400);
                            break;
                        }
                    }
                }
                Phase::Fixed(remaining) | Phase::ChunkData(remaining) => {
                    let count = remaining.min(bytes.len() as u64) as usize;
                    output.extend_from_slice(&bytes[..count]);
                    bytes = &bytes[count..];
                    let left = remaining - count as u64;
                    if matches!(self.phase, Phase::Fixed(_)) {
                        if left == 0 {
                            self.complete();
                        } else {
                            self.phase = Phase::Fixed(left);
                        }
                    } else if left == 0 {
                        self.phase = Phase::ChunkEnd(2);
                    } else {
                        self.phase = Phase::ChunkData(left);
                    }
                }
                Phase::ChunkEnd(remaining) => {
                    output.push(bytes[0]);
                    bytes = &bytes[1..];
                    if remaining == 1 {
                        self.phase = Phase::ChunkLine;
                    } else {
                        self.phase = Phase::ChunkEnd(remaining - 1);
                    }
                }
                Phase::ChunkLine | Phase::Trailers => {
                    let byte = bytes[0];
                    bytes = &bytes[1..];
                    output.push(byte);
                    self.line.push(byte);
                    if self.line.len() > HEAD_WIRE_BYTES {
                        self.reject(431);
                        break;
                    }
                    if byte == b'\n' {
                        let line = self.line.strip_suffix(b"\n").unwrap();
                        let line = line.strip_suffix(b"\r").unwrap_or(line);
                        if matches!(self.phase, Phase::Trailers) {
                            if line.is_empty() {
                                self.complete();
                            }
                        } else {
                            let number = line.split(|b| *b == b';').next().unwrap();
                            match std::str::from_utf8(number)
                                .ok()
                                .and_then(|n| u64::from_str_radix(n, 16).ok())
                            {
                                Some(0) => self.phase = Phase::Trailers,
                                Some(count) => self.phase = Phase::ChunkData(count),
                                None => {
                                    self.phase = Phase::Failed;
                                }
                            }
                        }
                        self.line.clear();
                    }
                }
            }
        }
        output
    }
}

pub struct NodeIo {
    pub socket: TcpStream,
    pub capture: Arc<Mutex<Capture>>,
    pending: Vec<u8>,
    offset: usize,
}
impl NodeIo {
    pub fn new(socket: TcpStream) -> Self {
        Self {
            socket,
            capture: Arc::new(Mutex::new(Capture::new())),
            pending: Vec::new(),
            offset: 0,
        }
    }
}
impl AsyncRead for NodeIo {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        output: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        if output.remaining() == 0 {
            return Poll::Ready(Ok(()));
        }
        loop {
            if this.offset < this.pending.len() {
                let count = output.remaining().min(this.pending.len() - this.offset);
                output.put_slice(&this.pending[this.offset..this.offset + count]);
                this.offset += count;
                return Poll::Ready(Ok(()));
            }
            if this.capture.lock().unwrap().rejected.is_some() {
                return Poll::Pending;
            }
            let mut buffer = [0u8; 8192];
            let mut read = ReadBuf::new(&mut buffer);
            match Pin::new(&mut this.socket).poll_read(cx, &mut read) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Err(error)) => return Poll::Ready(Err(error)),
                Poll::Ready(Ok(())) => {
                    if read.filled().is_empty() {
                        return Poll::Ready(Ok(()));
                    }
                    this.pending = this.capture.lock().unwrap().feed(read.filled());
                    this.offset = 0;
                }
            }
        }
    }
}
impl AsyncWrite for NodeIo {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        let result = Pin::new(&mut this.socket).poll_write(cx, bytes);
        if matches!(result,Poll::Ready(Ok(n)) if n>0)
            && bytes.starts_with(b"HTTP/1.")
            && bytes.get(9..12).is_some_and(|code| code >= b"200".as_slice())
        {
            this.capture.lock().unwrap().response_started = true;
        }
        result
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().socket).poll_flush(cx)
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().socket).poll_shutdown(cx)
    }
    fn is_write_vectored(&self) -> bool {
        false
    }
}
