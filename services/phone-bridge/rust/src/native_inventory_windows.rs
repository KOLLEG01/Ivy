//! Narrow MMDevice/IPropertyStore/endpoint-volume observation. No audio client,
//! sample stream, default/volume setter or routing API is called.
use super::{InventoryError, inventory};
use ivy_contracts::{IvyJson, json::JsString};
use std::{
    ffi::c_void,
    marker::PhantomData,
    ptr::{self, NonNull},
    rc::Rc,
    sync::atomic::{AtomicU32, Ordering, fence},
};
use windows_sys::core::GUID;
type Raw = *mut c_void;
type Hr = i32;
type Result<T> = std::result::Result<T, InventoryError>;
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
    enumerate: unsafe extern "system" fn(Raw, i32, u32, *mut Raw) -> Hr,
}
#[repr(C)]
struct Collection {
    unknown: Unknown,
    count: unsafe extern "system" fn(Raw, *mut i32) -> Hr,
    at: unsafe extern "system" fn(Raw, u32, *mut Raw) -> Hr,
}
#[repr(C)]
struct Device {
    unknown: Unknown,
    activate: unsafe extern "system" fn(Raw, *const GUID, u32, Raw, *mut Raw) -> Hr,
    properties: unsafe extern "system" fn(Raw, u32, *mut Raw) -> Hr,
    id: unsafe extern "system" fn(Raw, *mut *mut u16) -> Hr,
    state: unsafe extern "system" fn(Raw, *mut u32) -> Hr,
}
#[repr(C)]
struct Endpoint {
    unknown: Unknown,
    flow: unsafe extern "system" fn(Raw, *mut i32) -> Hr,
}
#[repr(C)]
struct Key {
    format: GUID,
    id: u32,
}
#[repr(C, align(8))]
struct Variant {
    vt: u16,
    reserved: [u16; 3],
    data: [usize; 2],
}
#[repr(C)]
struct Store {
    unknown: Unknown,
    reserved: [usize; 2],
    get: unsafe extern "system" fn(Raw, *const Key, *mut Variant) -> Hr,
}
#[repr(C)]
struct VolumeTable {
    unknown: Unknown,
    register: unsafe extern "system" fn(Raw, Raw) -> Hr,
    unregister: unsafe extern "system" fn(Raw, Raw) -> Hr,
    channels: unsafe extern "system" fn(Raw, *mut i32) -> Hr,
    unused: [usize; 8],
    set_mute: usize,
    mute: unsafe extern "system" fn(Raw, *mut i32) -> Hr,
    steps: unsafe extern "system" fn(Raw, *mut u32, *mut u32) -> Hr,
    step_changes: [usize; 2],
    hardware: unsafe extern "system" fn(Raw, *mut u32) -> Hr,
    range: unsafe extern "system" fn(Raw, *mut f32, *mut f32, *mut f32) -> Hr,
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
    fn CoCreateInstance(
        class: *const GUID,
        outer: Raw,
        context: u32,
        iid: *const GUID,
        out: *mut Raw,
    ) -> Hr;
    fn CoTaskMemFree(value: Raw);
    fn PropVariantClear(value: *mut Variant) -> Hr;
}
#[link(name = "ntdll")]
unsafe extern "system" {
    fn RtlGetVersion(value: *mut Version) -> Hr;
}
#[derive(Clone, Copy)]
struct Api {
    initialize: unsafe extern "system" fn(Raw, u32) -> Hr,
    uninitialize: unsafe extern "system" fn(),
    create: unsafe extern "system" fn(*const GUID, Raw, u32, *const GUID, *mut Raw) -> Hr,
    free: unsafe extern "system" fn(Raw),
    clear: unsafe extern "system" fn(*mut Variant) -> Hr,
    version: unsafe extern "system" fn(*mut Version) -> Hr,
}
impl Default for Api {
    fn default() -> Self {
        Self {
            initialize: CoInitializeEx,
            uninitialize: CoUninitialize,
            create: CoCreateInstance,
            free: CoTaskMemFree,
            clear: PropVariantClear,
            version: RtlGetVersion,
        }
    }
}
fn check(hr: Hr) -> Result<()> {
    if hr >= 0 { Ok(()) } else { Err(InventoryError) }
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
        owner.ok_or(InventoryError)
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
unsafe fn wide(raw: *const u16, maximum: usize) -> Result<JsString> {
    if raw.is_null() {
        return Err(InventoryError);
    }
    for n in 0..=maximum {
        if unsafe { *raw.add(n) } == 0 {
            return Ok(JsString(
                unsafe { std::slice::from_raw_parts(raw, n) }.to_vec(),
            ));
        }
    }
    Err(InventoryError)
}
struct Allocated {
    raw: *mut u16,
    api: Api,
}
impl Allocated {
    fn read(api: Api, call: impl FnOnce(*mut *mut u16) -> Hr) -> Result<JsString> {
        let mut s = Self {
            raw: ptr::null_mut(),
            api,
        };
        check(call(&mut s.raw))?;
        unsafe { wide(s.raw, 1024) }
    }
}
impl Drop for Allocated {
    fn drop(&mut self) {
        if !self.raw.is_null() {
            unsafe { (self.api.free)(self.raw.cast()) }
        }
    }
}
struct Property {
    value: Variant,
    api: Api,
}
impl Drop for Property {
    fn drop(&mut self) {
        unsafe {
            (self.api.clear)(&mut self.value);
        }
    }
}
fn friendly(store: &Reference, api: Api) -> Result<JsString> {
    let key = Key {
        format: GUID::from_u128(0xa45c254e_df1c_4efd_8020_67d146a850e0),
        id: 14,
    };
    let mut p = Property {
        value: Variant {
            vt: 0,
            reserved: [0; 3],
            data: [0; 2],
        },
        api,
    };
    let hr = unsafe { (store.table::<Store>().get)(store.raw.as_ptr(), &key, &mut p.value) };
    // NAudio TryGetValue returns false on failed GetValue or VT_EMPTY/null.
    if hr < 0 || p.value.vt == 0 || p.value.vt == 31 && p.value.data[0] == 0 {
        return Ok("Unknown".into());
    }
    if p.value.vt != 31 {
        return Err(InventoryError);
    }
    unsafe { wide(p.value.data[0] as *const u16, 256) }
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
            return Err(InventoryError);
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
fn state_name(state: u32) -> String {
    if state == 15 {
        return "All".into();
    }
    if state == 0 || state & !15 != 0 {
        return (state as i32).to_string();
    }
    [
        (1, "Active"),
        (2, "Disabled"),
        (4, "NotPresent"),
        (8, "Unplugged"),
    ]
    .into_iter()
    .filter_map(|(bit, name)| (state & bit != 0).then_some(name))
    .collect::<Vec<_>>()
    .join(", ")
}
pub(super) fn read() -> Result<IvyJson> {
    read_with(Api::default())
}
fn device_row(device: &Reference, api: Api) -> Result<IvyJson> {
    let id = Allocated::read(api, |out| unsafe {
        (device.table::<Device>().id)(device.raw.as_ptr(), out)
    })?;
    let store = Reference::acquire(|out| unsafe {
        (device.table::<Device>().properties)(device.raw.as_ptr(), 0, out)
    })?;
    let name = friendly(&store, api)?;
    let mut state = 0;
    check(unsafe { (device.table::<Device>().state)(device.raw.as_ptr(), &mut state) })?;
    let volume = if state == 1 {
        Volume::new(device).ok()
    } else {
        None
    };
    let muted = volume.as_ref().and_then(|v| v.muted().ok());
    let endpoint = device.query(&ENDPOINT)?;
    let mut flow = 0;
    check(unsafe { (endpoint.table::<Endpoint>().flow)(endpoint.raw.as_ptr(), &mut flow) })?;
    check(unsafe { (device.table::<Device>().state)(device.raw.as_ptr(), &mut state) })?;
    Ok(IvyJson::object([
        ("id", IvyJson::String(id)),
        ("name", IvyJson::String(name)),
        (
            "direction",
            if flow == 1 { "capture" } else { "render" }.into(),
        ),
        ("state", state_name(state).into()),
        ("muted", muted.map_or(IvyJson::Null, Into::into)),
    ]))
}
fn loopback_supported(api: Api) -> Result<bool> {
    let mut v = Version {
        size: std::mem::size_of::<Version>() as u32,
        major: 0,
        minor: 0,
        build: 0,
        platform: 0,
        service: [0; 128],
    };
    check(unsafe { (api.version)(&mut v) })?;
    Ok(v.major > 10 || v.major == 10 && (v.minor > 0 || v.build >= 20348))
}
fn read_with(api: Api) -> Result<IvyJson> {
    let _apartment = Apartment::new(api)?;
    let devices = Reference::acquire(|out| unsafe {
        (api.create)(&CLASS, ptr::null_mut(), 23, &DEVICES, out)
    })?;
    let collection = Reference::acquire(|out| unsafe {
        (devices.table::<Enumerator>().enumerate)(devices.raw.as_ptr(), 2, 15, out)
    })?;
    let mut rows = Vec::new();
    let mut index = 0;
    loop {
        let mut count = 0;
        check(unsafe {
            (collection.table::<Collection>().count)(collection.raw.as_ptr(), &mut count)
        })?;
        if index >= count {
            break;
        }
        let device = Reference::acquire(|out| unsafe {
            (collection.table::<Collection>().at)(collection.raw.as_ptr(), index as u32, out)
        })?;
        if rows.len() >= 32 {
            return Err(InventoryError);
        }
        rows.push(device_row(&device, api)?);
        index += 1;
    }
    Ok(inventory(rows, loopback_supported(api)?))
}
#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../tests/rust/phone_inventory_windows.rs"
    ));
}
