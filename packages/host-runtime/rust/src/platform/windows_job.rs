//! Kernel-owned process trees. The Job is attached during CreateProcessW, before
//! the child exists, so launcher death cannot orphan an unassigned suspended child.

use super::windows_arguments::{quote_argument, valid_job_name};
use std::{
    ffi::OsString,
    io,
    mem::{size_of, size_of_val, zeroed},
    os::windows::ffi::OsStrExt,
    ptr::{null, null_mut},
};
use windows_sys::Win32::{
    Foundation::{
        CloseHandle, ERROR_ALREADY_EXISTS, ERROR_FILE_NOT_FOUND, GetLastError, HANDLE,
        HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE, SetHandleInformation,
    },
    Storage::FileSystem::{CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING},
    System::{
        Console::{GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE},
        JobObjects::{
            CreateJobObjectW, JOB_OBJECT_LIMIT_BREAKAWAY_OK, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
            OpenJobObjectW, SetInformationJobObject, TerminateJobObject,
        },
        Threading::{
            CREATE_NO_WINDOW, CREATE_SUSPENDED, CreateProcessW, DeleteProcThreadAttributeList,
            EXTENDED_STARTUPINFO_PRESENT, GetExitCodeProcess, InitializeProcThreadAttributeList,
            OpenProcess, PROC_THREAD_ATTRIBUTE_JOB_LIST, PROCESS_INFORMATION, PROCESS_SYNCHRONIZE,
            ResumeThread, STARTF_USESHOWWINDOW, STARTF_USESTDHANDLES, STARTUPINFOEXW,
            TerminateProcess, UpdateProcThreadAttribute, WaitForMultipleObjects,
        },
    },
};

fn argument_error(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}
fn checked(value: i32) -> io::Result<()> {
    if value == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}
fn wide(value: &std::ffi::OsStr) -> io::Result<Vec<u16>> {
    let mut value: Vec<_> = value.encode_wide().collect();
    if value.contains(&0) {
        return Err(argument_error("NUL in process argument."));
    }
    value.push(0);
    Ok(value)
}

/// This type is the sole owner of a kernel handle; no handle is cloned or inherited.
struct Handle(HANDLE);
impl Handle {
    fn new(value: HANDLE) -> io::Result<Self> {
        if value.is_null() || value == INVALID_HANDLE_VALUE {
            Err(io::Error::last_os_error())
        } else {
            Ok(Self(value))
        }
    }
}
impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: constructors admit only valid owned handles, each closed once.
        unsafe {
            CloseHandle(self.0);
        }
    }
}

/// Never signals or changes the observed job. Only its exact absence proves stopped.
pub fn job_stopped(name: &str) -> io::Result<bool> {
    if !valid_job_name(name) {
        return Err(argument_error("Invalid process job name."));
    }
    let name = wide(name.as_ref())?;
    // SAFETY: NUL-terminated name remains alive; returned handle is owned below.
    let raw = unsafe {
        OpenJobObjectW(0x0004 /* JOB_OBJECT_QUERY */, 0, name.as_ptr())
    };
    if raw.is_null() {
        let error = unsafe { GetLastError() };
        return if error == ERROR_FILE_NOT_FOUND {
            Ok(true)
        } else {
            Err(io::Error::from_raw_os_error(error as i32))
        };
    }
    let _job = Handle::new(raw)?;
    Ok(false)
}

struct Attributes {
    // usize allocation supplies the required native pointer alignment.
    storage: Vec<usize>,
    initialized: bool,
}
impl Attributes {
    fn new(job: &HANDLE) -> io::Result<Self> {
        let mut bytes = 0;
        unsafe {
            InitializeProcThreadAttributeList(null_mut(), 1, 0, &mut bytes);
        }
        if bytes == 0 || bytes > 65_536 {
            return Err(io::Error::last_os_error());
        }
        let mut value = Self {
            storage: vec![0; bytes.div_ceil(size_of::<usize>())],
            initialized: false,
        };
        // Do not run DeleteProcThreadAttributeList until initialization succeeds.
        checked(unsafe {
            InitializeProcThreadAttributeList(value.as_mut_ptr(), 1, 0, &mut bytes)
        })?;
        value.initialized = true;
        checked(unsafe {
            UpdateProcThreadAttribute(
                value.as_mut_ptr(),
                0,
                PROC_THREAD_ATTRIBUTE_JOB_LIST as usize,
                (job as *const HANDLE).cast_mut().cast(),
                size_of::<HANDLE>(),
                null_mut(),
                null_mut(),
            )
        })?;
        Ok(value)
    }
    fn as_mut_ptr(&mut self) -> *mut std::ffi::c_void {
        self.storage.as_mut_ptr().cast()
    }
}
impl Drop for Attributes {
    fn drop(&mut self) {
        if self.initialized {
            unsafe {
                DeleteProcThreadAttributeList(self.as_mut_ptr());
            }
        }
    }
}

pub fn run(arguments: &[OsString]) -> io::Result<u32> {
    if arguments.len() == 2 && arguments[0] == "--probe-job" {
        let name = arguments[1]
            .to_str()
            .ok_or_else(|| argument_error("Invalid process job name."))?;
        println!("{{\"stopped\":{}}}", job_stopped(name)?);
        return Ok(0);
    }
    if arguments.len() < 3 || arguments[0] != "--parent" {
        return Err(argument_error(
            "Expected --parent PID executable [args...].",
        ));
    }
    let parent_id = arguments[1]
        .to_str()
        .and_then(|text| text.parse::<u32>().ok())
        .ok_or_else(|| argument_error("Invalid parent process identity."))?;
    let parent = if parent_id == 0 {
        None
    } else {
        Some(Handle::new(unsafe {
            OpenProcess(PROCESS_SYNCHRONIZE, 0, parent_id)
        })?)
    };
    let mut executable_index = 2;
    let allow_breakaway = arguments
        .get(executable_index)
        .is_some_and(|v| v == "--allow-breakaway");
    if allow_breakaway {
        executable_index += 1;
    }
    let job_name = if arguments
        .get(executable_index)
        .is_some_and(|v| v == "--job-name")
    {
        let name = arguments
            .get(executable_index + 1)
            .and_then(|v| v.to_str())
            .filter(|name| valid_job_name(name))
            .ok_or_else(|| argument_error("Invalid process job name."))?;
        executable_index += 2;
        Some(wide(name.as_ref())?)
    } else {
        None
    };
    let executable = arguments
        .get(executable_index)
        .ok_or_else(|| argument_error("Missing executable."))?;
    let executable = wide(executable)?;
    let raw_job =
        unsafe { CreateJobObjectW(null(), job_name.as_ref().map_or(null(), |n| n.as_ptr())) };
    let last_error = unsafe { GetLastError() };
    let job = Handle::new(raw_job)?;
    if job_name.is_some() && last_error == ERROR_ALREADY_EXISTS {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "The process job is already owned.",
        ));
    }
    let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { zeroed() };
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
        | if allow_breakaway {
            JOB_OBJECT_LIMIT_BREAKAWAY_OK
        } else {
            0
        };
    checked(unsafe {
        SetInformationJobObject(
            job.0,
            JobObjectExtendedLimitInformation,
            (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
            size_of_val(&limits) as u32,
        )
    })?;

    let mut command = Vec::new();
    for argument in &arguments[executable_index..] {
        if !command.is_empty() {
            command.push(b' ' as u16);
        }
        let raw = wide(argument)?;
        command.extend(quote_argument(&raw[..raw.len() - 1]));
    }
    command.push(0);
    let mut startup: STARTUPINFOEXW = unsafe { zeroed() };
    startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
    startup.StartupInfo.dwFlags = STARTF_USESHOWWINDOW | STARTF_USESTDHANDLES;
    startup.StartupInfo.wShowWindow = 0;
    startup.StartupInfo.hStdInput = unsafe { GetStdHandle(STD_INPUT_HANDLE) };
    startup.StartupInfo.hStdOutput = unsafe { GetStdHandle(STD_OUTPUT_HANDLE) };
    startup.StartupInfo.hStdError = unsafe { GetStdHandle(STD_ERROR_HANDLE) };
    let invalid = |handle: HANDLE| handle.is_null() || handle == INVALID_HANDLE_VALUE;
    let mut nul = None;
    if [
        startup.StartupInfo.hStdInput,
        startup.StartupInfo.hStdOutput,
        startup.StartupInfo.hStdError,
    ]
    .iter()
    .any(|&h| invalid(h))
    {
        let name = wide("NUL".as_ref())?;
        let file = Handle::new(unsafe {
            CreateFileW(
                name.as_ptr(),
                0xC0000000,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                null(),
                OPEN_EXISTING,
                0,
                null_mut(),
            )
        })?;
        checked(unsafe { SetHandleInformation(file.0, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT) })?;
        for handle in [
            &mut startup.StartupInfo.hStdInput,
            &mut startup.StartupInfo.hStdOutput,
            &mut startup.StartupInfo.hStdError,
        ] {
            if invalid(*handle) {
                *handle = file.0;
            }
        }
        nul = Some(file);
    }
    let mut attributes = Attributes::new(&job.0)?;
    startup.lpAttributeList = attributes.as_mut_ptr();
    let mut child: PROCESS_INFORMATION = unsafe { zeroed() };
    checked(unsafe {
        CreateProcessW(
            executable.as_ptr(),
            command.as_mut_ptr(),
            null(),
            null(),
            1,
            CREATE_NO_WINDOW | CREATE_SUSPENDED | EXTENDED_STARTUPINFO_PRESENT,
            null(),
            null(),
            &startup.StartupInfo,
            &mut child,
        )
    })?;
    let process = Handle::new(child.hProcess)?;
    let thread = Handle::new(child.hThread)?;
    drop(attributes);
    // All subsequent failures terminate the suspended/running child, with job drop
    // also terminating descendants. Parent observation cannot race the job assignment.
    let outcome = (|| {
        if let Some(parent) = &parent {
            if unsafe { WaitForMultipleObjects(1, &parent.0, 0, 0) } != 258 {
                return Err(io::Error::other("The process owner has exited."));
            }
        }
        if unsafe { ResumeThread(thread.0) } == u32::MAX {
            return Err(io::Error::last_os_error());
        }
        let waits: Vec<HANDLE> = std::iter::once(process.0)
            .chain(parent.as_ref().map(|p| p.0))
            .collect();
        if unsafe { WaitForMultipleObjects(waits.len() as u32, waits.as_ptr(), 0, u32::MAX) } != 0 {
            unsafe {
                TerminateJobObject(job.0, 1);
            }
            return Ok(1);
        }
        let mut code = 0;
        checked(unsafe { GetExitCodeProcess(process.0, &mut code) })?;
        Ok(code)
    })();
    if outcome.is_err() {
        unsafe {
            TerminateProcess(process.0, 1);
        }
    }
    drop(nul);
    outcome
}

/// Shared by console and windowless legacy-compatible launchers. No argv or
/// environment values enter failure diagnostics.
pub fn main() {
    let arguments: Vec<_> = std::env::args_os().skip(1).collect();
    match run(&arguments) {
        Ok(code) => std::process::exit(code as i32),
        Err(_) => {
            eprintln!("{{\"code\":\"process_launch_failed\",\"type\":\"OsError\"}}");
            std::process::exit(1);
        }
    }
}
