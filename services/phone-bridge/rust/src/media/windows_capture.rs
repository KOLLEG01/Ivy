//! One activated audio client's shared-mode capture pump. The owning worker
//! supplies its apartment/authority and retains its own completion and join.
use std::{
    ffi::c_void,
    marker::PhantomData,
    panic::{AssertUnwindSafe, catch_unwind},
    ptr::{self, NonNull},
    rc::Rc,
    sync::atomic::{AtomicBool, Ordering},
};
use windows_sys::{
    Win32::{
        Foundation::CloseHandle,
        System::Threading::{CreateEventW, WaitForSingleObject},
    },
    core::GUID,
};

pub(super) mod device;
pub(super) mod worker;

type Raw = *mut c_void;
type Hr = i32;
type Result<T> = std::result::Result<T, CaptureError>;
pub(crate) type CaptureSink<'a> = dyn FnMut(&[u8], u32, u64, u64) -> Result<()> + 'a;
const CAPTURE: GUID = GUID::from_u128(0xc8adbd64_e71e_48a0_a4de_185c395cd317);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CaptureError {
    Client,
    Configuration,
    Initialize,
    Event,
    BindEvent,
    Service,
    Start,
    Wait,
    Next,
    Buffer,
    Release,
    Pointer,
    Bound,
    Callback,
    Owner,
    Unavailable,
    Cleanup,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CaptureMode {
    Microphone,
    EndpointLoopback,
    ProcessLoopback,
}

#[repr(C)]
struct Unknown {
    query: unsafe extern "system" fn(Raw, *const GUID, *mut Raw) -> Hr,
    add: unsafe extern "system" fn(Raw) -> u32,
    release: unsafe extern "system" fn(Raw) -> u32,
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
#[link(name = "avrt")]
unsafe extern "system" {
    fn AvSetMmThreadCharacteristicsW(task: *const u16, index: *mut u32) -> Raw;
    fn AvRevertMmThreadCharacteristics(task: Raw) -> i32;
}
unsafe extern "system" fn create_event() -> Raw {
    unsafe { CreateEventW(ptr::null(), 0, 0, ptr::null()) as Raw }
}
unsafe extern "system" fn wait_event(event: Raw, ms: u32) -> u32 {
    unsafe { WaitForSingleObject(event as _, ms) }
}
unsafe extern "system" fn close_event(event: Raw) -> i32 {
    unsafe { CloseHandle(event as _) }
}
#[derive(Clone, Copy)]
struct Api {
    event: unsafe extern "system" fn() -> Raw,
    wait: unsafe extern "system" fn(Raw, u32) -> u32,
    close: unsafe extern "system" fn(Raw) -> i32,
    priority: unsafe extern "system" fn(*const u16, *mut u32) -> Raw,
    revert: unsafe extern "system" fn(Raw) -> i32,
}
impl Default for Api {
    fn default() -> Self {
        Self {
            event: create_event,
            wait: wait_event,
            close: close_event,
            priority: AvSetMmThreadCharacteristicsW,
            revert: AvRevertMmThreadCharacteristics,
        }
    }
}
fn check(hr: Hr, error: CaptureError) -> Result<()> {
    if hr < 0 { Err(error) } else { Ok(()) }
}
struct Reference {
    raw: NonNull<c_void>,
    _thread: PhantomData<Rc<()>>,
}
impl Reference {
    unsafe fn adopt(raw: Raw, error: CaptureError) -> Result<Self> {
        NonNull::new(raw)
            .map(|raw| Self {
                raw,
                _thread: PhantomData,
            })
            .ok_or(error)
    }
    fn acquire(error: CaptureError, call: impl FnOnce(*mut Raw) -> Hr) -> Result<Self> {
        let mut raw = ptr::null_mut();
        let hr = call(&mut raw);
        let owner = NonNull::new(raw).map(|raw| Self {
            raw,
            _thread: PhantomData,
        });
        check(hr, error)?;
        owner.ok_or(error)
    }
    unsafe fn table<T>(&self) -> &T {
        unsafe { &**self.raw.as_ptr().cast::<*const T>() }
    }
    fn query(&self, iid: &GUID) -> Result<Self> {
        Self::acquire(CaptureError::Client, |out| unsafe {
            (self.table::<Unknown>().query)(self.raw.as_ptr(), iid, out)
        })
    }
    fn into_raw(self) -> Raw {
        let raw = self.raw.as_ptr();
        std::mem::forget(self);
        raw
    }
}
impl Drop for Reference {
    fn drop(&mut self) {
        unsafe {
            (self.table::<Unknown>().release)(self.raw.as_ptr());
        }
    }
}
struct Event {
    raw: NonNull<c_void>,
    api: Api,
}
impl Drop for Event {
    fn drop(&mut self) {
        unsafe {
            (self.api.close)(self.raw.as_ptr());
        }
    }
}
struct Priority {
    raw: Raw,
    api: Api,
    _thread: PhantomData<Rc<()>>,
}
impl Priority {
    fn enter(api: Api) -> Self {
        let mut index = 0;
        let task: [u16; 10] = [80, 114, 111, 32, 65, 117, 100, 105, 111, 0];
        Self {
            raw: unsafe { (api.priority)(task.as_ptr(), &mut index) },
            api,
            _thread: PhantomData,
        }
    }
}
impl Drop for Priority {
    fn drop(&mut self) {
        if !self.raw.is_null() {
            unsafe {
                (self.api.revert)(self.raw);
            }
        }
    }
}
struct Packet {
    raw: Raw,
    frames: u32,
    released: bool,
}
impl Packet {
    fn release(mut self) -> Result<()> {
        self.released = true;
        check(
            unsafe { (self.table().release)(self.raw, self.frames) },
            CaptureError::Release,
        )
    }
    unsafe fn table(&self) -> &Capture {
        unsafe { &**self.raw.cast::<*const Capture>() }
    }
}
impl Drop for Packet {
    fn drop(&mut self) {
        if !self.released {
            unsafe {
                (self.table().release)(self.raw, self.frames);
            }
        }
    }
}

pub(crate) struct CapturePump {
    // Match capture-interface -> audio-client -> event teardown ordering.
    capture: Option<Reference>,
    client: Reference,
    event: Option<Event>,
    api: Api,
    buffer_ms: u16,
    silence: Vec<u8>,
}
impl CapturePump {
    /// Takes exactly one owned IAudioClient reference on its initialized COM
    /// thread. The caller must supply a valid vtable; the pump never moves it.
    pub unsafe fn from_owned_client(
        client: Raw,
        mode: CaptureMode,
        buffer_ms: u16,
    ) -> Result<Self> {
        unsafe { Self::prepare(client, mode, buffer_ms, Api::default()) }
    }
    unsafe fn prepare(client: Raw, mode: CaptureMode, buffer_ms: u16, api: Api) -> Result<Self> {
        let mut pump = Self {
            capture: None,
            client: unsafe { Reference::adopt(client, CaptureError::Client)? },
            event: None,
            api,
            buffer_ms,
            silence: vec![],
        };
        if !(10..=100).contains(&buffer_ms) {
            return Err(CaptureError::Configuration);
        }
        let flags = match mode {
            CaptureMode::Microphone => 0x88040000,
            CaptureMode::EndpointLoopback => 0x88060000,
            CaptureMode::ProcessLoopback => 0x60000,
        };
        let format = Format {
            tag: 3,
            channels: 2,
            rate: 48000,
            bytes: 384000,
            align: 8,
            bits: 32,
            extra: 0,
        };
        let table = unsafe { pump.client.table::<Client>() };
        let raw = pump.client.raw.as_ptr();
        check(
            unsafe {
                (table.initialize)(
                    raw,
                    0,
                    flags,
                    i64::from(buffer_ms) * 10000,
                    0,
                    &format,
                    &GUID::from_u128(0),
                )
            },
            CaptureError::Initialize,
        )?;
        let event = NonNull::new(unsafe { (api.event)() }).ok_or(CaptureError::Event)?;
        pump.event = Some(Event { raw: event, api });
        check(
            unsafe { (table.event)(raw, event.as_ptr()) },
            CaptureError::BindEvent,
        )?;
        Ok(pump)
    }
    /// Runs once on the same native thread. The enclosing retained worker owns
    /// its stop signal, reports stopped to the route and proves the thread join.
    pub fn run(mut self, stop: &AtomicBool, sink: &mut CaptureSink<'_>) -> Result<()> {
        self.run_retained(stop, sink)
    }
    fn run_retained(&mut self, stop: &AtomicBool, sink: &mut CaptureSink<'_>) -> Result<()> {
        let priority = Priority::enter(self.api);
        let raw = self.client.raw.as_ptr();
        let table = unsafe { &**raw.cast::<*const Client>() };
        let result = (|| {
            self.capture = Some(Reference::acquire(CaptureError::Service, |out| unsafe {
                (table.service)(raw, &CAPTURE, out)
            })?);
            check(unsafe { (table.start)(raw) }, CaptureError::Start)?;
            while !stop.load(Ordering::Acquire) {
                let event = self.event.as_ref().unwrap().raw.as_ptr();
                match unsafe { (self.api.wait)(event, u32::from(self.buffer_ms) * 3) } {
                    0 | 258 => {}
                    _ => return Err(CaptureError::Wait),
                }
                if stop.load(Ordering::Acquire) {
                    break;
                }
                self.drain(sink)?;
            }
            Ok(())
        })();
        // The original recorder preserves the capture failure when either of
        // these best-effort device-removal cleanup operations fails.
        unsafe {
            (table.stop)(raw);
            (table.reset)(raw);
        }
        drop(priority);
        result
    }
    fn drain(&mut self, sink: &mut CaptureSink<'_>) -> Result<()> {
        let raw = self.capture.as_ref().unwrap().raw.as_ptr();
        let table = unsafe { &**raw.cast::<*const Capture>() };
        let mut next = 0;
        check(unsafe { (table.next)(raw, &mut next) }, CaptureError::Next)?;
        while next > 0 {
            let (mut data, mut frames, mut flags, mut position, mut timestamp) =
                (ptr::null_mut(), 0, 0, 0, 0);
            check(
                unsafe {
                    (table.buffer)(
                        raw,
                        &mut data,
                        &mut frames,
                        &mut flags,
                        &mut position,
                        &mut timestamp,
                    )
                },
                CaptureError::Buffer,
            )?;
            let packet = Packet {
                raw,
                frames,
                released: false,
            };
            let delivered = (|| {
                if frames > 96000 {
                    return Err(CaptureError::Bound);
                }
                let length = frames as usize * 8;
                let bytes = if flags & 2 != 0 {
                    if self.silence.len() < length {
                        self.silence.resize(length, 0);
                    }
                    &self.silence[..length]
                } else if length == 0 {
                    &[]
                } else {
                    if data.is_null() {
                        return Err(CaptureError::Pointer);
                    }
                    // SAFETY: the owned packet remains leased, frame count is
                    // bounded, and the caller supplies a valid IAudioClient.
                    unsafe { std::slice::from_raw_parts(data, length) }
                };
                catch_unwind(AssertUnwindSafe(|| sink(bytes, flags, position, timestamp)))
                    .unwrap_or(Err(CaptureError::Callback))
            })();
            // NAudio queries the following packet while the current lease is
            // still held. A release failure supersedes callback/next failures.
            let following = delivered
                .and_then(|()| check(unsafe { (table.next)(raw, &mut next) }, CaptureError::Next));
            packet.release()?;
            following?;
        }
        Ok(())
    }
}

trait CaptureStream {
    fn run(&mut self, stop: &AtomicBool, sink: &mut CaptureSink<'_>) -> Result<()>;
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../tests/rust/phone_windows_capture.rs"
    ));
}
