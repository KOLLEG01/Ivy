use std::{
    io,
    mem::size_of,
    os::windows::io::{AsHandle, AsRawHandle, BorrowedHandle, FromRawHandle, OwnedHandle},
};
use windows_sys::Win32::{
    Foundation::{
        ERROR_NO_MORE_FILES, FILETIME, HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0, WAIT_TIMEOUT,
    },
    System::Threading::{
        GetProcessTimes, OpenProcess, PROCESS_QUERY_INFORMATION, PROCESS_QUERY_LIMITED_INFORMATION,
        PROCESS_SYNCHRONIZE, PROCESS_TERMINATE, TerminateProcess, WaitForSingleObject,
    },
};

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

#[repr(C)]
struct BasicInformation {
    reserved: [usize; 4],
    pid: usize,
    parent: usize,
}
#[link(name = "ntdll")]
unsafe extern "system" {
    fn NtQueryInformationProcess(
        process: HANDLE,
        class: u32,
        information: *mut BasicInformation,
        size: u32,
        returned: *mut u32,
    ) -> i32;
}

fn parent_matches(handle: BorrowedHandle<'_>, pid: u32) -> bool {
    let mut info: BasicInformation = unsafe { std::mem::zeroed() };
    unsafe {
        NtQueryInformationProcess(
            handle.as_raw_handle(),
            0,
            &mut info,
            size_of::<BasicInformation>() as u32,
            std::ptr::null_mut(),
        ) == 0
            && info.parent == pid as usize
    }
}

fn running(handle: BorrowedHandle<'_>) -> io::Result<bool> {
    match unsafe { WaitForSingleObject(handle.as_raw_handle(), 0) } {
        WAIT_OBJECT_0 => Ok(false),
        WAIT_TIMEOUT => Ok(true),
        _ => Err(io::Error::last_os_error()),
    }
}

fn created(handle: BorrowedHandle<'_>) -> io::Result<u64> {
    let mut times: [FILETIME; 4] = unsafe { std::mem::zeroed() };
    let [birth, exit, kernel, user] = &mut times;
    if unsafe { GetProcessTimes(handle.as_raw_handle(), birth, exit, kernel, user) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((u64::from(birth.dwHighDateTime) << 32) | u64::from(birth.dwLowDateTime))
}

fn terminate(handle: BorrowedHandle<'_>, pid: u32) -> io::Result<()> {
    if !running(handle)? {
        return Ok(());
    }
    // The query handle pins the PID while obtaining termination rights. Never
    // terminate a process found only by a stale numeric PID.
    let raw = unsafe { OpenProcess(PROCESS_TERMINATE, 0, pid) };
    if raw.is_null() {
        return Err(io::Error::last_os_error());
    }
    let kill = unsafe { OwnedHandle::from_raw_handle(raw) };
    if unsafe { TerminateProcess(kill.as_raw_handle(), u32::MAX) } == 0 {
        let error = io::Error::last_os_error();
        if running(handle)? {
            return Err(error);
        }
    }
    Ok(())
}

fn children(pid: u32, parent_created: u64) -> io::Result<Vec<(OwnedHandle, u32, u64)>> {
    let raw = unsafe { CreateToolhelp32Snapshot(2, 0) };
    if raw == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    let snapshot = unsafe { OwnedHandle::from_raw_handle(raw) };
    let mut entry: Entry = unsafe { std::mem::zeroed() };
    entry.size = size_of::<Entry>() as u32;
    let mut found = unsafe { Process32FirstW(snapshot.as_raw_handle(), &mut entry) };
    let mut result = Vec::new();
    loop {
        if found == 0 {
            let error = io::Error::last_os_error();
            return if error.raw_os_error() == Some(ERROR_NO_MORE_FILES as i32) {
                Ok(result)
            } else {
                Err(error)
            };
        }
        if entry.parent == pid && entry.pid != pid {
            let raw = unsafe {
                OpenProcess(
                    PROCESS_QUERY_INFORMATION
                        | PROCESS_QUERY_LIMITED_INFORMATION
                        | PROCESS_SYNCHRONIZE,
                    0,
                    entry.pid,
                )
            };
            if !raw.is_null() {
                let child = unsafe { OwnedHandle::from_raw_handle(raw) };
                // Verify the current relationship through this retained handle:
                // the snapshot's child PID may have been recycled before open.
                // The parent handle pins its PID; creation order also excludes
                // a process left over from an earlier parent incarnation.
                if let Ok(birth) = created(child.as_handle()) {
                    if birth > parent_created
                        && parent_matches(child.as_handle(), pid)
                        && running(child.as_handle()).unwrap_or(false)
                    {
                        result.push((child, entry.pid, birth));
                    }
                }
            }
            // Discovery is best-effort. Disappeared and inaccessible entries
            // provide no kill handle, as in Process.Kill(true).
        }
        found = unsafe { Process32NextW(snapshot.as_raw_handle(), &mut entry) };
    }
}

pub(super) fn kill_if_running(root: BorrowedHandle<'_>, pid: u32) -> io::Result<bool> {
    if !running(root)? {
        return Ok(false);
    }
    let mut pending = vec![(root.try_clone_to_owned()?, pid, created(root)?)];
    let mut failure = None;
    while let Some((handle, pid, birth)) = pending.pop() {
        // Root first: prevent that process creating further children. Retain its
        // handle through child discovery, then do the same for each descendant.
        if let Err(error) = terminate(handle.as_handle(), pid) {
            failure.get_or_insert(error);
        }
        match children(pid, birth) {
            Ok(children) => pending.extend(children),
            Err(error) => {
                failure.get_or_insert(error);
            }
        }
    }
    match failure {
        Some(error) => Err(error),
        None => Ok(true),
    }
}
