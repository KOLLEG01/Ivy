//! Private suspended-launch handshake. Handle values travel from the helper, but
//! only the owner duplicates them: an unread acknowledgement cannot leak a Job
//! handle into the owner and defeat kill-on-close cleanup.
use std::{
    io::{self, Read, Write},
    mem::{size_of, zeroed},
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    ptr::null_mut,
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::windows::named_pipe::{NamedPipeServer, ServerOptions},
    process::Child,
    time::Instant,
};
use windows_sys::Win32::{
    Foundation::{
        DuplicateHandle, ERROR_INVALID_PARAMETER, HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0,
        WAIT_TIMEOUT,
    },
    System::{
        IO::{CreateIoCompletionPort, GetQueuedCompletionStatus},
        JobObjects::{
            IsProcessInJob, JOBOBJECT_ASSOCIATE_COMPLETION_PORT,
            JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
            JOB_OBJECT_LIMIT_BREAKAWAY_OK, JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK,
            JOB_OBJECT_LIMIT_JOB_MEMORY, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JobObjectExtendedLimitInformation, JobObjectAssociateCompletionPortInformation,
            JobObjectBasicAccountingInformation, QueryInformationJobObject,
            SetInformationJobObject, TerminateJobObject,
        },
        Threading::{
            GetCurrentProcess, OpenProcess, PROCESS_QUERY_INFORMATION, PROCESS_SYNCHRONIZE,
            WaitForSingleObject,
        },
    },
};

const PREFIX: &str = r"\\.\pipe\Ivy.Process.";
const MAGIC: &[u8; 8] = b"IVYJOB01";
const MEMORY_MAGIC: &[u8; 8] = b"IVYJOB02";
const JOB_QUERY_TERMINATE: u32 = 0x0002 | 0x0004 | 0x0008;

fn checked(value: i32) -> io::Result<()> {
    if value == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn duplicate(source_process: HANDLE, source: HANDLE, access: u32) -> io::Result<OwnedHandle> {
    let mut copied = null_mut();
    // SAFETY: the source process handle is held for this call; Windows validates
    // the source handle and access. The new non-inheritable handle has one owner.
    checked(unsafe {
        DuplicateHandle(
            source_process,
            source,
            GetCurrentProcess(),
            &mut copied,
            access,
            0,
            0,
        )
    })?;
    Ok(unsafe { OwnedHandle::from_raw_handle(copied) })
}

pub(super) fn valid_pipe_name(name: &str) -> bool {
    name.strip_prefix(PREFIX)
        .is_some_and(|suffix| suffix.len() == 36 && uuid::Uuid::parse_str(suffix).is_ok())
}

/// Keeps parent-death cleanup active even while the helper blocks on its private
/// acknowledgement pipe. The process exit closes every helper-owned Job handle.
pub(super) fn watch_parent(parent: HANDLE, job: HANDLE) -> io::Result<()> {
    let parent = duplicate(unsafe { GetCurrentProcess() }, parent, PROCESS_SYNCHRONIZE)?;
    let job = duplicate(
        unsafe { GetCurrentProcess() },
        job,
        0x0008, /* JOB_OBJECT_TERMINATE */
    )?;
    std::thread::Builder::new()
        .name("process-owner".into())
        .spawn(move || {
            if unsafe { WaitForSingleObject(parent.as_raw_handle(), u32::MAX) } != WAIT_TIMEOUT {
                // Observers may retain Job handles, so closing ours is insufficient.
                unsafe {
                    TerminateJobObject(job.as_raw_handle(), 1);
                }
                std::process::exit(1);
            }
        })?;
    Ok(())
}

pub(super) fn acknowledge(name: &str, child: HANDLE, job: HANDLE, memory_limit: Option<usize>) -> io::Result<()> {
    if !valid_pipe_name(name) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Invalid private launch pipe.",
        ));
    }
    let mut pipe = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(name)?;
    let mut frame = [0; 24];
    frame[..8].copy_from_slice(if memory_limit.is_some() { MEMORY_MAGIC } else { MAGIC });
    frame[8..16].copy_from_slice(&(child as usize as u64).to_le_bytes());
    frame[16..].copy_from_slice(&(job as usize as u64).to_le_bytes());
    pipe.write_all(&frame)?;
    if let Some(bytes) = memory_limit {
        pipe.write_all(&(bytes as u64).to_le_bytes())?;
    }
    let mut go = [0];
    pipe.read_exact(&mut go)?;
    if go != [1] {
        return Err(io::Error::other("Launch acknowledgement rejected."));
    }
    Ok(())
}

pub(crate) struct Listener {
    pub name: String,
    pipe: NamedPipeServer,
    memory_limit: Option<usize>,
}
impl Listener {
    pub fn new() -> io::Result<Self> {
        Self::with_memory_limit(None)
    }

    pub fn with_memory_limit(memory_limit: Option<usize>) -> io::Result<Self> {
        let name = format!("{PREFIX}{}", uuid::Uuid::new_v4());
        let pipe = ServerOptions::new()
            .first_pipe_instance(true)
            .reject_remote_clients(true)
            .create(&name)?;
        Ok(Self { name, pipe, memory_limit })
    }

    pub async fn accept(mut self, helper: &mut Child) -> io::Result<Proof> {
        tokio::select! {
            result = helper.wait() => {
                result?;
                return Err(io::Error::other("Job launcher exited before acknowledgement."));
            }
            result = self.pipe.connect() => result?,
        }
        let mut frame = [0; 24];
        self.pipe.read_exact(&mut frame).await?;
        let magic = if self.memory_limit.is_some() { MEMORY_MAGIC } else { MAGIC };
        if &frame[..8] != magic {
            return Err(io::Error::other("Invalid launch acknowledgement."));
        }
        if let Some(bytes) = self.memory_limit {
            let mut echoed = [0; 8];
            self.pipe.read_exact(&mut echoed).await?;
            if u64::from_le_bytes(echoed) != bytes as u64 {
                return Err(io::Error::other("Launch memory limit acknowledgement differs."));
            }
        }
        let proof = {
            let source = helper
                .raw_handle()
                .ok_or_else(|| io::Error::other("Job launcher exited."))?;
            let decode = |bytes: &[u8]| -> io::Result<HANDLE> {
                let value = u64::from_le_bytes(bytes.try_into().unwrap());
                let value = usize::try_from(value)
                    .map_err(|_| io::Error::other("Invalid launch handle."))?;
                if value == 0 || value == usize::MAX {
                    return Err(io::Error::other("Invalid launch handle."));
                }
                Ok(value as HANDLE)
            };
            // Child first: partial failure cannot leave a retained Job handle.
            let child = duplicate(
                source,
                decode(&frame[8..16])?,
                PROCESS_SYNCHRONIZE | PROCESS_QUERY_INFORMATION,
            )?;
            let events = ProcessEvents::new()?;
            let job = duplicate(source, decode(&frame[16..])?, JOB_QUERY_TERMINATE)?;
            let mut proof = Proof { child, job, events };
            let association = JOBOBJECT_ASSOCIATE_COMPLETION_PORT {
                CompletionKey: 1usize as _,
                CompletionPort: proof.events.port.as_raw_handle(),
            };
            checked(unsafe {
                SetInformationJobObject(
                    proof.job.as_raw_handle(),
                    JobObjectAssociateCompletionPortInformation,
                    (&association as *const JOBOBJECT_ASSOCIATE_COMPLETION_PORT).cast(),
                    size_of::<JOBOBJECT_ASSOCIATE_COMPLETION_PORT>() as u32,
                )
            })?;
            let mut member = 0;
            checked(unsafe {
                IsProcessInJob(
                    proof.child.as_raw_handle(),
                    proof.job.as_raw_handle(),
                    &mut member,
                )
            })?;
            if member == 0 {
                return Err(io::Error::other("Launch child is outside its Job."));
            }
            if let Some(bytes) = self.memory_limit {
                let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { zeroed() };
                checked(unsafe { QueryInformationJobObject(
                    proof.job.as_raw_handle(), JobObjectExtendedLimitInformation,
                    (&mut limits as *mut JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                    size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32, null_mut(),
                ) })?;
                let flags = limits.BasicLimitInformation.LimitFlags;
                let required = JOB_OBJECT_LIMIT_JOB_MEMORY | JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                if flags & required != required || limits.JobMemoryLimit != bytes
                    || flags & (JOB_OBJECT_LIMIT_BREAKAWAY_OK | JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK) != 0 {
                    return Err(io::Error::other("Launch Job memory enforcement differs."));
                }
                for process in [source, unsafe { GetCurrentProcess() }] {
                    checked(unsafe { IsProcessInJob(process, proof.job.as_raw_handle(), &mut member) })?;
                    if member != 0 {
                        return Err(io::Error::other("Launch supervisor belongs to the limited Job."));
                    }
                }
                // Reconcile already queued births while the child is suspended.
                if !proof.events.drain()? {
                    return Err(io::Error::other("Launch Job notification queue is unconfirmed."));
                }
            }
            proof
        };
        // The child remains suspended until both exact kernel identities belong
        // to the parent. Cancellation from here drops Proof and terminates its Job.
        self.pipe.write_all(&[1]).await?;
        Ok(proof)
    }
}

pub(crate) struct Proof {
    child: OwnedHandle,
    job: OwnedHandle,
    events: ProcessEvents,
}

/// Associate before releasing the suspended child, so every descendant birth can
/// be reconciled with the Job's lifetime process count. Notifications alone are
/// not guaranteed: a missing event must leave retirement unconfirmed.
struct ProcessEvents {
    port: OwnedHandle,
    observed: u64,
    pending: Vec<OwnedHandle>,
    memory_limit_seen: bool,
}
impl ProcessEvents {
    fn new() -> io::Result<Self> {
        let raw = unsafe { CreateIoCompletionPort(INVALID_HANDLE_VALUE, null_mut(), 0, 1) };
        if raw.is_null() {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            port: unsafe { OwnedHandle::from_raw_handle(raw) },
            observed: 0,
            pending: Vec::new(),
            memory_limit_seen: false,
        })
    }

    fn drain(&mut self) -> io::Result<bool> {
        // Bound each synchronous poll; the fixed retirement deadline also bounds
        // an unusually large notification queue without discarding its evidence.
        for _ in 0..512 {
            let mut message = 0;
            let mut key = 0;
            let mut value = null_mut();
            let ok = unsafe {
                GetQueuedCompletionStatus(
                    self.port.as_raw_handle(),
                    &mut message,
                    &mut key,
                    &mut value,
                    0,
                )
            };
            if ok == 0 {
                let error = io::Error::last_os_error();
                if error.raw_os_error() == Some(WAIT_TIMEOUT as i32) {
                    return Ok(true);
                }
                return Err(error);
            }
            if key != 1 {
                return Err(io::Error::other("Unexpected Job notification identity."));
            }
            if message == 10 /* JOB_OBJECT_MSG_JOB_MEMORY_LIMIT */ {
                self.memory_limit_seen = true;
            }
            if message != 6
            /* JOB_OBJECT_MSG_NEW_PROCESS */
            {
                continue;
            }
            let pid = u32::try_from(value as usize)
                .ok()
                .filter(|&pid| pid != 0)
                .ok_or_else(|| io::Error::other("Invalid Job process identity."))?;
            let raw = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
            if raw.is_null() {
                let error = io::Error::last_os_error();
                // A nonexistent PID has no remaining process object to wait on.
                // Access denial is never equivalent to this evidence.
                if error.raw_os_error() != Some(ERROR_INVALID_PARAMETER as i32) {
                    return Err(error);
                }
            } else {
                // PID reuse can only make this wait conservative. Never terminate
                // a process reopened from a notification; terminate the held Job.
                self.pending
                    .push(unsafe { OwnedHandle::from_raw_handle(raw) });
            }
            self.observed = self
                .observed
                .checked_add(1)
                .ok_or_else(|| io::Error::other("Job notification count overflow."))?;
        }
        Ok(false)
    }

    fn signaled(&mut self) -> io::Result<bool> {
        let mut error = None;
        self.pending.retain(|process| {
            match unsafe { WaitForSingleObject(process.as_raw_handle(), 0) } {
                WAIT_OBJECT_0 => false,
                WAIT_TIMEOUT => true,
                _ => {
                    error = Some(io::Error::last_os_error());
                    true
                }
            }
        });
        if let Some(error) = error {
            return Err(error);
        }
        Ok(self.pending.is_empty())
    }
}
impl Proof {
    pub fn poll_memory_limit(&mut self) -> io::Result<bool> {
        self.events.drain()?;
        Ok(self.events.memory_limit_seen)
    }

    pub fn memory_limit_seen(&self) -> bool {
        self.events.memory_limit_seen
    }

    pub fn terminate(&self) -> io::Result<()> {
        checked(unsafe { TerminateJobObject(self.job.as_raw_handle(), 1) })
    }

    fn retired(&mut self) -> io::Result<bool> {
        let drained = self.events.drain()?;
        let descendants = self.events.signaled()?;
        let child = unsafe { WaitForSingleObject(self.child.as_raw_handle(), 0) };
        if child != WAIT_OBJECT_0 && child != WAIT_TIMEOUT {
            return Err(io::Error::last_os_error());
        }
        let mut accounting: JOBOBJECT_BASIC_ACCOUNTING_INFORMATION = unsafe { zeroed() };
        checked(unsafe {
            QueryInformationJobObject(
                self.job.as_raw_handle(),
                JobObjectBasicAccountingInformation,
                (&mut accounting as *mut JOBOBJECT_BASIC_ACCOUNTING_INFORMATION).cast(),
                size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as u32,
                null_mut(),
            )
        })?;
        Ok(drained
            && descendants
            && child == WAIT_OBJECT_0
            && accounting.ActiveProcesses == 0
            && self.events.observed == u64::from(accounting.TotalProcesses))
    }

    pub async fn retire(&mut self, deadline: Instant) -> io::Result<()> {
        // Retaining this Job handle disables last-helper-handle cleanup. Always
        // terminate explicitly, including normal helper exit and launcher crash.
        self.terminate()?;
        loop {
            if self.retired()? {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "Job retirement is unconfirmed.",
                ));
            }
            tokio::time::sleep_until(deadline.min(Instant::now() + Duration::from_millis(10)))
                .await;
        }
    }
}
impl Drop for Proof {
    fn drop(&mut self) {
        // Also covers cancellation/panic before supervision is installed. Close
        // alone is insufficient if a third party retains another Job handle.
        let _ = self.terminate();
    }
}

#[cfg(test)]
#[path = "../../../../../tests/rust/host-windows-launch.rs"]
mod tests;
