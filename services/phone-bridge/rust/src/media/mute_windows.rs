//! Exact MMDevice mute-policy adapter. It opens only the requested endpoint,
//! creates volume control lazily and releases every interface on its COM thread.
use super::{AudioMuteEndpoint, CallAudioError, Result};
use std::{
    cell::RefCell,
    ffi::c_void,
    marker::PhantomData,
    ptr::{self, NonNull},
    rc::Rc,
    sync::atomic::{AtomicU32, Ordering, fence},
};
use windows_sys::core::GUID;
type Raw = *mut c_void;
type Hr = i32;
const CLASS: GUID = GUID::from_u128(0xbcde0395_e52f_467c_8e3d_c4579291692e);
const DEVICES: GUID = GUID::from_u128(0xa95664d2_9614_4f35_a746_de8db63617e6);
const ENDPOINT: GUID = GUID::from_u128(0x1be09788_6894_4089_8586_9a2a6c265ac5);
const VOLUME: GUID = GUID::from_u128(0x5cdf2c82_841e_4546_9722_0cf74078229a);
const NOTIFY: GUID = GUID::from_u128(0x657804fa_d6ad_4496_8a60_352752af4f89);
const UNKNOWN: GUID = GUID::from_u128(0x00000000_0000_0000_c000_000000000046);
#[repr(C)]
struct Unknown {
    query: unsafe extern "system" fn(Raw, *const GUID, *mut Raw) -> Hr,
    add_ref: unsafe extern "system" fn(Raw) -> u32,
    release: unsafe extern "system" fn(Raw) -> u32,
}
#[repr(C)]
struct Enumerator {
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
struct VolumeTable {
    unknown: Unknown,
    register: unsafe extern "system" fn(Raw, Raw) -> Hr,
    unregister: unsafe extern "system" fn(Raw, Raw) -> Hr,
    channels: unsafe extern "system" fn(Raw, *mut i32) -> Hr,
    unused: [usize; 8],
    set_mute: unsafe extern "system" fn(Raw, i32, *const GUID) -> Hr,
    mute: unsafe extern "system" fn(Raw, *mut i32) -> Hr,
    steps: unsafe extern "system" fn(Raw, *mut u32, *mut u32) -> Hr,
    step_changes: [usize; 2],
    hardware: unsafe extern "system" fn(Raw, *mut u32) -> Hr,
    range: unsafe extern "system" fn(Raw, *mut f32, *mut f32, *mut f32) -> Hr,
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
fn check(hr: Hr) -> Result<()> {
    if hr >= 0 {
        Ok(())
    } else {
        Err(CallAudioError::RouteFailed)
    }
}
struct Reference {
    raw: NonNull<c_void>,
    _thread: PhantomData<Rc<()>>,
}
impl Reference {
    unsafe fn table<T>(&self) -> &T {
        unsafe { &**(self.raw.as_ptr().cast::<*const T>()) }
    }
    fn acquire(call: impl FnOnce(*mut Raw) -> Hr) -> Result<Self> {
        let mut out = ptr::null_mut();
        let hr = call(&mut out);
        let owner = NonNull::new(out).map(|raw| Self {
            raw,
            _thread: PhantomData,
        });
        check(hr)?;
        owner.ok_or(CallAudioError::RouteFailed)
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
// A changed mode means the caller already owns a different COM apartment.
// Endpoint interfaces stay on that thread; only successful init calls are balanced.
struct Apartment {
    api: Api,
    owned: bool,
}
impl Apartment {
    fn new(api: Api) -> Result<Self> {
        let hr = unsafe { (api.initialize)(ptr::null_mut(), 0) };
        if hr == 0x80010106u32 as i32 {
            return Ok(Self { api, owned: false });
        }
        check(hr)?;
        Ok(Self { api, owned: true })
    }
}
impl Drop for Apartment {
    fn drop(&mut self) {
        if self.owned {
            unsafe { (self.api.uninitialize)() }
        }
    }
}
struct Allocated {
    raw: *mut u16,
    api: Api,
}
impl Allocated {
    fn read(api: Api, call: impl FnOnce(*mut *mut u16) -> Hr) -> Result<String> {
        let mut value = Self {
            raw: ptr::null_mut(),
            api,
        };
        check(call(&mut value.raw))?;
        if !value.raw.is_null() {
            for n in 0..=32767 {
                if unsafe { *value.raw.add(n) } == 0 {
                    return String::from_utf16(unsafe { std::slice::from_raw_parts(value.raw, n) })
                        .map_err(|_| CallAudioError::RouteFailed);
                }
            }
        }
        Err(CallAudioError::RouteFailed)
    }
}
impl Drop for Allocated {
    fn drop(&mut self) {
        if !self.raw.is_null() {
            unsafe { (self.api.free)(self.raw.cast()) }
        }
    }
}
#[repr(C)]
struct NotifyTable {
    unknown: Unknown,
    notify: unsafe extern "system" fn(Raw, Raw) -> Hr,
}
#[repr(C)]
struct Notification {
    table: *const NotifyTable,
    references: AtomicU32,
}
fn equal(a: &GUID, b: &GUID) -> bool {
    a.data1 == b.data1 && a.data2 == b.data2 && a.data3 == b.data3 && a.data4 == b.data4
}
unsafe extern "system" fn notify_query(raw: Raw, iid: *const GUID, out: *mut Raw) -> Hr {
    if out.is_null() {
        return 0x80004003u32 as i32;
    }
    unsafe { *out = ptr::null_mut() };
    if !iid.is_null() && (equal(unsafe { &*iid }, &UNKNOWN) || equal(unsafe { &*iid }, &NOTIFY)) {
        unsafe {
            *out = raw;
            notify_add(raw)
        };
        0
    } else {
        0x80004002u32 as i32
    }
}
unsafe extern "system" fn notify_add(raw: Raw) -> u32 {
    unsafe { &*raw.cast::<Notification>() }
        .references
        .fetch_add(1, Ordering::Relaxed)
        + 1
}
unsafe extern "system" fn notify_release(raw: Raw) -> u32 {
    let n = unsafe { &*raw.cast::<Notification>() }
        .references
        .fetch_sub(1, Ordering::Release)
        - 1;
    if n == 0 {
        fence(Ordering::Acquire);
        drop(unsafe { Box::from_raw(raw.cast::<Notification>()) })
    }
    n
}
unsafe extern "system" fn notify_no_listener(_: Raw, _: Raw) -> Hr {
    0
}
static NOTIFICATION: NotifyTable = NotifyTable {
    unknown: Unknown {
        query: notify_query,
        add_ref: notify_add,
        release: notify_release,
    },
    notify: notify_no_listener,
};
struct Volume {
    raw: Reference,
    callback: Option<Reference>,
    registered: bool,
}
impl Volume {
    fn new(device: &Reference) -> Result<Self> {
        let raw = Reference::acquire(|out| unsafe {
            (device.table::<Device>().activate)(
                device.raw.as_ptr(),
                &VOLUME,
                23,
                ptr::null_mut(),
                out,
            )
        })?;
        let mut volume = Self {
            raw,
            callback: None,
            registered: false,
        };
        let table = unsafe { volume.raw.table::<VolumeTable>() };
        let r = volume.raw.raw.as_ptr();
        let (
            mut channels,
            mut step,
            mut steps,
            mut hardware,
            mut minimum,
            mut maximum,
            mut increment,
        ) = (0, 0, 0, 0, 0.0, 0.0, 0.0);
        check(unsafe { (table.channels)(r, &mut channels) })?;
        if channels < 0 {
            return Err(CallAudioError::RouteFailed);
        }
        check(unsafe { (table.steps)(r, &mut step, &mut steps) })?;
        check(unsafe { (table.hardware)(r, &mut hardware) })?;
        check(unsafe { (table.range)(r, &mut minimum, &mut maximum, &mut increment) })?;
        let callback = Box::into_raw(Box::new(Notification {
            table: &NOTIFICATION,
            references: AtomicU32::new(1),
        }))
        .cast();
        volume.callback = Some(Reference {
            raw: NonNull::new(callback).unwrap(),
            _thread: PhantomData,
        });
        check(unsafe { (volume.raw.table::<VolumeTable>().register)(r, callback) })?;
        volume.registered = true;
        Ok(volume)
    }
    fn set_muted(&self, value: bool) -> Result<()> {
        let context = GUID::from_u128(0);
        check(unsafe {
            (self.raw.table::<VolumeTable>().set_mute)(
                self.raw.raw.as_ptr(),
                i32::from(value),
                &context,
            )
        })
    }
    fn muted(&self) -> Result<bool> {
        let mut muted = 0;
        check(unsafe {
            (self.raw.table::<VolumeTable>().mute)(self.raw.raw.as_ptr(), &mut muted)
        })?;
        Ok(muted != 0)
    }
}
impl Drop for Volume {
    fn drop(&mut self) {
        if self.registered {
            unsafe {
                (self.raw.table::<VolumeTable>().unregister)(
                    self.raw.raw.as_ptr(),
                    self.callback.as_ref().unwrap().raw.as_ptr(),
                );
            }
        }
    }
}

struct WindowsEndpoint {
    volume: RefCell<Option<Volume>>,
    device: Option<Reference>,
    apartment: Option<Apartment>,
    api: Api,
}
impl WindowsEndpoint {
    fn open_with(api: Api, id: &str) -> Result<Self> {
        if id.trim().is_empty() || id.contains('\0') || id.encode_utf16().count() > 32767 {
            return Err(CallAudioError::InvalidRequest);
        }
        let apartment = Apartment::new(api)?;
        let device = {
            let devices = Reference::acquire(|out| unsafe {
                (api.create)(&CLASS, ptr::null_mut(), 23, &DEVICES, out)
            })?;
            let id = id.encode_utf16().chain([0]).collect::<Vec<_>>();
            Reference::acquire(|out| unsafe {
                (devices.table::<Enumerator>().get)(devices.raw.as_ptr(), id.as_ptr(), out)
            })?
        };
        Ok(Self {
            volume: RefCell::new(None),
            device: Some(device),
            apartment: Some(apartment),
            api,
        })
    }
    fn device(&self) -> Result<&Reference> {
        self.device.as_ref().ok_or(CallAudioError::RouteFailed)
    }
    fn volume<T>(&self, call: impl FnOnce(&Volume) -> Result<T>) -> Result<T> {
        let mut volume = self.volume.borrow_mut();
        if volume.is_none() {
            *volume = Some(Volume::new(self.device()?)?);
        }
        call(volume.as_ref().unwrap())
    }
}
impl AudioMuteEndpoint for WindowsEndpoint {
    fn id(&self) -> Result<String> {
        let device = self.device()?;
        Allocated::read(self.api, |out| unsafe {
            (device.table::<Device>().id)(device.raw.as_ptr(), out)
        })
    }
    fn active_render(&self) -> Result<bool> {
        let device = self.device()?;
        let flow = {
            let endpoint = device.query(&ENDPOINT)?;
            let mut value = 0;
            check(unsafe {
                (endpoint.table::<Endpoint>().flow)(endpoint.raw.as_ptr(), &mut value)
            })?;
            value
        };
        if flow != 0 {
            return Ok(false);
        }
        let mut state = 0;
        check(unsafe { (device.table::<Device>().state)(device.raw.as_ptr(), &mut state) })?;
        Ok(state == 1)
    }
    fn muted(&self) -> Result<bool> {
        self.volume(Volume::muted)
    }
    fn set_muted(&mut self, value: bool) -> Result<()> {
        self.volume(|v| v.set_muted(value))
    }
    fn close(&mut self) -> Result<()> {
        drop(self.volume.get_mut().take());
        drop(self.device.take());
        drop(self.apartment.take());
        Ok(())
    }
}
impl Drop for WindowsEndpoint {
    fn drop(&mut self) {
        let _ = self.close();
    }
}
pub(super) fn open(id: &str) -> Result<Box<dyn AudioMuteEndpoint>> {
    Ok(Box::new(WindowsEndpoint::open_with(Api::default(), id)?))
}
#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../tests/rust/phone_audio_mute_windows.rs"
    ));
}
