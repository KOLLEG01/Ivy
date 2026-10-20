//! Read-only Windows process identity for domain-owned launch/stop records.
use std::{io, os::windows::ffi::OsStringExt, path::PathBuf};
use windows_sys::Win32::{
    Foundation::{
        CloseHandle, ERROR_INVALID_PARAMETER, FILETIME, HANDLE, WAIT_FAILED, WAIT_OBJECT_0,
        WAIT_TIMEOUT,
    },
    System::Threading::{
        GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
        QueryFullProcessImageNameW, WaitForSingleObject,
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

fn creation_time(process: &Handle) -> io::Result<u64> {
    let mut created: FILETIME = unsafe { std::mem::zeroed() };
    let mut exited = created;
    let mut kernel = created;
    let mut user = created;
    if unsafe { GetProcessTimes(process.0, &mut created, &mut exited, &mut kernel, &mut user) } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok((u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime))
}

/// Check the exact predecessor identity without waiting or inspecting its image.
/// Absence or PID reuse retires the old identity; a matching identity needs a
/// signaled process handle. Access denial and inspection failures remain errors.
pub fn process_exited(pid: u32, created_filetime: u64) -> io::Result<bool> {
    if pid == 0 || created_filetime == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Process identity must have a positive PID and creation FILETIME.",
        ));
    }
    let raw = unsafe {
        OpenProcess(
            PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION,
            0,
            pid,
        )
    };
    if raw.is_null() {
        let error = io::Error::last_os_error();
        return if error.raw_os_error() == Some(ERROR_INVALID_PARAMETER as i32) {
            Ok(true)
        } else {
            Err(error)
        };
    }
    let process = Handle(raw);
    if creation_time(&process)? != created_filetime {
        return Ok(true);
    }
    match unsafe { WaitForSingleObject(process.0, 0) } {
        WAIT_OBJECT_0 => Ok(true),
        WAIT_TIMEOUT => Ok(false),
        WAIT_FAILED => Err(io::Error::last_os_error()),
        _ => Err(io::Error::other("Unexpected process wait result.")),
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
    let created_filetime = creation_time(&process)?;
    let mut name = vec![0u16; 32768];
    let mut size = name.len() as u32;
    if unsafe { QueryFullProcessImageNameW(process.0, 0, name.as_mut_ptr(), &mut size) } == 0 {
        return Err(io::Error::last_os_error());
    }
    name.truncate(size as usize);
    Ok(Some(ProcessIdentity {
        pid,
        created_filetime,
        image: std::ffi::OsString::from_wide(&name).into(),
    }))
}
