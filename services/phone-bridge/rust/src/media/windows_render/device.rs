//! A fixed render endpoint and its initialized shared/event stream. All native
//! resources remain on the owning COM thread; opening never starts playback.
use super::{
    Api as PumpApi, Event, Hr, Raw, Reference, RenderError, RenderPump, Result, check,
    format::{Encoding, EnginePeriod, MixFormat, RenderBytes},
};
use crate::media::{AudioSettings, windows_audio::WindowsAudioBuffers};
use std::{
    marker::PhantomData,
    ptr,
    rc::Rc,
    sync::{Arc, Mutex, atomic::AtomicBool},
};
use windows_sys::{
    Win32::System::Threading::{CreateEventW, SetEvent},
    core::GUID,
};

const CLASS: GUID = GUID::from_u128(0xbcde0395_e52f_467c_8e3d_c4579291692e);
const DEVICES: GUID = GUID::from_u128(0xa95664d2_9614_4f35_a746_de8db63617e6);
const ENDPOINT: GUID = GUID::from_u128(0x1be09788_6894_4089_8586_9a2a6c265ac5);
const CLIENT: GUID = GUID::from_u128(0x1cb9ad4c_dbfa_4c32_b178_c2f568a703b2);
const CLIENT3: GUID = GUID::from_u128(0x7ed4ee07_8e67_4cd4_8c1a_2b7a5987ad42);
const RENDER: GUID = GUID::from_u128(0xf294acfc_3146_4483_a7bf_addca7c260e2);
const EMPTY: GUID = GUID::from_u128(0);
#[derive(Clone)]
pub(super) struct Request {
    pub endpoint: String,
    pub latency_ms: u16,
    pub low_latency: bool,
}
impl Request {
    pub fn new(settings: &AudioSettings) -> Result<Self> {
        settings
            .validate()
            .map_err(|_| RenderError::Configuration)?;
        Ok(Self {
            endpoint: settings.render_endpoint.clone(),
            latency_ms: settings.render_latency_ms,
            low_latency: settings.enable_wasapi_low_latency,
        })
    }
    fn validate(&self) -> Result<()> {
        if !valid_endpoint(&self.endpoint) || !(2..=100).contains(&self.latency_ms) {
            return Err(RenderError::Configuration);
        }
        Ok(())
    }
}
fn valid_endpoint(value: &str) -> bool {
    !value.trim().is_empty() && !value.contains('\0') && value.encode_utf16().count() <= 32767
}
#[repr(C)]
struct Unknown {
    query: unsafe extern "system" fn(Raw, *const GUID, *mut Raw) -> Hr,
    add: usize,
    release: unsafe extern "system" fn(Raw) -> u32,
}
#[repr(C)]
struct Enumerator {
    unknown: Unknown,
    unused: [usize; 2],
    get: unsafe extern "system" fn(Raw, *const u16, *mut Raw) -> Hr,
}
#[repr(C)]
struct Device {
    unknown: Unknown,
    activate: unsafe extern "system" fn(Raw, *const GUID, u32, Raw, *mut Raw) -> Hr,
    properties: usize,
    id: unsafe extern "system" fn(Raw, *mut *mut u16) -> Hr,
    state: unsafe extern "system" fn(Raw, *mut u32) -> Hr,
}
#[repr(C)]
struct Endpoint {
    unknown: Unknown,
    flow: unsafe extern "system" fn(Raw, *mut i32) -> Hr,
}
#[repr(C, packed(1))]
#[derive(Clone, Copy)]
struct Wave {
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
    initialize: unsafe extern "system" fn(Raw, i32, u32, i64, i64, *const Wave, *const GUID) -> Hr,
    size: unsafe extern "system" fn(Raw, *mut u32) -> Hr,
    latency: unsafe extern "system" fn(Raw, *mut i64) -> Hr,
    padding: unsafe extern "system" fn(Raw, *mut u32) -> Hr,
    supported: usize,
    mix: unsafe extern "system" fn(Raw, *mut *mut Wave) -> Hr,
    period: usize,
    start: unsafe extern "system" fn(Raw) -> Hr,
    stop: unsafe extern "system" fn(Raw) -> Hr,
    reset: unsafe extern "system" fn(Raw) -> Hr,
    event: unsafe extern "system" fn(Raw, Raw) -> Hr,
    service: unsafe extern "system" fn(Raw, *const GUID, *mut Raw) -> Hr,
}
#[repr(C)]
struct Client3 {
    base: Client,
    client2: [usize; 3],
    periods:
        unsafe extern "system" fn(Raw, *const Wave, *mut u32, *mut u32, *mut u32, *mut u32) -> Hr,
    current: usize,
    initialize: unsafe extern "system" fn(Raw, u32, u32, *const Wave, *const GUID) -> Hr,
}
#[link(name = "ole32")]
unsafe extern "system" {
    fn CoInitializeEx(reserved: Raw, mode: u32) -> Hr;
    fn CoUninitialize();
    fn CoCreateInstance(
        class: *const GUID,
        outer: Raw,
        context: u32,
        iid: *const GUID,
        out: *mut Raw,
    ) -> Hr;
    fn CoTaskMemFree(value: Raw);
}
unsafe extern "system" fn event(manual: bool) -> Raw {
    unsafe { CreateEventW(ptr::null(), i32::from(manual), 0, ptr::null()) as Raw }
}
unsafe extern "system" fn signal(event: Raw) -> i32 {
    unsafe { SetEvent(event as _) }
}
#[derive(Clone, Copy)]
struct Api {
    initialize: unsafe extern "system" fn(Raw, u32) -> Hr,
    uninitialize: unsafe extern "system" fn(),
    create: unsafe extern "system" fn(*const GUID, Raw, u32, *const GUID, *mut Raw) -> Hr,
    free: unsafe extern "system" fn(Raw),
    event: unsafe extern "system" fn(bool) -> Raw,
    signal: unsafe extern "system" fn(Raw) -> i32,
    pump: PumpApi,
}
impl Default for Api {
    fn default() -> Self {
        Self {
            initialize: CoInitializeEx,
            uninitialize: CoUninitialize,
            create: CoCreateInstance,
            free: CoTaskMemFree,
            event,
            signal,
            pump: PumpApi::default(),
        }
    }
}
struct Apartment {
    api: Api,
    _thread: PhantomData<Rc<()>>,
}
impl Apartment {
    fn new(api: Api) -> Result<Self> {
        check(
            unsafe { (api.initialize)(ptr::null_mut(), 0) },
            RenderError::Client,
        )?;
        Ok(Self {
            api,
            _thread: PhantomData,
        })
    }
}
impl Drop for Apartment {
    fn drop(&mut self) {
        unsafe {
            (self.api.uninitialize)();
        }
    }
}
struct Allocation {
    raw: Raw,
    api: Api,
}
impl Drop for Allocation {
    fn drop(&mut self) {
        if !self.raw.is_null() {
            unsafe {
                (self.api.free)(self.raw);
            }
        }
    }
}
fn acquire(error: RenderError, call: impl FnOnce(*mut Raw) -> Hr) -> Result<Reference> {
    let mut raw = ptr::null_mut();
    let hr = call(&mut raw);
    let owner = Reference::adopt(raw);
    check(hr, error)?;
    owner.ok_or(error)
}
fn query(owner: &Reference, iid: &GUID) -> Result<Reference> {
    acquire(RenderError::Client, |out| unsafe {
        (owner.table::<Unknown>().query)(owner.raw.as_ptr(), iid, out)
    })
}
fn into_raw(owner: Reference) -> Raw {
    let raw = owner.raw.as_ptr();
    std::mem::forget(owner);
    raw
}
fn into_event(event: Event) -> Raw {
    let raw = event.raw.as_ptr();
    std::mem::forget(event);
    raw
}
fn create_event(api: Api, manual: bool) -> Result<Event> {
    std::ptr::NonNull::new(unsafe { (api.event)(manual) })
        .map(|raw| Event { raw, api: api.pump })
        .ok_or(RenderError::Event)
}
fn endpoint_id(api: Api, device: &Reference) -> Result<String> {
    let mut value = Allocation {
        raw: ptr::null_mut(),
        api,
    };
    check(
        unsafe {
            (device.table::<Device>().id)(device.raw.as_ptr(), (&mut value.raw as *mut Raw).cast())
        },
        RenderError::Client,
    )?;
    if !value.raw.is_null() {
        let wide = value.raw.cast::<u16>();
        for n in 0..=32767 {
            if unsafe { *wide.add(n) } == 0 {
                return String::from_utf16(unsafe { std::slice::from_raw_parts(wide, n) })
                    .map_err(|_| RenderError::Client);
            }
        }
    }
    Err(RenderError::Client)
}
// Word storage preserves WAVEFORMATEX's two-byte alignment. The native API owns
// and guarantees the returned format extent; all allocations are freed on error.
struct Format {
    words: Vec<u16>,
}
impl Format {
    fn raw(&self) -> *const Wave {
        self.words.as_ptr().cast()
    }
    fn header(&self) -> Wave {
        unsafe { ptr::read_unaligned(self.raw()) }
    }
    fn bytes(&self) -> &[u8] {
        unsafe {
            std::slice::from_raw_parts(
                self.words.as_ptr().cast(),
                18 + usize::from(self.header().extra),
            )
        }
    }
    fn from_bytes(bytes: &[u8]) -> Self {
        let mut value = Self {
            words: vec![0; bytes.len().div_ceil(2)],
        };
        unsafe {
            ptr::copy_nonoverlapping(bytes.as_ptr(), value.words.as_mut_ptr().cast(), bytes.len());
        }
        value
    }
    fn float(rate: u32, channels: u16) -> Self {
        let wave = Wave {
            tag: 3,
            channels,
            rate,
            bytes: rate.wrapping_mul(u32::from(channels) * 4),
            align: channels * 4,
            bits: 32,
            extra: 0,
        };
        Self::from_bytes(unsafe { std::slice::from_raw_parts((&wave as *const Wave).cast(), 18) })
    }
    fn read(api: Api, client: &Reference) -> Result<Self> {
        let mut value = Allocation {
            raw: ptr::null_mut(),
            api,
        };
        check(
            unsafe {
                (client.table::<Client>().mix)(
                    client.raw.as_ptr(),
                    (&mut value.raw as *mut Raw).cast(),
                )
            },
            RenderError::Mix,
        )?;
        if value.raw.is_null() {
            return Err(RenderError::Mix);
        }
        let mut wave = unsafe { ptr::read_unaligned(value.raw.cast::<Wave>()) };
        // Published MarshalFromPtr ignores unreliable PCM cbSize. Extensible
        // requires its entire fixed extension before any subtype can be read.
        if wave.tag == 1 {
            wave.extra = 0;
        }
        if wave.tag == 65534 && wave.extra < 22 {
            return Err(RenderError::Mix);
        }
        let mut format = Self::from_bytes(unsafe {
            std::slice::from_raw_parts(value.raw.cast(), 18 + usize::from(wave.extra))
        });
        unsafe {
            ptr::write_unaligned(format.words.as_mut_ptr().cast::<Wave>(), wave);
        }
        Ok(format)
    }
    fn adaptation(&self) -> Option<Encoding> {
        let wave = self.header();
        let subtype = if wave.tag == 65534 {
            let guid = unsafe {
                ptr::read_unaligned(self.words.as_ptr().cast::<u8>().add(24).cast::<GUID>())
            };
            (u128::from(guid.data1) << 96)
                | (u128::from(guid.data2) << 80)
                | (u128::from(guid.data3) << 64)
                | u128::from(u64::from_be_bytes(guid.data4))
        } else {
            0
        };
        MixFormat {
            rate: wave.rate,
            tag: wave.tag,
            bits: wave.bits,
            subtype,
        }
        .adaptation(wave.rate)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Unavailable {
    Client3,
    Format,
    Engine,
}
#[derive(Clone, Debug)]
pub(super) struct Info {
    pub rate: u32,
    pub channels: u16,
    pub latency_ms: u32,
    pub low_latency: bool,
    pub unavailable: Option<Unavailable>,
    pub mix: Vec<u8>,
}
struct SignalState {
    handle: Option<usize>,
    set: unsafe extern "system" fn(Raw) -> i32,
}
/// Only SetEvent crosses threads. The owner revokes this capability under the
/// same lock before closing the event; no COM pointer is made Send or shared.
#[derive(Clone)]
pub(super) struct StopSignal(Arc<Mutex<SignalState>>);
impl StopSignal {
    /// The owner must revoke before closing the handle. The setter must neither
    /// retain it nor reenter this capability. Native SetEvent meets that contract.
    pub(super) unsafe fn new(handle: usize, set: unsafe extern "system" fn(Raw) -> i32) -> Self {
        Self(Arc::new(Mutex::new(SignalState {
            handle: Some(handle),
            set,
        })))
    }
    pub(super) fn set(&self) -> Result<()> {
        let state = self.0.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(handle) = state.handle {
            if unsafe { (state.set)(handle as Raw) } == 0 {
                return Err(RenderError::Signal);
            }
        }
        Ok(())
    }
    pub(super) fn revoke(&self) {
        self.0.lock().unwrap_or_else(|p| p.into_inner()).handle = None;
    }
}
struct Opened {
    client: Option<Reference>,
    stop: Option<Event>,
    frame: Option<Event>,
    device: Reference,
    api: Api,
    _apartment: Apartment,
}
impl Opened {
    fn open(request: &Request, api: Api) -> Result<Self> {
        Self::admit(request, None, api)?.initialize()
    }
    fn admit(request: &Request, capture: Option<&str>, api: Api) -> Result<Self> {
        request.validate()?;
        if capture.is_some_and(|id| !valid_endpoint(id) || id == request.endpoint) {
            return Err(RenderError::Configuration);
        }
        let apartment = Apartment::new(api)?;
        let enumerator = acquire(RenderError::Client, |out| unsafe {
            (api.create)(&CLASS, ptr::null_mut(), 1, &DEVICES, out)
        })?;
        // The full route checks its configured capture endpoint even for process
        // capture. Hold that view through render admission, as the managed route
        // does; neither client activation nor mute work belongs to this phase.
        let capture = capture
            .map(|id| Self::endpoint(&enumerator, id, 1, api))
            .transpose()?;
        let device = Self::endpoint(&enumerator, &request.endpoint, 0, api)?;
        drop(capture);
        drop(enumerator);
        Ok(Self {
            client: None,
            stop: None,
            frame: None,
            device,
            api,
            _apartment: apartment,
        })
    }
    fn endpoint(
        enumerator: &Reference,
        id: &str,
        expected_flow: i32,
        api: Api,
    ) -> Result<Reference> {
        let wide: Vec<u16> = id.encode_utf16().chain(Some(0)).collect();
        let device = acquire(RenderError::Client, |out| unsafe {
            (enumerator.table::<Enumerator>().get)(enumerator.raw.as_ptr(), wide.as_ptr(), out)
        })?;
        if endpoint_id(api, &device)? != id {
            return Err(RenderError::Owner);
        }
        let endpoint = query(&device, &ENDPOINT)?;
        let mut flow = -1;
        check(
            unsafe { (endpoint.table::<Endpoint>().flow)(endpoint.raw.as_ptr(), &mut flow) },
            RenderError::Client,
        )?;
        if flow != expected_flow {
            return Err(RenderError::Owner);
        }
        let mut state = 0;
        check(
            unsafe { (device.table::<Device>().state)(device.raw.as_ptr(), &mut state) },
            RenderError::Client,
        )?;
        if state != 1 {
            return Err(RenderError::Owner);
        }
        drop(endpoint);
        Ok(device)
    }
    fn initialize(mut self) -> Result<Self> {
        self.initialize_parts()?;
        Ok(self)
    }
    fn initialize_parts(&mut self) -> Result<()> {
        self.stop = Some(create_event(self.api, true)?);
        self.client = Some(self.activate()?);
        Ok(())
    }
    fn activate(&self) -> Result<Reference> {
        acquire(RenderError::Client, |out| unsafe {
            (self.device.table::<Device>().activate)(
                self.device.raw.as_ptr(),
                &CLIENT,
                23,
                ptr::null_mut(),
                out,
            )
        })
    }
    fn configure(mut self, request: &Request) -> Result<NativeStream> {
        let mut stream = self.configure_parts(request)?;
        stream._opened = Some(self);
        Ok(stream)
    }
    fn configure_parts(&mut self, request: &Request) -> Result<NativeStream> {
        let mix = Format::read(self.api, self.client.as_ref().unwrap())?;
        let wave = mix.header();
        if wave.rate == 0 || wave.rate > i32::MAX as u32 || !matches!(wave.channels, 1 | 2) {
            return Err(RenderError::Configuration);
        }
        let source = Format::float(wave.rate, wave.channels);
        let mut encoding = Encoding::Float32;
        let mut latency = i32::from(request.latency_ms);
        let mut low_latency = false;
        let mut unavailable = None;
        if request.low_latency {
            let client3 = query(self.client.as_ref().unwrap(), &CLIENT3).ok();
            if let Some(client3) = client3 {
                if let Some(adapted) = mix.adaptation() {
                    let mut period = EnginePeriod {
                        default: 0,
                        fundamental: 0,
                        minimum: 0,
                        maximum: 0,
                    };
                    let hr = unsafe {
                        (client3.table::<Client3>().periods)(
                            client3.raw.as_ptr(),
                            mix.raw(),
                            &mut period.default,
                            &mut period.fundamental,
                            &mut period.minimum,
                            &mut period.maximum,
                        )
                    };
                    if hr >= 0 {
                        let frames = period.choose();
                        let hr = unsafe {
                            (client3.table::<Client3>().initialize)(
                                client3.raw.as_ptr(),
                                0x40000,
                                frames,
                                mix.raw(),
                                &EMPTY,
                            )
                        };
                        if hr >= 0 {
                            low_latency = true;
                            encoding = adapted;
                            latency =
                                ((i64::from(frames) * 1000 / i64::from(wave.rate)) as i32).max(1);
                        } else {
                            // Only declined initialization can leave the client partially
                            // initialized. Release every view before activating its replacement.
                            drop(client3);
                            drop(self.client.take());
                            self.client = Some(self.activate()?);
                            unavailable =
                                Some(if query(self.client.as_ref().unwrap(), &CLIENT3).is_ok() {
                                    Unavailable::Engine
                                } else {
                                    Unavailable::Client3
                                });
                        }
                    } else {
                        unavailable = Some(Unavailable::Engine);
                    }
                } else {
                    unavailable = Some(Unavailable::Format);
                }
            } else {
                unavailable = Some(Unavailable::Client3);
            }
        }
        let client = self.client.as_ref().unwrap();
        if !low_latency {
            check(
                unsafe {
                    (client.table::<Client>().initialize)(
                        client.raw.as_ptr(),
                        0,
                        0x88040000,
                        i64::from(request.latency_ms) * 10000,
                        0,
                        source.raw(),
                        &EMPTY,
                    )
                },
                RenderError::Initialize,
            )?;
            let mut stream_latency = 0;
            check(
                unsafe {
                    (client.table::<Client>().latency)(client.raw.as_ptr(), &mut stream_latency)
                },
                RenderError::Latency,
            )?;
            if stream_latency != 0 {
                latency = (stream_latency / 10000) as i32;
            }
        }
        self.frame = Some(create_event(self.api, false)?);
        check(
            unsafe {
                (client.table::<Client>().event)(
                    client.raw.as_ptr(),
                    self.frame.as_ref().unwrap().raw.as_ptr(),
                )
            },
            RenderError::BindEvent,
        )?;
        let render = acquire(RenderError::Service, |out| unsafe {
            (client.table::<Client>().service)(client.raw.as_ptr(), &RENDER, out)
        })?;
        let align = if low_latency {
            wave.align
        } else {
            wave.channels * 4
        };
        if align != wave.channels * encoding.width() as u16 {
            return Err(RenderError::Configuration);
        }
        let source = RenderBytes::new(wave.rate, wave.channels, encoding)
            .map_err(|_| RenderError::Configuration)?;
        let info = Info {
            rate: wave.rate,
            channels: wave.channels,
            latency_ms: u32::try_from(latency).map_err(|_| RenderError::Bound)?,
            low_latency,
            unavailable,
            mix: mix.bytes().to_vec(),
        };
        let pump = unsafe {
            RenderPump::prepare(
                into_raw(self.client.take().unwrap()),
                into_raw(render),
                into_event(self.frame.take().unwrap()),
                into_event(self.stop.take().unwrap()),
                align,
                info.latency_ms,
                self.api.pump,
            )
        }?;
        let stop = unsafe {
            StopSignal::new(
                pump.stop_event.as_ref().unwrap().raw.as_ptr() as usize,
                self.api.signal,
            )
        };
        Ok(NativeStream {
            pump,
            source,
            info,
            stop,
            _opened: None,
        })
    }
}
pub(super) struct NativeStream {
    pump: RenderPump,
    source: RenderBytes,
    pub info: Info,
    stop: StopSignal,
    _opened: Option<Opened>,
}
impl NativeStream {
    pub fn stop_signal(&self) -> StopSignal {
        self.stop.clone()
    }
    pub fn run(&mut self, stop: &AtomicBool, buffers: &Mutex<WindowsAudioBuffers>) -> Result<()> {
        self.pump.run(stop, &mut |bytes| {
            Ok(self.source.read(
                &mut buffers.lock().unwrap_or_else(|p| p.into_inner()),
                bytes,
            ))
        })
    }
}
impl Drop for NativeStream {
    fn drop(&mut self) {
        self.stop.revoke();
    }
}
pub(super) fn open(request: &Request) -> Result<NativeStream> {
    Opened::open(request, Api::default())?.configure(request)
}
/// Stream retirement precedes mute cleanup. This owner keeps the endpoint and
/// apartment on their original thread until the route's final close.
pub(super) struct Admission {
    opened: Opened,
    request: Request,
}
impl Admission {
    pub fn configure(&mut self) -> Result<NativeStream> {
        self.opened.initialize_parts()?;
        self.opened.configure_parts(&self.request)
    }
    pub fn retire(&mut self) {
        drop(self.opened.client.take());
        drop(self.opened.stop.take());
        drop(self.opened.frame.take());
    }
}
pub(super) fn admit(request: &Request, capture: &str) -> Result<Admission> {
    Ok(Admission {
        opened: Opened::admit(request, Some(capture), Api::default())?,
        request: request.clone(),
    })
}
#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../tests/rust/phone_windows_render_device.rs"
    ));
}
