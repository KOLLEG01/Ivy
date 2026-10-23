//! Exact capture source activation. Every interface and the COM apartment stay
//! with the worker until its explicit disposal; no default endpoint is chosen.
use super::{
    CaptureError, CaptureMode, CapturePump, CaptureSink, CaptureStream, GUID, Hr, Raw, Reference,
    Result, check,
};
use crate::media::{AudioDesktopIdentity, AudioSettings};
use std::{marker::PhantomData, ptr, rc::Rc, sync::atomic::AtomicBool};

const CLASS: GUID = GUID::from_u128(0xbcde0395_e52f_467c_8e3d_c4579291692e);
const DEVICES: GUID = GUID::from_u128(0xa95664d2_9614_4f35_a746_de8db63617e6);
const ENDPOINT: GUID = GUID::from_u128(0x1be09788_6894_4089_8586_9a2a6c265ac5);
const CLIENT: GUID = GUID::from_u128(0x1cb9ad4c_dbfa_4c32_b178_c2f568a703b2);
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Source {
    Microphone(String),
    Loopback(String),
    Process { pid: u32, include_tree: bool },
}
#[derive(Clone)]
pub(super) struct Request {
    pub source: Source,
    pub buffer_ms: u16,
    pub desktop: Option<AudioDesktopIdentity>,
}
impl Request {
    pub fn new(settings: &AudioSettings, desktop: Option<&AudioDesktopIdentity>) -> Result<Self> {
        settings
            .validate()
            .map_err(|_| CaptureError::Configuration)?;
        let source = match settings.source_mode.as_str() {
            "microphone" => Source::Microphone(settings.capture_endpoint.clone()),
            "virtual_speaker" => Source::Loopback(
                settings
                    .loopback_render_endpoint
                    .clone()
                    .ok_or(CaptureError::Configuration)?,
            ),
            "system_excluding_runtime" => Source::Process {
                pid: std::process::id(),
                include_tree: false,
            },
            "desktop_process" => Source::Process {
                pid: desktop.ok_or(CaptureError::Owner)?.pid() as u32,
                include_tree: true,
            },
            _ => return Err(CaptureError::Configuration),
        };
        Ok(Self {
            source,
            buffer_ms: settings.capture_buffer_ms,
            desktop: desktop.cloned(),
        })
    }
    fn validate(&self) -> Result<()> {
        if !(10..=100).contains(&self.buffer_ms) {
            return Err(CaptureError::Configuration);
        }
        match &self.source {
            Source::Microphone(id) | Source::Loopback(id)
                if id.trim().is_empty()
                    || id.contains('\0')
                    || id.encode_utf16().count() > 32767 =>
            {
                Err(CaptureError::Configuration)
            }
            Source::Process { pid: 0, .. } => Err(CaptureError::Configuration),
            _ => Ok(()),
        }
    }
    fn mode(&self) -> CaptureMode {
        match self.source {
            Source::Microphone(_) => CaptureMode::Microphone,
            Source::Loopback(_) => CaptureMode::EndpointLoopback,
            Source::Process { .. } => CaptureMode::ProcessLoopback,
        }
    }
}
#[repr(C)]
struct Enumerator {
    unknown: super::Unknown,
    unused: [usize; 2],
    get: unsafe extern "system" fn(Raw, *const u16, *mut Raw) -> Hr,
}
#[repr(C)]
struct Device {
    unknown: super::Unknown,
    activate: unsafe extern "system" fn(Raw, *const GUID, u32, Raw, *mut Raw) -> Hr,
    properties: usize,
    id: unsafe extern "system" fn(Raw, *mut *mut u16) -> Hr,
    state: unsafe extern "system" fn(Raw, *mut u32) -> Hr,
}
#[repr(C)]
struct Endpoint {
    unknown: super::Unknown,
    flow: unsafe extern "system" fn(Raw, *mut i32) -> Hr,
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
fn native_process(pid: u32, include: bool, out: *mut Raw) -> Hr {
    match unsafe { crate::loopback_probe::windows::activate_audio_client(pid, include) } {
        Ok(raw) => {
            unsafe {
                *out = raw;
            }
            0
        }
        Err(_) => -1,
    }
}
#[derive(Clone, Copy)]
struct Api {
    initialize: unsafe extern "system" fn(Raw, u32) -> Hr,
    uninitialize: unsafe extern "system" fn(),
    create: unsafe extern "system" fn(*const GUID, Raw, u32, *const GUID, *mut Raw) -> Hr,
    free: unsafe extern "system" fn(Raw),
    process: fn(u32, bool, *mut Raw) -> Hr,
    supported: fn() -> bool,
}
impl Default for Api {
    fn default() -> Self {
        Self {
            initialize: CoInitializeEx,
            uninitialize: CoUninitialize,
            create: CoCreateInstance,
            free: CoTaskMemFree,
            process: native_process,
            supported: crate::loopback_probe::windows::process_loopback_supported,
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
            CaptureError::Client,
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
struct Allocated {
    raw: *mut u16,
    api: Api,
}
impl Drop for Allocated {
    fn drop(&mut self) {
        if !self.raw.is_null() {
            unsafe {
                (self.api.free)(self.raw.cast());
            }
        }
    }
}
fn endpoint_id(api: Api, device: &Reference) -> Result<String> {
    let mut value = Allocated {
        raw: ptr::null_mut(),
        api,
    };
    check(
        unsafe { (device.table::<Device>().id)(device.raw.as_ptr(), &mut value.raw) },
        CaptureError::Client,
    )?;
    if !value.raw.is_null() {
        for n in 0..=32767 {
            if unsafe { *value.raw.add(n) } == 0 {
                return String::from_utf16(unsafe { std::slice::from_raw_parts(value.raw, n) })
                    .map_err(|_| CaptureError::Client);
            }
        }
    }
    Err(CaptureError::Client)
}
struct Opened {
    client: Option<Reference>,
    device: Option<Reference>,
    _apartment: Apartment,
}
impl Opened {
    fn open(request: &Request, api: Api) -> Result<Self> {
        let mut opened = Self::admit(request, api)?;
        opened.activate(request)?;
        Ok(opened)
    }
    fn admit(request: &Request, api: Api) -> Result<Self> {
        let (opened, validation) = Self::admit_retained(request, api)?;
        validation?;
        Ok(opened)
    }
    fn admit_retained(request: &Request, api: Api) -> Result<(Self, Result<()>)> {
        request.validate()?;
        if matches!(request.source, Source::Process { .. }) && !(api.supported)() {
            return Err(CaptureError::Unavailable);
        }
        let apartment = Apartment::new(api)?;
        let mut opened = Self {
            client: None,
            device: None,
            _apartment: apartment,
        };
        let validation = opened.find_device(request);
        Ok((opened, validation))
    }
    fn find_device(&mut self, request: &Request) -> Result<()> {
        let api = self._apartment.api;
        match &request.source {
            Source::Process { .. } => {}
            Source::Microphone(id) | Source::Loopback(id) => {
                let enumerator = Reference::acquire(CaptureError::Client, |out| unsafe {
                    (api.create)(&CLASS, ptr::null_mut(), 1, &DEVICES, out)
                })?;
                let wide: Vec<u16> = id.encode_utf16().chain(Some(0)).collect();
                let device = Reference::acquire(CaptureError::Client, |out| unsafe {
                    (enumerator.table::<Enumerator>().get)(
                        enumerator.raw.as_ptr(),
                        wide.as_ptr(),
                        out,
                    )
                })?;
                // The route owns GetDevice's result before validation. Even a
                // validation failure must retain it through mute cleanup.
                self.device = Some(device);
                let device = self.device.as_ref().unwrap();
                if endpoint_id(api, &device)? != *id {
                    return Err(CaptureError::Owner);
                }
                let endpoint = device.query(&ENDPOINT)?;
                let mut flow = -1;
                check(
                    unsafe {
                        (endpoint.table::<Endpoint>().flow)(endpoint.raw.as_ptr(), &mut flow)
                    },
                    CaptureError::Client,
                )?;
                let expected = if matches!(request.source, Source::Microphone(_)) {
                    1
                } else {
                    0
                };
                if flow != expected {
                    return Err(CaptureError::Owner);
                }
                let mut state = 0;
                check(
                    unsafe { (device.table::<Device>().state)(device.raw.as_ptr(), &mut state) },
                    CaptureError::Client,
                )?;
                if state != 1 {
                    return Err(CaptureError::Owner);
                }
            }
        }
        Ok(())
    }
    fn activate(&mut self, request: &Request) -> Result<()> {
        self.client = Some(match request.source {
            Source::Process { pid, include_tree } => {
                Reference::acquire(CaptureError::Client, |out| {
                    (self._apartment.api.process)(pid, include_tree, out)
                })?
            }
            _ => {
                let device = self.device.as_ref().unwrap();
                Reference::acquire(CaptureError::Client, |out| unsafe {
                    (device.table::<Device>().activate)(
                        device.raw.as_ptr(),
                        &CLIENT,
                        23,
                        ptr::null_mut(),
                        out,
                    )
                })?
            }
        });
        Ok(())
    }
    fn prepare(
        mut self,
        request: &Request,
        build: impl FnOnce(Reference, CaptureMode, u16) -> Result<CapturePump>,
    ) -> Result<NativeStream> {
        let mut stream = self.prepare_parts(request, build)?;
        stream._opened = Some(self);
        Ok(stream)
    }
    fn prepare_parts(
        &mut self,
        request: &Request,
        build: impl FnOnce(Reference, CaptureMode, u16) -> Result<CapturePump>,
    ) -> Result<NativeStream> {
        let client = self.client.take().unwrap();
        let pump = build(client, request.mode(), request.buffer_ms)?;
        Ok(NativeStream {
            pump,
            _opened: None,
        })
    }
}
struct NativeStream {
    pump: CapturePump,
    _opened: Option<Opened>,
}
impl CaptureStream for NativeStream {
    fn run(&mut self, stop: &AtomicBool, sink: &mut CaptureSink<'_>) -> Result<()> {
        self.pump.run_retained(stop, sink)
    }
}
pub(super) fn open(request: &Request) -> Result<Box<dyn CaptureStream>> {
    Ok(Box::new(
        Opened::open(request, Api::default())?.prepare(request, |client, mode, ms| unsafe {
            CapturePump::from_owned_client(client.into_raw(), mode, ms)
        })?,
    ))
}
pub(super) struct Admission {
    opened: Opened,
    request: Request,
    validation: Result<()>,
}
impl Admission {
    pub fn configure(&mut self) -> Result<Box<dyn CaptureStream>> {
        self.configure_with(|client, mode, ms| unsafe {
            CapturePump::from_owned_client(client.into_raw(), mode, ms)
        })
    }
    fn configure_with(
        &mut self,
        build: impl FnOnce(Reference, CaptureMode, u16) -> Result<CapturePump>,
    ) -> Result<Box<dyn CaptureStream>> {
        self.validation?;
        self.opened.activate(&self.request)?;
        Ok(Box::new(self.opened.prepare_parts(&self.request, build)?))
    }
    pub fn retire(&mut self) {
        drop(self.opened.client.take());
    }
}
pub(super) fn admit(request: &Request) -> Result<Admission> {
    let (opened, validation) = Opened::admit_retained(request, Api::default())?;
    Ok(Admission {
        opened,
        validation,
        request: request.clone(),
    })
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../tests/rust/phone_windows_capture_device.rs"
    ));
}
