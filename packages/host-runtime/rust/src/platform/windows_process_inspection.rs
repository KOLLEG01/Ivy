//! Caller-ordered Windows process inspection; application and authority policy
//! remain with the caller. Metadata is deliberately read at separate boundaries.
use std::{
    io,
    mem::size_of,
    os::windows::io::{AsHandle, AsRawHandle, BorrowedHandle, FromRawHandle, OwnedHandle},
    path::PathBuf,
    ptr::null_mut,
    sync::Once,
};
mod ffi;
mod modules;
mod snapshot;
mod token;
use ffi::*;
pub use token::{UserIdentity, UserSid};

const QUERY: u32 = 0x1000;
const SYNCHRONIZE: u32 = 0x100000;

fn owned(raw: Handle) -> io::Result<OwnedHandle> {
    if raw.is_null() {
        Err(io::Error::last_os_error())
    } else {
        Ok(unsafe { OwnedHandle::from_raw_handle(raw) })
    }
}

/// Open only the requested query handle; no identity, module or token prefetch.
pub fn open_process_query_limited(pid: u32) -> io::Result<OwnedHandle> {
    owned(unsafe { OpenProcess(QUERY, 0, pid) })
}

/// The caller retains the process handle and controls the token's lifetime.
pub fn open_process_token_query(process: BorrowedHandle<'_>) -> io::Result<OwnedHandle> {
    let mut raw = null_mut();
    if unsafe { OpenProcessToken(process.as_raw_handle(), 8, &mut raw) } == 0 {
        return Err(io::Error::last_os_error());
    }
    owned(raw)
}

// System.Diagnostics.Process initializes its process manager once, including a
// best-effort debug-privilege attempt. Keep that boundary separate from the raw
// query/token helpers, which the original callers invoke directly.
fn manager_init() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        let name: Vec<u16> = "SeDebugPrivilege\0".encode_utf16().collect();
        let mut luid = Luid::default();
        if unsafe { LookupPrivilegeValueW(std::ptr::null(), name.as_ptr(), &mut luid) } == 0 {
            return;
        }
        let mut raw = null_mut();
        if unsafe { OpenProcessToken(GetCurrentProcess(), 0x20, &mut raw) } == 0 {
            return;
        }
        if let Ok(token) = owned(raw) {
            let state = TokenPrivileges {
                count: 1,
                luid,
                attributes: 2,
            };
            unsafe {
                AdjustTokenPrivileges(token.as_raw_handle(), 0, &state, 0, null_mut(), null_mut());
            }
        }
    });
}

fn exited(handle: BorrowedHandle<'_>, signaled: &mut bool) -> io::Result<bool> {
    let mut code = 0;
    if unsafe { GetExitCodeProcess(handle.as_raw_handle(), &mut code) } != 0 && code != 259 {
        return Ok(true);
    }
    if !*signaled {
        let wait = handle.try_clone_to_owned()?;
        *signaled = match unsafe { WaitForSingleObject(wait.as_raw_handle(), 0) } {
            0 => true,
            258 => false,
            u32::MAX => return Err(io::Error::last_os_error()),
            _ => return Err(io::Error::other("Unexpected process wait result.")),
        };
    }
    if *signaled && unsafe { GetExitCodeProcess(handle.as_raw_handle(), &mut code) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(*signaled)
}

fn running(pid: u32) -> io::Result<bool> {
    if pid != 0 {
        match owned(unsafe { OpenProcess(QUERY | SYNCHRONIZE, 0, pid) }) {
            Ok(handle) => return exited(handle.as_handle(), &mut false).map(|value| !value),
            Err(error) if error.raw_os_error() == Some(87) => return Ok(false),
            Err(_) => {} // The original falls back to PID enumeration on denial.
        }
    }
    let mut pids = vec![0u32; 256];
    loop {
        let mut needed = 0;
        if unsafe { K32EnumProcesses(pids.as_mut_ptr(), (pids.len() * 4) as u32, &mut needed) } == 0
        {
            return Err(io::Error::last_os_error());
        }
        if needed as usize != pids.len() * 4 {
            return Ok(pids[..needed as usize / 4].contains(&pid));
        }
        pids.resize(pids.len() * 2, 0);
    }
}

fn process_handle(pid: u32, access: u32, throw_if_exited: bool) -> io::Result<Option<OwnedHandle>> {
    match owned(unsafe { OpenProcess(access, 0, pid) }) {
        Ok(handle) => Ok(Some(handle)),
        Err(_) if pid == 0 => Err(io::Error::from_raw_os_error(5)),
        Err(error) => {
            if error.raw_os_error() != Some(5) && !running(pid)? {
                if throw_if_exited {
                    Err(io::Error::new(
                        io::ErrorKind::NotFound,
                        "Process has exited.",
                    ))
                } else {
                    Ok(None)
                }
            } else {
                Err(error)
            }
        }
    }
}

/// One original process-object lifetime. A new object starts with no positive
/// exit, creation-time or window cache, even when its PID matches another object.
pub struct ProcessObservation {
    pid: u32,
    session: Option<u32>,
    exited: bool,
    signaled: bool,
    created: Option<u64>,
    window: isize,
}
impl ProcessObservation {
    fn new(pid: u32, session: Option<u32>) -> Self {
        Self {
            pid,
            session,
            exited: false,
            signaled: false,
            created: None,
            window: 0,
        }
    }
    pub fn by_name(name: &str) -> io::Result<Vec<Self>> {
        manager_init();
        snapshot::processes(Some(name), None).map(|rows| {
            rows.into_iter()
                .map(|(pid, session)| Self::new(pid, Some(session)))
                .collect()
        })
    }
    pub fn by_id(pid: u32) -> io::Result<Option<Self>> {
        manager_init();
        Ok(running(pid)?.then(|| Self::new(pid, None)))
    }
    pub fn current() -> Self {
        Self::new(std::process::id(), None)
    }
    pub fn pid(&self) -> u32 {
        self.pid
    }
    pub fn has_exited(&mut self) -> io::Result<bool> {
        if !self.exited {
            manager_init();
            self.exited = match process_handle(self.pid, QUERY | SYNCHRONIZE, false)? {
                None => true,
                Some(handle) => exited(handle.as_handle(), &mut self.signaled)?,
            };
        }
        Ok(self.exited)
    }
    pub fn session_id(&mut self) -> io::Result<u32> {
        if let Some(session) = self.session {
            return Ok(session);
        }
        manager_init();
        let rows = snapshot::processes(None, Some(self.pid))?;
        let session = rows
            .first()
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    "Process information is unavailable.",
                )
            })?
            .1;
        self.session = Some(session);
        Ok(session)
    }
    pub fn created_filetime(&mut self) -> io::Result<u64> {
        if let Some(value) = self.created {
            return Ok(value);
        }
        manager_init();
        let process = process_handle(self.pid, QUERY, false)?
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "Process has exited."))?;
        let mut created = FileTime::default();
        let mut exit = FileTime::default();
        let mut kernel = FileTime::default();
        let mut user = FileTime::default();
        if unsafe {
            GetProcessTimes(
                process.as_raw_handle(),
                &mut created,
                &mut exit,
                &mut kernel,
                &mut user,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        let value = (u64::from(created.high) << 32) | u64::from(created.low);
        self.created = Some(value);
        Ok(value)
    }
    pub fn main_window_handle(&mut self) -> io::Result<isize> {
        if self.window == 0 {
            manager_init();
            struct Search {
                pid: u32,
                window: isize,
            }
            unsafe extern "system" fn visit(window: Handle, context: isize) -> i32 {
                let search = unsafe { &mut *(context as *mut Search) };
                let mut pid = 0;
                unsafe {
                    GetWindowThreadProcessId(window, &mut pid);
                }
                if pid == search.pid
                    && unsafe { GetWindow(window, 4).is_null() && IsWindowVisible(window) != 0 }
                {
                    search.window = window as isize;
                    0
                } else {
                    1
                }
            }
            let mut search = Search {
                pid: self.pid,
                window: 0,
            };
            // FALSE also ends a successful early selection; the original finder
            // returns the selected value and does not interpret last-error.
            unsafe {
                EnumWindows(visit, (&mut search as *mut Search) as isize);
            }
            self.window = search.window;
        }
        Ok(self.window)
    }
    pub fn main_module_file_name(&self) -> io::Result<Option<PathBuf>> {
        manager_init();
        modules::main_module(self.pid)
    }
}

/// Two fresh native queries; the caller decides how to map a failed inspection.
pub fn same_current_session(pid: u32) -> io::Result<bool> {
    if pid == 0 {
        return Ok(false);
    }
    let mut target = 0;
    let mut caller = 0;
    if unsafe { ProcessIdToSessionId(pid, &mut target) } == 0 {
        return Err(io::Error::last_os_error());
    }
    if unsafe { ProcessIdToSessionId(std::process::id(), &mut caller) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(target == caller)
}
