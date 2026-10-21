//! Packaged application activation only. Process observation and the full
//! Desktop owner remain separate; an activation PID never grants authority.
use super::{ActionResult, DesktopActionError, validate_application};
use std::{ffi::c_void, ptr};
use windows_sys::core::GUID;

const CLASS: GUID = GUID::from_u128(0x45ba127d_10a8_46ea_8ab7_56ea9078943c);
const INTERFACE: GUID = GUID::from_u128(0x2e941141_7f97_4756_ba1d_9decde894a3d);
const CHANGED_MODE: i32 = 0x80010106u32 as i32;
type Raw = *mut c_void;
#[repr(C)]
#[derive(Default)]
struct UserObjectFlags {
    inherit: i32,
    reserved: i32,
    flags: u32,
}
type StationFn = unsafe extern "system" fn() -> isize;
type InformationFn = unsafe extern "system" fn(isize, i32, Raw, u32, *mut u32) -> i32;
type InitializeFn = unsafe extern "system" fn(Raw, u32) -> i32;
type UninitializeFn = unsafe extern "system" fn();
type CreateFn = unsafe extern "system" fn(*const GUID, Raw, u32, *const GUID, *mut Raw) -> i32;
#[repr(C)]
struct ActivationManager {
    query: unsafe extern "system" fn(Raw, *const GUID, *mut Raw) -> i32,
    add_ref: unsafe extern "system" fn(Raw) -> u32,
    release: unsafe extern "system" fn(Raw) -> u32,
    activate: unsafe extern "system" fn(Raw, *const u16, *const u16, u32, *mut u32) -> i32,
    _file: usize,
    _protocol: usize,
}
#[link(name = "user32")]
unsafe extern "system" {
    fn GetProcessWindowStation() -> isize;
    fn GetUserObjectInformationW(
        station: isize,
        index: i32,
        value: Raw,
        size: u32,
        needed: *mut u32,
    ) -> i32;
}
#[link(name = "ole32")]
unsafe extern "system" {
    fn CoInitializeEx(reserved: Raw, mode: u32) -> i32;
    fn CoUninitialize();
    fn CoCreateInstance(
        class: *const GUID,
        outer: Raw,
        context: u32,
        iid: *const GUID,
        result: *mut Raw,
    ) -> i32;
}
#[derive(Clone, Copy)]
struct Native {
    station: StationFn,
    information: InformationFn,
    initialize: InitializeFn,
    uninitialize: UninitializeFn,
    create: CreateFn,
}
impl Default for Native {
    fn default() -> Self {
        Self {
            station: GetProcessWindowStation,
            information: GetUserObjectInformationW,
            initialize: CoInitializeEx,
            uninitialize: CoUninitialize,
            create: CoCreateInstance,
        }
    }
}
impl Native {
    fn interactive(self) -> bool {
        // Match Environment.UserInteractive: a successful query decides by
        // WSF_VISIBLE; an unavailable station/query retains its true fallback.
        let station = unsafe { (self.station)() };
        if station != 0 {
            let mut flags = UserObjectFlags::default();
            let mut needed = 0;
            if unsafe {
                (self.information)(
                    station,
                    1,
                    ptr::from_mut(&mut flags).cast(),
                    size_of::<UserObjectFlags>() as u32,
                    &mut needed,
                )
            } != 0
            {
                return flags.flags & 1 != 0;
            }
        }
        true
    }
}
struct Apartment {
    initialized: bool,
    uninitialize: UninitializeFn,
}
impl Drop for Apartment {
    fn drop(&mut self) {
        if self.initialized {
            unsafe { (self.uninitialize)() };
        }
    }
}
struct Manager(Raw);
impl Manager {
    unsafe fn table(&self) -> &ActivationManager {
        unsafe { &**self.0.cast::<*const ActivationManager>() }
    }
}
impl Drop for Manager {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { (self.table().release)(self.0) };
        }
    }
}

pub struct WindowsApplicationActivation {
    application: Vec<u16>,
    native: Native,
}
impl WindowsApplicationActivation {
    pub fn new(application: &str) -> ActionResult<Self> {
        Self::with_native(application, Native::default())
    }
    fn with_native(application: &str, native: Native) -> ActionResult<Self> {
        validate_application(application)?;
        if !native.interactive() {
            return Err(DesktopActionError::Platform(
                "Desktop activation requires interactive Windows x64.",
            ));
        }
        Ok(Self {
            application: application.encode_utf16().chain([0]).collect(),
            native,
        })
    }
    pub fn activate(&self) -> ActionResult<i32> {
        let initialized = unsafe { (self.native.initialize)(ptr::null_mut(), 0) };
        if initialized < 0 && initialized != CHANGED_MODE {
            return Err(DesktopActionError::Unavailable);
        }
        let _apartment = Apartment {
            initialized: initialized >= 0,
            uninitialize: self.native.uninitialize,
        };
        let mut raw = ptr::null_mut();
        let created =
            unsafe { (self.native.create)(&CLASS, ptr::null_mut(), 4, &INTERFACE, &mut raw) };
        // Even an unexpected failing non-null out reference has scoped ownership.
        // This guard drops before the apartment balance on every return path.
        let manager = Manager(raw);
        if created < 0 || manager.0.is_null() {
            return Err(DesktopActionError::Unavailable);
        }
        let mut pid = 0;
        let result = unsafe {
            (manager.table().activate)(
                manager.0,
                self.application.as_ptr(),
                ptr::null(),
                2,
                &mut pid,
            )
        };
        if result < 0 {
            return Err(DesktopActionError::Unavailable);
        }
        i32::try_from(pid).map_err(|_| DesktopActionError::Unavailable)
    }
}

#[cfg(test)]
#[path = "../../../../../tests/rust/phone_desktop_activation.rs"]
mod tests;
