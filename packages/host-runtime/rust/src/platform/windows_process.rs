//! Read-only Windows process identity for domain-owned launch/stop records.
use std::{io, os::windows::ffi::OsStringExt, path::PathBuf};
use windows_sys::Win32::{
    Foundation::{CloseHandle, ERROR_INVALID_PARAMETER, FILETIME, HANDLE},
    System::Threading::{
        GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
    },
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessIdentity {
    pub pid: u32,
    /// Exact Windows FILETIME ticks; callers choose their existing persisted encoding.
    pub created_filetime: u64,
    pub image: PathBuf,
}

struct Handle(HANDLE);
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}

/// Not-found means the PID is gone. Access denial and other failures stay errors;
/// they can never be used as stopped evidence.
pub fn process_identity(pid: u32) -> io::Result<Option<ProcessIdentity>> {
    let raw = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if raw.is_null() {
        let error = io::Error::last_os_error();
        return if error.raw_os_error() == Some(ERROR_INVALID_PARAMETER as i32) {
            Ok(None)
        } else {
            Err(error)
        };
    }
    let process = Handle(raw);
    let mut created: FILETIME = unsafe { std::mem::zeroed() };
    let mut exited = created;
    let mut kernel = created;
    let mut user = created;
    if unsafe { GetProcessTimes(process.0, &mut created, &mut exited, &mut kernel, &mut user) } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let mut name = vec![0u16; 32768];
    let mut size = name.len() as u32;
    if unsafe { QueryFullProcessImageNameW(process.0, 0, name.as_mut_ptr(), &mut size) } == 0 {
        return Err(io::Error::last_os_error());
    }
    name.truncate(size as usize);
    Ok(Some(ProcessIdentity {
        pid,
        created_filetime: (u64::from(created.dwHighDateTime) << 32)
            | u64::from(created.dwLowDateTime),
        image: std::ffi::OsString::from_wide(&name).into(),
    }))
}
