//! Narrow read-only MMDevice/session ABI. Every interface and allocated string
//! is owned locally, including non-null out parameters returned with failure.
use super::{
    ActionResult, CaptureEndpoint, CaptureObservation, CaptureSession, DesktopActionError,
};
use ivy_contracts::json::JsString;
use std::{
    ffi::c_void,
    marker::PhantomData,
    ptr::{self, NonNull},
    rc::Rc,
};
use windows_sys::core::GUID;
type Raw = *mut c_void;
type Hr = i32;
const DEVICES: GUID = GUID::from_u128(0xa95664d2_9614_4f35_a746_de8db63617e6);
const CLASS: GUID = GUID::from_u128(0xbcde0395_e52f_467c_8e3d_c4579291692e);
const ENDPOINT: GUID = GUID::from_u128(0x1be09788_6894_4089_8586_9a2a6c265ac5);
const MANAGER: GUID = GUID::from_u128(0x77aa99a0_1bd6_484f_8bc7_2c654c9a9b6f);
const SESSION: GUID = GUID::from_u128(0xbfb7ff88_7239_4fc9_8fa2_07c950be9c6d);
#[repr(C)]
#[derive(Clone, Copy)]
struct Unknown {
    query: unsafe extern "system" fn(Raw, *const GUID, *mut Raw) -> Hr,
    add_ref: unsafe extern "system" fn(Raw) -> u32,
    release: unsafe extern "system" fn(Raw) -> u32,
}
#[repr(C)]
struct Devices {
    unknown: Unknown,
    reserved: [usize; 2],
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
#[repr(C)]
struct Manager {
    unknown: Unknown,
    reserved: [usize; 2],
    sessions: unsafe extern "system" fn(Raw, *mut Raw) -> Hr,
}
#[repr(C)]
struct Sessions {
    unknown: Unknown,
    count: unsafe extern "system" fn(Raw, *mut i32) -> Hr,
    at: unsafe extern "system" fn(Raw, i32, *mut Raw) -> Hr,
}
#[repr(C)]
struct Session {
    unknown: Unknown,
    state: unsafe extern "system" fn(Raw, *mut i32) -> Hr,
    reserved: [usize; 9],
    instance: unsafe extern "system" fn(Raw, *mut *mut u16) -> Hr,
    process: unsafe extern "system" fn(Raw, *mut u32) -> Hr,
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
        result: *mut Raw,
    ) -> Hr;
    fn CoTaskMemFree(value: Raw);
}
#[derive(Clone, Copy)]
struct Api {
    initialize: unsafe extern "system" fn(Raw, u32) -> Hr,
    uninitialize: unsafe extern "system" fn(),
    create: unsafe extern "system" fn(*const GUID, Raw, u32, *const GUID, *mut Raw) -> Hr,
    free: unsafe extern "system" fn(Raw),
}
impl Default for Api {
    fn default() -> Self {
        Self {
            initialize: CoInitializeEx,
            uninitialize: CoUninitialize,
            create: CoCreateInstance,
            free: CoTaskMemFree,
        }
    }
}
fn check(hr: Hr) -> ActionResult<()> {
    if hr == 0 {
        Ok(())
    } else {
        Err(DesktopActionError::Unavailable)
    }
}
struct Reference {
    raw: NonNull<c_void>,
    _thread: PhantomData<Rc<()>>,
}
impl Reference {
    unsafe fn table<T>(&self) -> &T {
        // SAFETY: callers use the exact SDK interface acquired for this owner.
        unsafe { &**(self.raw.as_ptr() as *const *const T) }
    }
    fn acquire(call: impl FnOnce(*mut Raw) -> Hr) -> ActionResult<Self> {
        let mut raw = ptr::null_mut();
        let hr = call(&mut raw);
        let owned = NonNull::new(raw).map(|raw| Self {
            raw,
            _thread: PhantomData,
        });
        check(hr)?;
        owned.ok_or(DesktopActionError::Unavailable)
    }
    fn query(&self, iid: &GUID) -> ActionResult<Self> {
        // SAFETY: QueryInterface borrows this owner and initializes one owned out reference.
        Self::acquire(|out| unsafe { (self.table::<Unknown>().query)(self.raw.as_ptr(), iid, out) })
    }
}
impl Drop for Reference {
    fn drop(&mut self) {
        // SAFETY: exactly one release for the acquired interface, on its owning thread.
        unsafe {
            (self.table::<Unknown>().release)(self.raw.as_ptr());
        }
    }
}
struct Wide {
    raw: *mut u16,
    free: unsafe extern "system" fn(Raw),
}
impl Wide {
    fn read(api: Api, call: impl FnOnce(*mut *mut u16) -> Hr) -> ActionResult<JsString> {
        let mut value = Self {
            raw: ptr::null_mut(),
            free: api.free,
        };
        let hr = call(&mut value.raw);
        check(hr)?;
        if value.raw.is_null() {
            return Err(DesktopActionError::Unavailable);
        }
        // SAFETY: LPWSTR is a native NUL-terminated allocation, retained until
        // after its bounded UTF16 copy. No lossy conversion of session IDs.
        for n in 0..=32767 {
            if unsafe { *value.raw.add(n) } == 0 {
                return Ok(JsString(
                    unsafe { std::slice::from_raw_parts(value.raw, n) }.to_vec(),
                ));
            }
        }
        Err(DesktopActionError::Unavailable)
    }
}
impl Drop for Wide {
    fn drop(&mut self) {
        if !self.raw.is_null() {
            // SAFETY: SDK LPWSTR out parameters use the COM task allocator.
            unsafe {
                (self.free)(self.raw.cast());
            }
        }
    }
}
pub struct WindowsCaptureEndpoint {
    devices: Option<Reference>,
    device: Option<Reference>,
    endpoint: Option<Reference>,
    initialized: bool,
    api: Api,
}
impl WindowsCaptureEndpoint {
    pub fn new(id: &JsString) -> ActionResult<Self> {
        Self::with_api(id, Api::default())
    }
    fn with_api(id: &JsString, api: Api) -> ActionResult<Self> {
        super::validate_capture_text(id)?;
        let mut owner = Self {
            devices: None,
            device: None,
            endpoint: None,
            initialized: false,
            api,
        };
        // SAFETY: the owner pairs successful MTA initialization with exactly one
        // uninitialization after releasing all local interfaces, even on error.
        let hr = unsafe { (api.initialize)(ptr::null_mut(), 0) };
        if hr < 0 {
            return Err(DesktopActionError::Unavailable);
        }
        owner.initialized = true;
        owner.devices = Some(Reference::acquire(|out| unsafe {
            (api.create)(&CLASS, ptr::null_mut(), 23, &DEVICES, out)
        })?);
        let devices = owner.devices.as_ref().unwrap();
        let id: Vec<_> = id.0.iter().copied().chain([0]).collect();
        // SAFETY: enumerator and terminated ID live through the SDK call.
        owner.device = Some(Reference::acquire(|out| unsafe {
            (devices.table::<Devices>().get)(devices.raw.as_ptr(), id.as_ptr(), out)
        })?);
        Ok(owner)
    }
    pub fn observe(id: &JsString) -> ActionResult<CaptureObservation> {
        CaptureObservation::read(id, |id| Self::new(id).map(Some))
    }
}
impl CaptureEndpoint for WindowsCaptureEndpoint {
    fn id(&mut self) -> ActionResult<JsString> {
        let device = self
            .device
            .as_ref()
            .ok_or(DesktopActionError::Unavailable)?;
        // SAFETY: the SDK device writes an allocated LPWSTR to the live owner.
        Wide::read(self.api, |out| unsafe {
            (device.table::<Device>().id)(device.raw.as_ptr(), out)
        })
    }
    fn is_active_capture(&mut self) -> ActionResult<bool> {
        let device = self
            .device
            .as_ref()
            .ok_or(DesktopActionError::Unavailable)?;
        let mut state = 0;
        let mut flow = 0;
        // SAFETY: typed SDK methods write into local scalar out parameters.
        check(unsafe { (device.table::<Device>().state)(device.raw.as_ptr(), &mut state) })?;
        if self.endpoint.is_none() {
            self.endpoint = Some(device.query(&ENDPOINT)?);
        }
        let endpoint = self.endpoint.as_ref().unwrap();
        check(unsafe { (endpoint.table::<Endpoint>().flow)(endpoint.raw.as_ptr(), &mut flow) })?;
        Ok(state == 1 && flow == 1)
    }
    fn read_sessions(&mut self) -> ActionResult<Vec<CaptureSession>> {
        let device = self
            .device
            .as_ref()
            .ok_or(DesktopActionError::Unavailable)?;
        // SAFETY: each acquired SDK interface out reference is adopted before
        // inspecting HRESULT and released locally on every later failure.
        let manager = Reference::acquire(|out| unsafe {
            (device.table::<Device>().activate)(
                device.raw.as_ptr(),
                &MANAGER,
                23,
                ptr::null_mut(),
                out,
            )
        })?;
        let sessions = Reference::acquire(|out| unsafe {
            (manager.table::<Manager>().sessions)(manager.raw.as_ptr(), out)
        })?;
        let mut count = 0;
        check(unsafe { (sessions.table::<Sessions>().count)(sessions.raw.as_ptr(), &mut count) })?;
        if !(0..=4096).contains(&count) {
            return Err(DesktopActionError::Unavailable);
        }
        let mut result = Vec::with_capacity(count as usize);
        for index in 0..count {
            let base = Reference::acquire(|out| unsafe {
                (sessions.table::<Sessions>().at)(sessions.raw.as_ptr(), index, out)
            })?;
            let session = base.query(&SESSION)?;
            let id = Wide::read(self.api, |out| unsafe {
                (session.table::<Session>().instance)(session.raw.as_ptr(), out)
            })?;
            let mut pid = 0;
            let mut state = 0;
            let process_result =
                unsafe { (session.table::<Session>().process)(session.raw.as_ptr(), &mut pid) };
            check(unsafe { (session.table::<Session>().state)(session.raw.as_ptr(), &mut state) })?;
            result.push(CaptureSession::new(id, state, pid, process_result)?);
        }
        Ok(result)
    }
    fn close(&mut self) -> ActionResult<()> {
        self.endpoint.take();
        self.device.take();
        self.devices.take();
        if self.initialized {
            self.initialized = false;
            // SAFETY: this owner initialized COM on this same thread.
            unsafe {
                (self.api.uninitialize)();
            }
        }
        Ok(())
    }
}
impl Drop for WindowsCaptureEndpoint {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

#[cfg(test)]
#[path = "../../../../../tests/rust/phone_capture_native.rs"]
pub(crate) mod tests;
