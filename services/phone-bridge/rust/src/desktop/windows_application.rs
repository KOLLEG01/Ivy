//! Packaged Desktop application observation. A matching process or window is an
//! observation only; activation and live Voice/audio ownership remain separate.
use super::{
    ActionResult, DesktopApplication, DesktopObservation, DesktopOwner,
    WindowsApplicationActivation, WindowsDesktopProcessOwner, windows_owner,
};
use crate::media::AudioDesktopIdentity;
use ivy_host_runtime::platform::windows_process;
use std::{ffi::c_void, io, ptr};
use windows_sys::Win32::{Foundation::CloseHandle, System::Threading::WaitForSingleObject};

type Raw = isize;
#[repr(C)]
struct Entry {
    size: u32,
    usage: u32,
    pid: u32,
    heap: usize,
    module: u32,
    threads: u32,
    parent: u32,
    priority: i32,
    flags: u32,
    executable: [u16; 260],
}
#[link(name = "kernel32")]
unsafe extern "system" {
    fn CreateToolhelp32Snapshot(flags: u32, pid: u32) -> Raw;
    fn Process32FirstW(snapshot: Raw, entry: *mut Entry) -> i32;
    fn Process32NextW(snapshot: Raw, entry: *mut Entry) -> i32;
    fn OpenProcess(access: u32, inherit: i32, pid: u32) -> Raw;
    fn GetExitCodeProcess(process: Raw, code: *mut u32) -> i32;
    fn GetApplicationUserModelId(process: Raw, length: *mut u32, value: *mut u16) -> i32;
    fn CompareStringOrdinal(
        a: *const u16,
        an: i32,
        b: *const u16,
        bn: i32,
        ignore_case: i32,
    ) -> i32;
}
#[link(name = "advapi32")]
unsafe extern "system" {
    fn OpenProcessToken(process: Raw, access: u32, token: *mut Raw) -> i32;
    fn GetTokenInformation(
        token: Raw,
        class: u32,
        buffer: *mut c_void,
        bytes: u32,
        required: *mut u32,
    ) -> i32;
    fn EqualSid(a: *const c_void, b: *const c_void) -> i32;
}
struct Handle(Raw);
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0 as _) };
    }
}
fn unavailable() -> io::Error {
    io::Error::other("Desktop application observation unavailable.")
}
// The SID pointer returned by TokenUser borrows this aligned allocation. Moving
// its Vec retains the allocation and does not copy or reinterpret UTF16/account names.
struct User(Vec<u64>);
impl User {
    fn read(token: Raw) -> io::Result<Self> {
        let mut length = 0;
        unsafe { GetTokenInformation(token, 1, ptr::null_mut(), 0, &mut length) };
        if io::Error::last_os_error().raw_os_error() != Some(122) || length < 16 {
            return Err(unavailable());
        }
        let mut words = vec![0u64; (length as usize).div_ceil(8)];
        if unsafe { GetTokenInformation(token, 1, words.as_mut_ptr().cast(), length, &mut length) }
            == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(words))
    }
    fn sid(&self) -> *const c_void {
        unsafe { *self.0.as_ptr().cast::<*const c_void>() }
    }
    fn same(&self, other: &Self) -> bool {
        !self.sid().is_null()
            && !other.sid().is_null()
            && unsafe { EqualSid(self.sid(), other.sid()) } != 0
    }
}
// Keep the original query handle/token alive only through this iteration; token
// drops first, including foreign-user, unpackaged, mismatch and error branches.
struct Admission {
    token: Handle,
    process: Handle,
}
trait Source {
    type User;
    type Admission;
    fn candidates(&mut self) -> io::Result<Vec<u32>>;
    fn caller(&mut self) -> io::Result<(u32, Self::User)>;
    fn exited(&mut self, pid: u32) -> io::Result<bool>;
    fn session(&mut self, pid: u32) -> io::Result<u32>;
    fn open(&mut self, pid: u32) -> io::Result<Self::Admission>;
    fn user_matches(&mut self, admitted: &Self::Admission, user: &Self::User) -> io::Result<bool>;
    fn application(&mut self, admitted: &Self::Admission) -> io::Result<Option<Vec<u16>>>;
    fn window(&mut self, pid: u32) -> Raw;
    fn identity(&mut self, pid: u32, application: &str) -> io::Result<AudioDesktopIdentity>;
    fn current(&mut self, identity: &AudioDesktopIdentity) -> bool;
}
fn observe_with(application: &str, source: &mut impl Source) -> DesktopObservation {
    let mut observe = || -> io::Result<DesktopObservation> {
        let processes = source.candidates()?;
        let (session, user) = source.caller()?;
        let wanted: Vec<_> = application.encode_utf16().collect();
        let mut matching = false;
        let mut windows = Vec::new();
        for pid in processes {
            if source.exited(pid)? || source.session(pid)? != session {
                continue;
            }
            let admitted = source.open(pid)?;
            if !source.user_matches(&admitted, &user)? {
                continue;
            }
            let Some(id) = source.application(&admitted)? else {
                continue;
            };
            if id != wanted {
                continue;
            }
            matching = true;
            if source.window(pid) == 0 {
                continue;
            }
            let identity = source.identity(pid, application)?;
            if !source.current(&identity) {
                return Err(unavailable());
            }
            windows.push(identity);
        }
        Ok(DesktopObservation::select(matching, windows))
    };
    observe().unwrap_or_else(|_| DesktopObservation::unavailable())
}
struct Native;
fn matches_name(name: &[u16]) -> bool {
    let name = &name[name
        .iter()
        .rposition(|c| *c == b'\\' as u16)
        .map_or(0, |n| n + 1)..];
    let extension = [46, 101, 120, 101];
    let name = if name.len() >= 4
        && unsafe {
            CompareStringOrdinal(name[name.len() - 4..].as_ptr(), 4, extension.as_ptr(), 4, 1)
        } == 2
    {
        &name[..name.len() - 4]
    } else {
        name
    };
    let wanted = [67, 104, 97, 116, 71, 80, 84];
    unsafe {
        CompareStringOrdinal(
            name.as_ptr(),
            name.len() as i32,
            wanted.as_ptr(),
            wanted.len() as i32,
            1,
        ) == 2
    }
}
impl Source for Native {
    type User = User;
    type Admission = Admission;
    fn candidates(&mut self) -> io::Result<Vec<u32>> {
        let raw = unsafe { CreateToolhelp32Snapshot(2, 0) };
        if raw == -1 {
            return Err(io::Error::last_os_error());
        }
        let snapshot = Handle(raw);
        let mut entry: Entry = unsafe { std::mem::zeroed() };
        entry.size = size_of::<Entry>() as u32;
        let mut found = unsafe { Process32FirstW(snapshot.0, &mut entry) };
        let mut rows = Vec::new();
        while found != 0 {
            let n = entry
                .executable
                .iter()
                .position(|c| *c == 0)
                .unwrap_or(entry.executable.len());
            if matches_name(&entry.executable[..n]) {
                rows.push(entry.pid);
            }
            found = unsafe { Process32NextW(snapshot.0, &mut entry) };
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(18) {
            return Err(error);
        }
        Ok(rows)
    }
    fn caller(&mut self) -> io::Result<(u32, User)> {
        let session = windows_owner::session(std::process::id())?;
        // GetCurrentThreadEffectiveToken is the Windows8 SDK pseudo-handle -6:
        // use the thread token when present, otherwise the process token. Borrow
        // it for TokenUser only; never duplicate, close or change impersonation.
        Ok((session, User::read(-6)?))
    }
    fn exited(&mut self, pid: u32) -> io::Result<bool> {
        let raw = unsafe { OpenProcess(0x00101000, 0, pid) };
        if raw == 0 {
            let error = io::Error::last_os_error();
            return if error.raw_os_error() == Some(87) {
                Ok(true)
            } else {
                Err(error)
            };
        }
        let process = Handle(raw);
        let mut code = 0;
        if unsafe { GetExitCodeProcess(process.0, &mut code) } != 0 && code != 259 {
            return Ok(true);
        }
        match unsafe { WaitForSingleObject(process.0 as _, 0) } {
            258 => Ok(false),
            0 => {
                if unsafe { GetExitCodeProcess(process.0, &mut code) } != 0 {
                    Ok(true)
                } else {
                    Err(io::Error::last_os_error())
                }
            }
            _ => Err(io::Error::last_os_error()),
        }
    }
    fn session(&mut self, pid: u32) -> io::Result<u32> {
        windows_owner::session(pid)
    }
    fn open(&mut self, pid: u32) -> io::Result<Admission> {
        let raw = unsafe { OpenProcess(0x1000, 0, pid) };
        if raw == 0 {
            return Err(io::Error::last_os_error());
        }
        let process = Handle(raw);
        let mut token = 0;
        if unsafe { OpenProcessToken(process.0, 8, &mut token) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Admission {
            token: Handle(token),
            process,
        })
    }
    fn user_matches(&mut self, admitted: &Admission, user: &User) -> io::Result<bool> {
        Ok(User::read(admitted.token.0)?.same(user))
    }
    fn application(&mut self, admitted: &Admission) -> io::Result<Option<Vec<u16>>> {
        let mut length = 0;
        let status =
            unsafe { GetApplicationUserModelId(admitted.process.0, &mut length, ptr::null_mut()) };
        if status == 15703 {
            return Ok(None);
        }
        if status != 122 || !(2..=129).contains(&length) {
            return Err(unavailable());
        }
        let mut value = vec![0; length as usize];
        if unsafe { GetApplicationUserModelId(admitted.process.0, &mut length, value.as_mut_ptr()) }
            != 0
        {
            return Err(unavailable());
        }
        value.truncate(value.iter().position(|c| *c == 0).unwrap_or(value.len()));
        Ok(Some(value))
    }
    fn window(&mut self, pid: u32) -> Raw {
        windows_owner::main_window(pid)
    }
    fn identity(&mut self, pid: u32, application: &str) -> io::Result<AudioDesktopIdentity> {
        let identity = windows_process::process_identity(pid)?.ok_or_else(unavailable)?;
        let ticks = identity
            .created_filetime
            .checked_add(504_911_232_000_000_000)
            .and_then(|ticks| i64::try_from(ticks).ok())
            .ok_or_else(unavailable)?;
        AudioDesktopIdentity::new(pid as i32, ticks, &identity.image, application)
            .map_err(|_| unavailable())
    }
    fn current(&mut self, identity: &AudioDesktopIdentity) -> bool {
        WindowsDesktopProcessOwner::new(identity.pid(), identity.creation_ticks(), identity.image())
            .and_then(|mut owner| owner.is_current())
            .unwrap_or(false)
    }
}

/// Read-only observation plus the existing normal packaged-application activation
/// primitive. Application admission preserves the original session/user/AUMID
/// ordering and never treats a lost observation as absence.
pub struct WindowsDesktopApplication {
    application: String,
    activation: WindowsApplicationActivation,
}
impl WindowsDesktopApplication {
    pub fn new(application: &str) -> ActionResult<Self> {
        let activation = WindowsApplicationActivation::new(application)?;
        Ok(Self {
            application: application.into(),
            activation,
        })
    }
}
impl DesktopApplication for WindowsDesktopApplication {
    fn observe(&mut self) -> ActionResult<Option<DesktopObservation>> {
        Ok(Some(observe_with(&self.application, &mut Native)))
    }
    fn activate(&mut self) -> ActionResult<i32> {
        self.activation.activate()
    }
}

#[cfg(test)]
#[path = "../../../../../tests/rust/phone_desktop_application.rs"]
mod tests;
