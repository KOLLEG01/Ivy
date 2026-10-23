//! Shared/event render pumping after native format negotiation. The owning
//! worker supplies its apartment and retains stop, completion and actual join.
use std::{
    ffi::c_void,
    marker::PhantomData,
    panic::{AssertUnwindSafe, catch_unwind},
    ptr::{self, NonNull},
    rc::Rc,
    sync::atomic::{AtomicBool, Ordering},
};
use windows_sys::Win32::{Foundation::CloseHandle, System::Threading::WaitForMultipleObjects};

pub(super) mod device;
pub(super) mod format;
pub(super) mod status;
pub(super) mod worker;

type Raw = *mut c_void;
type Hr = i32;
type Result<T> = std::result::Result<T, RenderError>;
pub(crate) type RenderSink<'a> = dyn FnMut(&mut [u8]) -> Result<usize> + 'a;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RenderError {
    Configuration,
    Client,
    Owner,
    Mix,
    Initialize,
    Event,
    BindEvent,
    Service,
    Latency,
    Signal,
    Unavailable,
    Cleanup,
    BufferSize,
    Buffer,
    Pointer,
    Bound,
    Callback,
    Release,
    Start,
    Wait,
    Padding,
    Stop,
    Reset,
}
#[repr(C)]
struct Unknown {
    unused: [usize; 2],
    release: unsafe extern "system" fn(Raw) -> u32,
}
#[repr(C)]
struct Client {
    unknown: Unknown,
    initialize: usize,
    size: unsafe extern "system" fn(Raw, *mut u32) -> Hr,
    latency: usize,
    padding: unsafe extern "system" fn(Raw, *mut u32) -> Hr,
    unused: [usize; 3],
    start: unsafe extern "system" fn(Raw) -> Hr,
    stop: unsafe extern "system" fn(Raw) -> Hr,
    reset: unsafe extern "system" fn(Raw) -> Hr,
    event: usize,
    service: usize,
}
#[repr(C)]
struct Render {
    unknown: Unknown,
    buffer: unsafe extern "system" fn(Raw, u32, *mut *mut u8) -> Hr,
    release: unsafe extern "system" fn(Raw, u32, u32) -> Hr,
}
#[link(name = "avrt")]
unsafe extern "system" {
    fn AvSetMmThreadCharacteristicsW(task: *const u16, index: *mut u32) -> Raw;
    fn AvRevertMmThreadCharacteristics(task: Raw) -> i32;
}
unsafe extern "system" fn wait(frame: Raw, stop: Raw, milliseconds: u32) -> u32 {
    let handles = [frame as _, stop as _];
    unsafe { WaitForMultipleObjects(2, handles.as_ptr(), 0, milliseconds) }
}
unsafe extern "system" fn close(handle: Raw) -> i32 {
    unsafe { CloseHandle(handle as _) }
}
#[derive(Clone, Copy)]
struct Api {
    wait: unsafe extern "system" fn(Raw, Raw, u32) -> u32,
    close: unsafe extern "system" fn(Raw) -> i32,
    priority: unsafe extern "system" fn(*const u16, *mut u32) -> Raw,
    revert: unsafe extern "system" fn(Raw) -> i32,
}
impl Default for Api {
    fn default() -> Self {
        Self {
            wait,
            close,
            priority: AvSetMmThreadCharacteristicsW,
            revert: AvRevertMmThreadCharacteristics,
        }
    }
}
fn check(hr: Hr, error: RenderError) -> Result<()> {
    if hr < 0 { Err(error) } else { Ok(()) }
}
struct Reference {
    raw: NonNull<c_void>,
    _thread: PhantomData<Rc<()>>,
}
impl Reference {
    fn adopt(raw: Raw) -> Option<Self> {
        NonNull::new(raw).map(|raw| Self {
            raw,
            _thread: PhantomData,
        })
    }
    unsafe fn table<T>(&self) -> &T {
        unsafe { &**self.raw.as_ptr().cast::<*const T>() }
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
}
impl Priority {
    fn enter(api: Api) -> Self {
        let mut index = 0;
        let task = [80, 114, 111, 32, 65, 117, 100, 105, 111, 0];
        Self {
            raw: unsafe { (api.priority)(task.as_ptr(), &mut index) },
            api,
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
struct Lease {
    owner: Option<Raw>,
    frames: u32,
}
impl Lease {
    fn release(&mut self, frames: u32, flags: u32) -> Result<()> {
        if let Some(raw) = self.owner {
            let table = unsafe { &**raw.cast::<*const Render>() };
            check(
                unsafe { (table.release)(raw, frames, flags) },
                RenderError::Release,
            )?;
            self.owner = None;
        }
        Ok(())
    }
    fn dispose(&mut self) -> Result<()> {
        // A failed explicit release still gets the original using/finally
        // attempt with the full frame count and no flags. There is no third try.
        if let Some(raw) = self.owner.take() {
            let table = unsafe { &**raw.cast::<*const Render>() };
            check(
                unsafe { (table.release)(raw, self.frames, 0) },
                RenderError::Release,
            )?;
        }
        Ok(())
    }
}
impl Drop for Lease {
    fn drop(&mut self) {
        let _ = self.dispose();
    }
}

pub(crate) struct RenderPump {
    // Match AudioClient.Dispose -> render interface, client, stop event, frame event.
    render: Option<Reference>,
    client: Option<Reference>,
    stop_event: Option<Event>,
    frame_event: Option<Event>,
    bytes_per_frame: u16,
    latency_ms: u32,
    api: Api,
}
impl RenderPump {
    /// Takes one owned reference for each already configured COM interface and
    /// one owned handle for each event. All methods/drop stay on this thread.
    /// The caller guarantees valid vtables and writable driver buffer extents.
    pub unsafe fn from_owned_stream(
        client: Raw,
        render: Raw,
        frame_event: Raw,
        stop_event: Raw,
        bytes_per_frame: u16,
        latency_ms: u32,
    ) -> Result<Self> {
        unsafe {
            Self::prepare(
                client,
                render,
                frame_event,
                stop_event,
                bytes_per_frame,
                latency_ms,
                Api::default(),
            )
        }
    }
    unsafe fn prepare(
        client: Raw,
        render: Raw,
        frame_event: Raw,
        stop_event: Raw,
        bytes_per_frame: u16,
        latency_ms: u32,
        api: Api,
    ) -> Result<Self> {
        let pump = Self {
            render: Reference::adopt(render),
            client: Reference::adopt(client),
            stop_event: NonNull::new(stop_event).map(|raw| Event { raw, api }),
            frame_event: NonNull::new(frame_event).map(|raw| Event { raw, api }),
            bytes_per_frame,
            latency_ms,
            api,
        };
        if pump.render.is_none()
            || pump.client.is_none()
            || pump.stop_event.is_none()
            || pump.frame_event.is_none()
            || bytes_per_frame == 0
            || latency_ms > i32::MAX as u32 / 3
        {
            return Err(RenderError::Configuration);
        }
        Ok(pump)
    }
    /// Retains interfaces/events after success or failure until explicit owner
    /// disposal. Stops/reset only on the original normal completion path.
    pub fn run(&mut self, stop: &AtomicBool, sink: &mut RenderSink<'_>) -> Result<()> {
        let _priority = Priority::enter(self.api);
        let client = self.client.as_ref().unwrap();
        let raw = client.raw.as_ptr();
        let table = unsafe { client.table::<Client>() };
        let mut frames = 0;
        check(
            unsafe { (table.size)(raw, &mut frames) },
            RenderError::BufferSize,
        )?;
        if frames > i32::MAX as u32 {
            return Err(RenderError::Bound);
        }
        // Initial filling intentionally precedes Start and the stop check.
        if self.fill(frames, sink)? {
            return Ok(());
        }
        check(unsafe { (table.start)(raw) }, RenderError::Start)?;
        let mut ended = false;
        while !stop.load(Ordering::Acquire) {
            let waited = unsafe {
                (self.api.wait)(
                    self.frame_event.as_ref().unwrap().raw.as_ptr(),
                    self.stop_event.as_ref().unwrap().raw.as_ptr(),
                    self.latency_ms * 3,
                )
            };
            if !matches!(waited, 0 | 1 | 258) {
                return Err(RenderError::Wait);
            }
            if stop.load(Ordering::Acquire) {
                continue;
            }
            let mut padding = 0;
            check(
                unsafe { (table.padding)(raw, &mut padding) },
                RenderError::Padding,
            )?;
            if ended {
                if padding == 0 {
                    break;
                }
                continue;
            }
            let available = frames.saturating_sub(padding);
            if available > 10 {
                ended = self.fill(available, sink)?;
            }
        }
        check(unsafe { (table.stop)(raw) }, RenderError::Stop)?;
        check(unsafe { (table.reset)(raw) }, RenderError::Reset)
    }
    fn fill(&self, frames: u32, sink: &mut RenderSink<'_>) -> Result<bool> {
        let render = self.render.as_ref().unwrap();
        let raw = render.raw.as_ptr();
        let table = unsafe { render.table::<Render>() };
        let mut pointer = ptr::null_mut();
        check(
            unsafe { (table.buffer)(raw, frames, &mut pointer) },
            RenderError::Buffer,
        )?;
        let length = (frames as usize)
            .checked_mul(usize::from(self.bytes_per_frame))
            .filter(|&n| n <= i32::MAX as usize)
            .ok_or(RenderError::Bound)?;
        // Span construction failure in the original precedes the lease's using.
        let bytes = if length == 0 {
            &mut []
        } else {
            if pointer.is_null() {
                return Err(RenderError::Pointer);
            }
            unsafe { std::slice::from_raw_parts_mut(pointer, length) }
        };
        let mut lease = Lease {
            owner: Some(raw),
            frames,
        };
        let outcome = (|| {
            let count = catch_unwind(AssertUnwindSafe(|| sink(bytes)))
                .unwrap_or(Err(RenderError::Callback))?;
            if count > length {
                return Err(RenderError::Bound);
            }
            if count == 0 {
                lease.release(0, 2)?;
                return Ok(true);
            }
            lease.release((count / usize::from(self.bytes_per_frame)) as u32, 0)?;
            Ok(false)
        })();
        lease.dispose()?;
        outcome
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../tests/rust/phone_windows_render_pump.rs"
    ));
}
