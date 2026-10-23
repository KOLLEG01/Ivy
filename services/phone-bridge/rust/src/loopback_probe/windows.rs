//! Process-excluding WASAPI recorder. COM interfaces, packet leases and event
//! handles stay on one MTA worker; callers retain only commands and completion.
use super::{
    ProbeCapture, ProbeError, ProbeEvents, ProbeFactory, ProbeFuture, ProbeRecorder, Result,
};
use std::{
    ffi::c_void,
    marker::PhantomData,
    panic::{AssertUnwindSafe, catch_unwind},
    ptr::{self, NonNull},
    rc::Rc,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU32, Ordering, fence},
        mpsc,
    },
};
use tokio::sync::{Notify, oneshot};
use windows_sys::core::GUID;

type Raw = *mut c_void;
type Hr = i32;
const UNKNOWN: GUID = GUID::from_u128(0x00000000_0000_0000_c000_000000000046);
const AGILE: GUID = GUID::from_u128(0x94ea2b94_e9cc_49e0_c0ff_ee64ca8f5b90);
const COMPLETION: GUID = GUID::from_u128(0x41d949ab_9862_444a_80f6_c261334da5eb);
const CLIENT: GUID = GUID::from_u128(0x1cb9ad4c_dbfa_4c32_b178_c2f568a703b2);
const CAPTURE: GUID = GUID::from_u128(0xc8adbd64_e71e_48a0_a4de_185c395cd317);
#[repr(C)]
struct Unknown {
    query: unsafe extern "system" fn(Raw, *const GUID, *mut Raw) -> Hr,
    add: unsafe extern "system" fn(Raw) -> u32,
    release: unsafe extern "system" fn(Raw) -> u32,
}
#[repr(C)]
struct Operation {
    unknown: Unknown,
    result: unsafe extern "system" fn(Raw, *mut Hr, *mut Raw) -> Hr,
}
#[repr(C)]
struct CompletionTable {
    unknown: Unknown,
    complete: unsafe extern "system" fn(Raw, Raw) -> Hr,
}
#[repr(C)]
struct Activation {
    kind: i32,
    process: u32,
    mode: i32,
}
#[repr(C)]
struct Blob {
    size: u32,
    data: *mut u8,
}
#[repr(C)]
struct Variant {
    kind: u16,
    reserved: [u16; 3],
    blob: Blob,
}
#[repr(C, packed(1))]
struct Format {
    tag: u16,
    channels: u16,
    rate: u32,
    bytes: u32,
    align: u16,
    bits: u16,
    extra: u16,
}
#[repr(C)]
struct Client {
    unknown: Unknown,
    initialize:
        unsafe extern "system" fn(Raw, i32, u32, i64, i64, *const Format, *const GUID) -> Hr,
    unused: [usize; 6],
    start: unsafe extern "system" fn(Raw) -> Hr,
    stop: unsafe extern "system" fn(Raw) -> Hr,
    reset: unsafe extern "system" fn(Raw) -> Hr,
    event: unsafe extern "system" fn(Raw, Raw) -> Hr,
    service: unsafe extern "system" fn(Raw, *const GUID, *mut Raw) -> Hr,
}
#[repr(C)]
struct Capture {
    unknown: Unknown,
    buffer:
        unsafe extern "system" fn(Raw, *mut *mut u8, *mut u32, *mut u32, *mut u64, *mut u64) -> Hr,
    release: unsafe extern "system" fn(Raw, u32) -> Hr,
    next: unsafe extern "system" fn(Raw, *mut u32) -> Hr,
}
#[repr(C)]
struct Version {
    size: u32,
    major: u32,
    minor: u32,
    build: u32,
    platform: u32,
    service: [u16; 128],
}
#[link(name = "ole32")]
unsafe extern "system" {
    fn CoInitializeEx(reserved: Raw, mode: u32) -> Hr;
    fn CoUninitialize();
}
#[link(name = "ntdll")]
unsafe extern "system" {
    fn RtlGetVersion(version: *mut Version) -> Hr;
}
#[link(name = "Mmdevapi")]
unsafe extern "system" {
    fn ActivateAudioInterfaceAsync(
        path: *const u16,
        iid: *const GUID,
        params: *mut Variant,
        handler: Raw,
        out: *mut Raw,
    ) -> Hr;
}
#[link(name = "kernel32")]
unsafe extern "system" {
    fn CreateEventW(attributes: Raw, manual: i32, initial: i32, name: *const u16) -> Raw;
    fn WaitForSingleObject(event: Raw, milliseconds: u32) -> u32;
    fn CloseHandle(handle: Raw) -> i32;
}
#[derive(Clone, Copy)]
struct Api {
    initialize: unsafe extern "system" fn(Raw, u32) -> Hr,
    uninitialize: unsafe extern "system" fn(),
    version: unsafe extern "system" fn(*mut Version) -> Hr,
    activate: unsafe extern "system" fn(*const u16, *const GUID, *mut Variant, Raw, *mut Raw) -> Hr,
    event: unsafe extern "system" fn(Raw, i32, i32, *const u16) -> Raw,
    wait: unsafe extern "system" fn(Raw, u32) -> u32,
    close: unsafe extern "system" fn(Raw) -> i32,
}
impl Default for Api {
    fn default() -> Self {
        Self {
            initialize: CoInitializeEx,
            uninitialize: CoUninitialize,
            version: RtlGetVersion,
            activate: ActivateAudioInterfaceAsync,
            event: CreateEventW,
            wait: WaitForSingleObject,
            close: CloseHandle,
        }
    }
}
fn check(hr: Hr) -> Result<()> {
    if hr >= 0 {
        Ok(())
    } else {
        Err(ProbeError::Capture)
    }
}
fn equal(a: &GUID, b: &GUID) -> bool {
    a.data1 == b.data1 && a.data2 == b.data2 && a.data3 == b.data3 && a.data4 == b.data4
}
struct Reference {
    raw: NonNull<c_void>,
    _thread: PhantomData<Rc<()>>,
}
impl Reference {
    unsafe fn table<T>(&self) -> &T {
        unsafe { &**self.raw.as_ptr().cast::<*const T>() }
    }
    fn acquire(call: impl FnOnce(*mut Raw) -> Hr) -> Result<Self> {
        let mut out = ptr::null_mut();
        let hr = call(&mut out);
        let owner = NonNull::new(out).map(|raw| Self {
            raw,
            _thread: PhantomData,
        });
        check(hr)?;
        owner.ok_or(ProbeError::Capture)
    }
    fn query(&self, iid: &GUID) -> Result<Self> {
        Self::acquire(|out| unsafe { (self.table::<Unknown>().query)(self.raw.as_ptr(), iid, out) })
    }
}
impl Drop for Reference {
    fn drop(&mut self) {
        unsafe {
            (self.table::<Unknown>().release)(self.raw.as_ptr());
        }
    }
}
struct Apartment(Api);
impl Apartment {
    fn new(api: Api) -> Result<Self> {
        check(unsafe { (api.initialize)(ptr::null_mut(), 0) })?;
        Ok(Self(api))
    }
}
impl Drop for Apartment {
    fn drop(&mut self) {
        unsafe { (self.0.uninitialize)() }
    }
}
struct Event {
    raw: NonNull<c_void>,
    api: Api,
}
impl Event {
    fn new(api: Api) -> Result<Self> {
        NonNull::new(unsafe { (api.event)(ptr::null_mut(), 0, 0, ptr::null()) })
            .map(|raw| Self { raw, api })
            .ok_or(ProbeError::Capture)
    }
    fn wait(&self) -> Result<()> {
        match unsafe { (self.api.wait)(self.raw.as_ptr(), 120) } {
            0 | 258 => Ok(()),
            _ => Err(ProbeError::Capture),
        }
    }
}
impl Drop for Event {
    fn drop(&mut self) {
        unsafe {
            (self.api.close)(self.raw.as_ptr());
        }
    }
}

// This agile callback only signals readiness. The original activation operation
// and every returned audio interface are read and released on the worker MTA.
#[repr(C)]
struct Completion {
    table: &'static CompletionTable,
    references: AtomicU32,
    ready: mpsc::SyncSender<()>,
}
unsafe extern "system" fn callback_query(raw: Raw, iid: *const GUID, out: *mut Raw) -> Hr {
    if out.is_null() {
        return 0x80004003u32 as i32;
    }
    unsafe {
        *out = ptr::null_mut();
    }
    if !iid.is_null()
        && [UNKNOWN, AGILE, COMPLETION]
            .iter()
            .any(|g| equal(unsafe { &*iid }, g))
    {
        unsafe {
            *out = raw;
            callback_add(raw);
        }
        0
    } else {
        0x80004002u32 as i32
    }
}
unsafe extern "system" fn callback_add(raw: Raw) -> u32 {
    unsafe { &*raw.cast::<Completion>() }
        .references
        .fetch_add(1, Ordering::Relaxed)
        + 1
}
unsafe extern "system" fn callback_release(raw: Raw) -> u32 {
    let remaining = unsafe { &*raw.cast::<Completion>() }
        .references
        .fetch_sub(1, Ordering::Release)
        - 1;
    if remaining == 0 {
        fence(Ordering::Acquire);
        drop(unsafe { Box::from_raw(raw.cast::<Completion>()) });
    }
    remaining
}
unsafe extern "system" fn callback_complete(raw: Raw, _: Raw) -> Hr {
    // Waking the worker may release its operation/callback owners immediately.
    // Use an independent sender before publishing that wakeup.
    let ready = unsafe { &*raw.cast::<Completion>() }.ready.clone();
    let _ = ready.try_send(());
    0
}
static COMPLETION_TABLE: CompletionTable = CompletionTable {
    unknown: Unknown {
        query: callback_query,
        add: callback_add,
        release: callback_release,
    },
    complete: callback_complete,
};
fn activate(api: Api, process: u32) -> Result<Reference> {
    activate_target(api, process, 1)
}
fn activate_target(api: Api, process: u32, mode: i32) -> Result<Reference> {
    if process == 0 || !matches!(mode, 0 | 1) {
        return Err(ProbeError::Capture);
    }
    let (ready, wait) = mpsc::sync_channel(1);
    let callback = Reference {
        raw: NonNull::new(
            Box::into_raw(Box::new(Completion {
                table: &COMPLETION_TABLE,
                references: AtomicU32::new(1),
                ready,
            }))
            .cast(),
        )
        .unwrap(),
        _thread: PhantomData,
    };
    let mut activation = Activation {
        kind: 1,
        process,
        mode,
    };
    let mut variant = Variant {
        kind: 65,
        reserved: [0; 3],
        blob: Blob {
            size: std::mem::size_of::<Activation>() as u32,
            data: (&mut activation as *mut Activation).cast(),
        },
    };
    let path: Vec<u16> = "VAD\\Process_Loopback\0".encode_utf16().collect();
    let operation = Reference::acquire(|out| unsafe {
        (api.activate)(
            path.as_ptr(),
            &CLIENT,
            &mut variant,
            callback.raw.as_ptr(),
            out,
        )
    })?;
    // Like the original async builder, activation has no cancellation/timeout.
    // Keep parameters, callback and operation alive through a late completion.
    wait.recv().map_err(|_| ProbeError::Capture)?;
    let mut status = -1;
    let activated = Reference::acquire(|out| unsafe {
        (operation.table::<Operation>().result)(operation.raw.as_ptr(), &mut status, out)
    })?;
    check(status)?;
    activated.query(&CLIENT)
}
/// Transfers one IAudioClient reference to the call recorder on its worker.
/// The caller must own that thread's initialized COM apartment and release the
/// returned interface there. Activation remains uncancelable like the builder.
pub(crate) unsafe fn activate_audio_client(process: u32, include_tree: bool) -> Result<Raw> {
    let client = activate_target(Api::default(), process, i32::from(!include_tree))?;
    let raw = client.raw.as_ptr();
    std::mem::forget(client);
    Ok(raw)
}
pub(crate) fn process_loopback_supported() -> bool {
    WindowsProbeFactory::default().supported()
}
struct Session {
    capture: Option<Reference>,
    client: Reference,
    event: Option<Event>,
}
impl Session {
    fn prepare(&mut self, api: Api) -> Result<()> {
        let format = Format {
            tag: 3,
            channels: 2,
            rate: 48000,
            bytes: 384000,
            align: 8,
            bits: 32,
            extra: 0,
        };
        let table = unsafe { self.client.table::<Client>() };
        let r = self.client.raw.as_ptr();
        // The original process-loopback path uses LOOPBACK | EVENTCALLBACK,
        // without AUTOCONVERTPCM or SRC_DEFAULT_QUALITY.
        check(unsafe {
            (table.initialize)(r, 0, 0x60000, 400000, 0, &format, &GUID::from_u128(0))
        })?;
        self.event = Some(Event::new(api)?);
        check(unsafe { (table.event)(r, self.event.as_ref().unwrap().raw.as_ptr()) })
    }
    fn run(&mut self, state: &Worker) -> Result<()> {
        let table = unsafe { self.client.table::<Client>() };
        let r = self.client.raw.as_ptr();
        let result = (|| {
            self.capture = Some(Reference::acquire(|out| unsafe {
                (table.service)(r, &CAPTURE, out)
            })?);
            check(unsafe { (table.start)(r) })?;
            let capture = self.capture.as_ref().unwrap();
            while !state.stop.load(Ordering::Acquire) {
                self.event.as_ref().unwrap().wait()?;
                if !state.stop.load(Ordering::Acquire) {
                    drain(capture, state)?;
                }
            }
            Ok(())
        })();
        // Preserve NAudio's best-effort device-removal cleanup: neither masks
        // the originating capture failure or prevents the stopped notification.
        unsafe {
            (table.stop)(r);
            (table.reset)(r);
        }
        result
    }
}
struct Packet<'a> {
    capture: &'a Reference,
    frames: u32,
    released: bool,
}
impl Packet<'_> {
    fn release(mut self) -> Result<()> {
        self.released = true;
        check(unsafe {
            (self.capture.table::<Capture>().release)(self.capture.raw.as_ptr(), self.frames)
        })
    }
}
impl Drop for Packet<'_> {
    fn drop(&mut self) {
        if !self.released {
            unsafe {
                (self.capture.table::<Capture>().release)(self.capture.raw.as_ptr(), self.frames);
            }
        }
    }
}
fn drain(capture: &Reference, state: &Worker) -> Result<()> {
    let table = unsafe { capture.table::<Capture>() };
    let r = capture.raw.as_ptr();
    let events = state.events.lock().unwrap().as_ref().unwrap().clone();
    while !state.stop.load(Ordering::Acquire) {
        let mut next = 0;
        check(unsafe { (table.next)(r, &mut next) })?;
        if next == 0 {
            break;
        }
        let (mut data, mut frames, mut flags) = (ptr::null_mut(), 0, 0);
        check(unsafe {
            (table.buffer)(
                r,
                &mut data,
                &mut frames,
                &mut flags,
                ptr::null_mut(),
                ptr::null_mut(),
            )
        })?;
        let packet = Packet {
            capture,
            frames,
            released: false,
        };
        // Bound before constructing any span or dereferencing OS memory. Silent
        // packet contents are undefined, may be null and never need to be read.
        if frames > 96000 {
            events.stopped(Err(ProbeError::SampleBound));
        } else if flags & 2 != 0 {
            events.silence(frames as usize * 2);
        } else if frames != 0 {
            if data.is_null() {
                return Err(ProbeError::Capture);
            }
            events.data(
                unsafe { std::slice::from_raw_parts(data, frames as usize * 8) },
                false,
            );
        }
        packet.release()?;
    }
    Ok(())
}
enum Command {
    Start(mpsc::SyncSender<Result<()>>),
    Close,
}
struct Worker {
    stop: AtomicBool,
    events: Mutex<Option<ProbeEvents>>,
    result: OnceLock<Result<()>>,
    joined: OnceLock<Result<()>>,
    changed: Notify,
}
impl Worker {
    fn complete(&self, result: Result<()>) {
        let _ = self.result.set(result);
        self.changed.notify_waiters();
    }
    async fn wait(&self) -> Result<()> {
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if let Some(result) = self.joined.get() {
                return *result;
            }
            changed.await;
        }
    }
}
fn join_worker(
    runtime: &tokio::runtime::Handle,
    thread: std::thread::JoinHandle<()>,
    state: Arc<Worker>,
) {
    // Start joining in the close prefix, so an unpolled/dropped close observer
    // cannot abandon teardown. This mirrors the original Task.Run(thread.Join).
    runtime.spawn_blocking(move || {
        let result = thread
            .join()
            .map_err(|_| ProbeError::Cleanup)
            .and_then(|()| {
                state
                    .result
                    .get()
                    .copied()
                    .unwrap_or(Err(ProbeError::Cleanup))
            });
        let _ = state.joined.set(result);
        state.changed.notify_waiters();
    });
}
fn worker(
    api: Api,
    process: u32,
    state: &Worker,
    commands: mpsc::Receiver<Command>,
    ready: oneshot::Sender<Result<()>>,
) -> Result<()> {
    let opening = (|| {
        let apartment = Apartment::new(api)?;
        let client = activate(api, process)?;
        Ok((apartment, client))
    })();
    let (_apartment, client) = match opening {
        Ok(value) => value,
        Err(error) => {
            let _ = ready.send(Err(error));
            return Ok(());
        }
    };
    let mut session = Session {
        capture: None,
        client,
        event: None,
    };
    if ready.send(Ok(())).is_err() {
        return Ok(());
    }
    if let Ok(Command::Start(reply)) = commands.recv() {
        match session.prepare(api) {
            Ok(()) => {
                let _ = reply.send(Ok(()));
                let result = session.run(state);
                if let Some(events) = state.events.lock().unwrap().as_ref() {
                    events.stopped(result);
                }
            }
            Err(error) => {
                let _ = reply.send(Err(error));
            }
        }
        // The recorder owns the interfaces until explicit close, including
        // after capture errors. Stop alone does not dispose the recorder.
        while !matches!(commands.recv(), Ok(Command::Close) | Err(_)) {}
    }
    Ok(())
}
/// Default native recorder for the fixed passive probe. Construction opens no
/// device; only `open` activates the virtual process-loopback interface.
#[derive(Default)]
pub struct WindowsProbeFactory {
    api: Api,
}
impl ProbeFactory for WindowsProbeFactory {
    fn supported(&self) -> bool {
        let mut v = Version {
            size: std::mem::size_of::<Version>() as u32,
            major: 0,
            minor: 0,
            build: 0,
            platform: 0,
            service: [0; 128],
        };
        (unsafe { (self.api.version)(&mut v) }) >= 0
            && (v.major > 10 || v.major == 10 && (v.minor > 0 || v.build >= 20348))
    }
    fn open(&self, capture: ProbeCapture) -> ProbeFuture<Box<dyn ProbeRecorder>> {
        if capture
            != (ProbeCapture {
                sample_rate: 48000,
                channels: 2,
                buffer_ms: 40,
                excluded_process_id: std::process::id(),
            })
        {
            return Box::pin(async { Err(ProbeError::Capture) });
        }
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return Box::pin(async { Err(ProbeError::Unavailable) });
        };
        let state = Arc::new(Worker {
            stop: AtomicBool::new(false),
            events: Mutex::new(None),
            result: OnceLock::new(),
            joined: OnceLock::new(),
            changed: Notify::new(),
        });
        let (tx, rx) = mpsc::channel();
        let (ready, opening) = oneshot::channel();
        let worker_state = state.clone();
        let api = self.api;
        let started = std::thread::Builder::new()
            .name("phone-loopback-probe".into())
            .spawn(move || {
                let result = catch_unwind(AssertUnwindSafe(|| {
                    worker(api, capture.excluded_process_id, &worker_state, rx, ready)
                }))
                .unwrap_or(Err(ProbeError::Cleanup));
                worker_state.complete(result);
            });
        Box::pin(async move {
            let thread = started.map_err(|_| ProbeError::Unavailable)?;
            if let Err(error) = opening.await.unwrap_or(Err(ProbeError::Capture)) {
                join_worker(&runtime, thread, state.clone());
                let _ = state.wait().await;
                return Err(error);
            }
            Ok(Box::new(Recorder {
                commands: tx,
                state,
                runtime,
                thread: Some(thread),
                started: false,
                closed: false,
            }) as Box<dyn ProbeRecorder>)
        })
    }
}
struct Recorder {
    commands: mpsc::Sender<Command>,
    state: Arc<Worker>,
    runtime: tokio::runtime::Handle,
    thread: Option<std::thread::JoinHandle<()>>,
    started: bool,
    closed: bool,
}
impl Recorder {
    fn signal_close(&mut self) {
        self.state.stop.store(true, Ordering::Release);
        if !self.closed {
            self.closed = true;
            let _ = self.commands.send(Command::Close);
        }
        if let Some(thread) = self.thread.take() {
            join_worker(&self.runtime, thread, self.state.clone());
        }
    }
}
impl ProbeRecorder for Recorder {
    fn subscribe(&mut self, events: ProbeEvents) -> Result<()> {
        if self.started || self.closed {
            return Err(ProbeError::Capture);
        }
        *self.state.events.lock().unwrap() = Some(events);
        Ok(())
    }
    fn start(&mut self) -> Result<()> {
        if self.started || self.closed || self.state.events.lock().unwrap().is_none() {
            return Err(ProbeError::Capture);
        }
        self.started = true;
        let (reply, wait) = mpsc::sync_channel(1);
        self.commands
            .send(Command::Start(reply))
            .map_err(|_| ProbeError::Capture)?;
        wait.recv().map_err(|_| ProbeError::Capture)?
    }
    fn stop(&mut self) -> Result<()> {
        self.state.stop.store(true, Ordering::Release);
        Ok(())
    }
    fn close(&mut self) -> ProbeFuture<()> {
        self.signal_close();
        let state = self.state.clone();
        Box::pin(async move { state.wait().await })
    }
}
impl Drop for Recorder {
    fn drop(&mut self) {
        self.signal_close();
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../tests/rust/phone_loopback_windows.rs"
    ));
}
