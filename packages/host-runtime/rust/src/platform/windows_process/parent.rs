use super::Handle;
use std::{io, mem::size_of};
use windows_sys::Win32::Foundation::{ERROR_NO_MORE_FILES, HANDLE, INVALID_HANDLE_VALUE};

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
    fn CreateToolhelp32Snapshot(flags: u32, pid: u32) -> HANDLE;
    fn Process32FirstW(snapshot: HANDLE, entry: *mut Entry) -> i32;
    fn Process32NextW(snapshot: HANDLE, entry: *mut Entry) -> i32;
}

/// Read one fresh process inventory, visiting at most `limit` entries. A normal
/// end without the PID returns None; native and bound failures remain errors.
/// The caller owns positive/Int32 PID policy and process-incarnation checks.
pub fn process_parent_id(pid: u32, limit: usize) -> io::Result<Option<u32>> {
    let raw = unsafe { CreateToolhelp32Snapshot(2, 0) };
    if raw == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // Keep the existing best-effort close semantics for this transient snapshot.
    let snapshot = Handle(raw);
    let mut entry: Entry = unsafe { std::mem::zeroed() };
    entry.size = size_of::<Entry>() as u32;
    let mut found = unsafe { Process32FirstW(snapshot.0, &mut entry) };
    let mut visited = 0;
    loop {
        if found == 0 {
            let error = io::Error::last_os_error();
            return if error.raw_os_error() == Some(ERROR_NO_MORE_FILES as i32) {
                Ok(None)
            } else {
                Err(error)
            };
        }
        if visited == limit {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Process inventory limit exceeded.",
            ));
        }
        visited += 1;
        if entry.pid == pid {
            return Ok(Some(entry.parent));
        }
        found = unsafe { Process32NextW(snapshot.0, &mut entry) };
    }
}
