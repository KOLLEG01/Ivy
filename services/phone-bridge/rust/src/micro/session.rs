//! One USB/IP connection. Only firmware replies may wait for a future HID read.
use super::{
    BUS_ID, ControlTransfer, DEVICE_ID, MicroClose, MicroInputReceipt, MicroInputResult,
    MicroProtocol, MicroVoiceObservation, Report, control, export, gate::Gate, microphone,
};
use std::{
    collections::VecDeque,
    io,
    net::Shutdown,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::{net::TcpStream, runtime::Handle};
use tokio_util::sync::CancellationToken;

pub type MicroDiagnostic = Arc<dyn Fn(&str) + Send + Sync>;
pub trait MicroClock: Send + Sync {
    fn timestamp(&self) -> Duration;
}
pub(super) struct Clock(Instant);
impl Default for Clock {
    fn default() -> Self {
        Self(Instant::now())
    }
}
impl MicroClock for Clock {
    fn timestamp(&self) -> Duration {
        self.0.elapsed()
    }
}

/// The session owns this connection, including interrupting an outstanding
/// read on close. The native multi-thread executor drives cancellable TCP IO;
/// a session worker retains the original synchronous report-write boundary.
pub trait MicroStream: Send + Sync {
    fn read_exact(&self, bytes: &mut [u8]) -> io::Result<()>;
    fn write_all(&self, bytes: &[u8]) -> io::Result<()>;
    fn close(&self);
}
struct TcpMicroStream {
    socket: TcpStream,
    runtime: Handle,
    stop: CancellationToken,
}
impl MicroStream for TcpMicroStream {
    fn read_exact(&self, bytes: &mut [u8]) -> io::Result<()> {
        // Only the session's blocking worker reads. Cancelling this readiness
        // wait interrupts an idle import immediately; shutdown alone does not
        // cancel an outstanding synchronous Winsock receive.
        self.runtime.block_on(async {
            let mut read = 0;
            while read < bytes.len() {
                tokio::select! {
                    biased;
                    _ = self.stop.cancelled() => return Err(io::ErrorKind::Interrupted.into()),
                    ready = self.socket.readable() => ready?,
                }
                match self.socket.try_read(&mut bytes[read..]) {
                    Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
                    Ok(count) => read += count,
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => continue,
                    Err(error) => return Err(error),
                }
            }
            Ok(())
        })
    }
    fn write_all(&self, bytes: &[u8]) -> io::Result<()> {
        // Dispatch remains synchronous under the import/session gates. Hand
        // off the executor worker while preserving the original one-second
        // send bound, including a peer that stops draining reports.
        tokio::task::block_in_place(|| {
            self.runtime.block_on(async {
                tokio::time::timeout(Duration::from_secs(1), async {
                let mut written = 0;
                while written < bytes.len() {
                    tokio::select! {
                        biased;
                        _ = self.stop.cancelled() => return Err(io::ErrorKind::Interrupted.into()),
                        ready = self.socket.writable() => ready?,
                    }
                    match self.socket.try_write(&bytes[written..]) {
                        Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
                        Ok(count) => written += count,
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => continue,
                        Err(error) => return Err(error),
                    }
                }
                Ok(())
            }).await.unwrap_or_else(|_| Err(io::ErrorKind::TimedOut.into()))
            })
        })
    }
    fn close(&self) {
        self.stop.cancel();
        let _ = socket2::SockRef::from(&self.socket).shutdown(Shutdown::Both);
    }
}
struct Group {
    reports: VecDeque<Report>,
    created: Duration,
    started: bool,
}
struct State {
    protocol: MicroProtocol,
    reads: VecDeque<u32>,
    replies: VecDeque<Group>,
    report_count: usize,
    imported: bool,
    closed: bool,
    generation: i64,
    last_status: Duration,
    last_inbound: Duration,
    last_transfer: String,
    running: Option<MicroClose>,
}
type Activate = dyn Fn(&MicroUsbSession) -> bool + Send + Sync;
struct Owner {
    gate: Gate,
    state: Mutex<State>,
    stream: Box<dyn MicroStream>,
    activate: Arc<Activate>,
    time: Arc<dyn MicroClock>,
    created: Duration,
    diagnostic: Option<MicroDiagnostic>,
}
#[derive(Clone)]
pub struct MicroUsbSession(Arc<Owner>);
fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
impl MicroUsbSession {
    pub fn new(
        stream: Box<dyn MicroStream>,
        generation: i64,
        activate: impl Fn(&Self) -> bool + Send + Sync + 'static,
        time: Arc<dyn MicroClock>,
        diagnostic: Option<MicroDiagnostic>,
    ) -> io::Result<Self> {
        if !(1..=2_097_151).contains(&generation) {
            return Err(invalid("Bounded positive connection generation required."));
        }
        let created = time.timestamp();
        Ok(Self(Arc::new(Owner {
            gate: Gate::default(),
            state: Mutex::new(State {
                protocol: MicroProtocol::default(),
                reads: VecDeque::new(),
                replies: VecDeque::new(),
                report_count: 0,
                imported: false,
                closed: false,
                generation: generation << 32,
                last_status: Duration::ZERO,
                last_inbound: Duration::ZERO,
                last_transfer: "import".into(),
                running: None,
            }),
            stream,
            activate: Arc::new(activate),
            time,
            created,
            diagnostic,
        })))
    }
    pub(super) fn tcp(
        stream: TcpStream,
        generation: i64,
        activate: impl Fn(&Self) -> bool + Send + Sync + 'static,
        diagnostic: Option<MicroDiagnostic>,
    ) -> io::Result<Self> {
        stream.set_nodelay(true)?;
        Self::new(
            Box::new(TcpMicroStream {
                socket: stream,
                runtime: Handle::current(),
                stop: CancellationToken::new(),
            }),
            generation,
            activate,
            Arc::new(Clock::default()),
            diagnostic,
        )
    }
    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.0.state.lock().unwrap_or_else(|e| e.into_inner())
    }
    pub fn same_connection(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
    pub fn generation(&self) -> i64 {
        self.0.gate.run(|| self.state().generation)
    }
    fn ready_state(&self, s: &State) -> bool {
        !s.closed
            && s.imported
            && s.protocol.saw_rgb_configuration()
            && s.protocol.saw_status()
            && self.0.time.timestamp().saturating_sub(s.last_status) < Duration::from_secs(240)
    }
    pub fn ready(&self) -> bool {
        self.0.gate.run(|| self.ready_state(&self.state()))
    }
    pub fn observe_voice(&self, expected: i64) -> MicroVoiceObservation {
        self.0.gate.run(|| {
            let s = self.state();
            if s.closed || expected != s.generation {
                MicroVoiceObservation {
                    state: "unavailable".into(),
                    generation: s.generation,
                    dictation_sequence: 0,
                }
            } else {
                MicroVoiceObservation {
                    state: s.protocol.lighting_state().into(),
                    generation: s.generation,
                    dictation_sequence: s.protocol.dictation_sequence(),
                }
            }
        })
    }
    pub fn close(&self) {
        self.0.gate.run(|| {
            {
                let mut s = self.state();
                if s.closed {
                    return;
                }
                s.closed = true;
                s.reads.clear();
                s.replies.clear();
                s.report_count = 0;
            }
            self.0.stream.close();
        });
    }
    pub fn start(&self) -> MicroInputResult<MicroClose> {
        self.0.gate.run(|| {
            let mut s = self.state();
            if let Some(running) = &s.running {
                return Ok(running.clone());
            }
            let session = self.clone();
            let running = MicroClose::start(async move {
                match tokio::task::spawn_blocking(move || session.run()).await {
                    Ok(result) => result.map_err(Arc::from),
                    Err(error) => Err(Arc::new(error) as Arc<dyn std::error::Error + Send + Sync>),
                }
            })?;
            s.running = Some(running.clone());
            Ok(running)
        })
    }
    pub fn disposal(&self) -> MicroClose {
        self.close();
        self.0.gate.run(|| {
            self.state()
                .running
                .clone()
                .unwrap_or_else(|| MicroClose::completed(Ok(())))
        })
    }
    fn write(&self, s: &State, bytes: &[u8]) -> io::Result<()> {
        if s.closed {
            return Err(invalid("Micro connection closed."));
        }
        self.0.stream.write_all(bytes)
    }
    fn reply(
        &self,
        s: &State,
        sequence: u32,
        status: i32,
        payload: &[u8],
        command: u32,
    ) -> io::Result<()> {
        let mut bytes = vec![0; 48 + payload.len()];
        bytes[..4].copy_from_slice(&command.to_be_bytes());
        bytes[4..8].copy_from_slice(&sequence.to_be_bytes());
        bytes[20..24].copy_from_slice(&status.to_be_bytes());
        bytes[24..28].copy_from_slice(&(payload.len() as u32).to_be_bytes());
        bytes[48..].copy_from_slice(payload);
        self.write(s, &bytes)
    }
    fn next_report(&self, s: &mut State) -> io::Result<Option<Report>> {
        while let Some(group) = s.replies.front_mut() {
            if self.0.time.timestamp().saturating_sub(group.created) >= Duration::from_secs(2) {
                if group.started {
                    return Err(invalid("Partially delivered Micro response expired."));
                }
                s.report_count -= group.reports.len();
                s.replies.pop_front();
                continue;
            }
            group.started = true;
            let report = group.reports.pop_front().expect("nonempty response group");
            s.report_count -= 1;
            if group.reports.is_empty() {
                s.replies.pop_front();
            }
            return Ok(Some(report));
        }
        Ok(None)
    }
    fn inbound(&self, s: &mut State, bytes: &[u8]) -> MicroInputResult<()> {
        let before = s.protocol.status_requests();
        let response =
            s.protocol
                .receive(bytes)
                .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> {
                    if e.is_argument() {
                        Box::new(invalid(&e.to_string()))
                    } else {
                        Box::new(e)
                    }
                })?;
        s.last_inbound = self.0.time.timestamp();
        if before != s.protocol.status_requests() {
            s.last_status = s.last_inbound;
        }
        if response.is_empty() {
            return Ok(());
        }
        let mut group = VecDeque::new();
        for report in response {
            group.push_back(report);
            if report[2] > 0
                && usize::from(report[2]) + 2 < report.len()
                && report[usize::from(report[2]) + 2] == 10
            {
                if s.report_count + group.len() > 128 {
                    return Err(invalid("Micro response backlog limit.").into());
                }
                s.report_count += group.len();
                s.replies.push_back(Group {
                    reports: std::mem::take(&mut group),
                    created: self.0.time.timestamp(),
                    started: false,
                });
            }
        }
        if !group.is_empty() {
            return Err(invalid("Unterminated Micro response.").into());
        }
        while !s.reads.is_empty() {
            let Some(report) = self.next_report(s)? else {
                break;
            };
            let sequence = s.reads.pop_front().unwrap();
            self.reply(s, sequence, 0, &report, 3)?;
        }
        Ok(())
    }
    pub fn send_microphone(
        &self,
        pressed: bool,
        expected: i64,
        current: &dyn Fn() -> MicroInputResult<bool>,
    ) -> MicroInputReceipt {
        let mut writing = false;
        let result = catch_unwind(AssertUnwindSafe(|| {
            self.0.gate.run(|| -> MicroInputResult<&'static str> {
                {
                    let s = self.state();
                    if expected != s.generation || !self.ready_state(&s) {
                        return Ok("not_submitted");
                    }
                }
                if !current()? {
                    return Ok("not_submitted");
                }
                let mut s = self.state();
                if s.reads.is_empty() || s.report_count != 0 {
                    return Ok("not_submitted");
                }
                let report = microphone(pressed);
                let sequence = s.reads.pop_front().unwrap();
                writing = true;
                self.reply(&s, sequence, 0, &report, 3)?;
                Ok("submitted")
            })
        }));
        let phase = match result {
            Ok(Ok(phase)) => phase,
            _ => {
                self.close();
                if writing {
                    "outcome_unknown"
                } else {
                    "not_submitted"
                }
            }
        };
        MicroInputReceipt {
            phase: phase.into(),
            pressed,
            generation: self.generation(),
        }
    }
    fn read(&self, length: usize) -> io::Result<Vec<u8>> {
        let mut bytes = vec![0; length];
        self.0.stream.read_exact(&mut bytes)?;
        Ok(bytes)
    }
    fn run(&self) -> MicroInputResult<()> {
        struct Finish(MicroUsbSession);
        impl Drop for Finish {
            fn drop(&mut self) {
                self.0.close();
            }
        }
        let _finish = Finish(self.clone());
        match self.transfers() {
            Err(error) if error.downcast_ref::<io::Error>().is_some() => {
                let (closed, last) = self.0.gate.run(|| {
                    let s = self.state();
                    (s.closed, s.last_transfer.clone())
                });
                if !closed {
                    if let Some(diagnostic) = &self.0.diagnostic {
                        let _ = catch_unwind(AssertUnwindSafe(|| {
                            diagnostic(&format!("{last}: {error}"))
                        }));
                    }
                }
                Ok(())
            }
            result => result,
        }
    }
    fn transfers(&self) -> MicroInputResult<()> {
        while !self.0.gate.run(|| self.state().closed) {
            if !self.0.gate.run(|| self.state().imported) {
                let operation = self.read(8)?;
                if u16::from_be_bytes(operation[..2].try_into().unwrap()) != 0x0111
                    || u32_at(&operation, 4) != 0
                {
                    return Err(invalid("Invalid USB/IP operation header.").into());
                }
                match u16::from_be_bytes(operation[2..4].try_into().unwrap()) {
                    0x8005 => {
                        let mut reply = vec![0; 328];
                        reply[..4].copy_from_slice(&[1, 0x11, 0, 5]);
                        reply[8..12].copy_from_slice(&1u32.to_be_bytes());
                        reply[12..324].copy_from_slice(&export());
                        reply[324] = 3;
                        self.0.gate.run(|| self.write(&self.state(), &reply))?;
                    }
                    0x8003 => {
                        let bus = self.read(32)?;
                        let mut expected = [0; 32];
                        expected[..BUS_ID.len()].copy_from_slice(BUS_ID.as_bytes());
                        // Activation must run outside the session gate: every
                        // device status/input call acquires device then session.
                        if bus != expected || !(self.0.activate)(self) {
                            return Err(invalid("USB/IP import refused.").into());
                        }
                        self.0.gate.run(|| -> io::Result<()> {
                            let mut s = self.state();
                            let mut reply = vec![0; 320];
                            reply[..4].copy_from_slice(&[1, 0x11, 0, 3]);
                            reply[8..].copy_from_slice(&export());
                            self.write(&s, &reply)?;
                            s.imported = true;
                            s.last_status = self.0.time.timestamp();
                            s.last_inbound = s.last_status;
                            Ok(())
                        })?;
                    }
                    _ => return Err(invalid("Unsupported USB/IP operation.").into()),
                }
                continue;
            }
            let header = self.read(48)?;
            let (command, sequence, direction, endpoint) = (
                u32_at(&header, 0),
                u32_at(&header, 4),
                u32_at(&header, 12),
                u32_at(&header, 16),
            );
            if self.0.diagnostic.is_some() {
                self.0.gate.run(|| {
                use std::fmt::Write;
                let mut setup = String::with_capacity(16); for b in &header[40..] { write!(&mut setup, "{b:02X}").unwrap(); }
                self.state().last_transfer = format!("command={command} device={} direction={direction} endpoint={endpoint} length={} packets={} setup={setup}", u32_at(&header, 8), u32_at(&header, 24), u32_at(&header, 32));
            });
            }
            if u32_at(&header, 8) != DEVICE_ID || direction > 1 || endpoint > 1 {
                return Err(invalid("Invalid USB/IP transfer identity.").into());
            }
            if command == 2 {
                self.0.gate.run(|| -> io::Result<()> {
                    let mut s = self.state();
                    let victim = u32_at(&header, 20);
                    let removed = s.reads.contains(&victim);
                    s.reads.retain(|v| *v != victim);
                    self.reply(&s, sequence, if removed { -104 } else { 0 }, &[], 4)?;
                    if removed && s.reads.is_empty() {
                        if s.generation & i64::from(u32::MAX) == i64::from(u32::MAX) {
                            return Err(invalid("Micro generation exhausted."));
                        }
                        s.generation += 1;
                        s.protocol = MicroProtocol::default();
                        s.replies.clear();
                        s.report_count = 0;
                        s.last_status = self.0.time.timestamp();
                        s.last_inbound = s.last_status;
                    }
                    Ok(())
                })?;
                continue;
            }
            let length = u32_at(&header, 24);
            if command != 1 || length > 4096 || !matches!(u32_at(&header, 32), 0 | u32::MAX) {
                return Err(invalid("Unsupported USB/IP transfer.").into());
            }
            let bytes = if direction == 0 {
                self.read(length as usize)?
            } else {
                Vec::new()
            };
            self.0.gate.run(|| -> MicroInputResult<()> {
                let mut s = self.state();
                if endpoint == 0 {
                    match control(
                        direction,
                        length,
                        header[40..48].try_into().unwrap(),
                        &bytes,
                    )
                    .map_err(|e| invalid(&e.to_string()))?
                    {
                        ControlTransfer::Reply { status, payload } => {
                            self.reply(&s, sequence, status, &payload, 3)?
                        }
                        ControlTransfer::HidOutput(report) => {
                            self.inbound(&mut s, &report)?;
                            self.reply(&s, sequence, 0, &[], 3)?;
                        }
                    }
                } else if length != 64 {
                    return Err(invalid("Exact HID report transfer required.").into());
                } else if direction == 0 {
                    self.inbound(&mut s, &bytes)?;
                    self.reply(&s, sequence, 0, &[], 3)?;
                } else {
                    if s.reads.len() >= 32 || s.reads.contains(&sequence) {
                        return Err(invalid("Pending HID read limit or duplicate sequence.").into());
                    }
                    if let Some(report) = self.next_report(&mut s)? {
                        self.reply(&s, sequence, 0, &report, 3)?;
                    } else {
                        s.reads.push_back(sequence);
                    }
                }
                Ok(())
            })?;
        }
        Ok(())
    }
    pub fn check_heartbeat(&self) {
        self.0.gate.run(|| {
            let close = {
                let s = self.state();
                let now = self.0.time.timestamp();
                !s.closed
                    && ((!s.imported
                        && now.saturating_sub(self.0.created) > Duration::from_secs(10))
                        || (s.imported
                            && now.saturating_sub(s.last_inbound) < Duration::from_secs(240)
                            && now.saturating_sub(s.last_status) >= Duration::from_secs(240)))
            };
            if close {
                self.close();
            }
        });
    }
}
fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap())
}
