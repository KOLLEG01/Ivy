//! Opt-in root-process lifetime, separate from the managed process-tree owner.
use super::{Command, io_error, resolve_command};
use ivy_contracts::IvyResult;
use std::{
    collections::BTreeMap,
    ffi::OsString,
    io,
    path::{Path, PathBuf},
    process::{ExitStatus, Stdio},
};
use tokio::process::{Child, ChildStderr, ChildStdout};

mod tree;

/// Raw output and completion of the original root. Neither natural root exit nor
/// dropping this handle kills descendants. Dropping it also leaves a live root
/// running. The caller owns deadlines, pipe readers and failure cleanup.
pub struct RootProcess {
    pid: u32,
    pub stdout: Option<ChildStdout>,
    pub stderr: Option<ChildStderr>,
    child: Child,
}

impl RootProcess {
    /// The original root's PID. Its retained native handle supplies identity.
    pub fn id(&self) -> u32 {
        self.pid
    }

    /// Cancellation-safe root wait. It neither drains pipes nor waits for or
    /// proves descendant retirement. ExitStatus retains nonzero exit codes.
    pub async fn wait(&mut self) -> io::Result<ExitStatus> {
        self.child.wait().await
    }

    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        self.child.try_wait()
    }

    /// Best-effort native tree termination, only if the original root is still
    /// live. False means it had exited before termination began. True means a
    /// request was made, not that any process has finished. Errors remain errors;
    /// the caller separately bounds its root wait and pipe drain.
    pub fn kill_tree_if_running(&mut self) -> io::Result<bool> {
        if self.child.try_wait()?.is_some() {
            return Ok(false);
        }
        let raw = self
            .child
            .raw_handle()
            .ok_or_else(|| io::Error::other("Root process handle is unavailable."))?;
        // The child keeps this exact process incarnation alive for the call.
        let handle = unsafe { std::os::windows::io::BorrowedHandle::borrow_raw(raw) };
        tree::kill_if_running(handle, self.pid)
    }
}

/// Direct, windowless launch with the complete supplied environment and the
/// existing executable/cwd admission. Stdin is inherited; stdout/stderr are raw
/// caller-owned pipes. No Job, timeout, output limit, diagnostic capture or drop
/// termination is installed. Set command.cwd to the admitted executable directory
/// when that is the consumer's required working directory.
pub async fn spawn_exact_environment(
    command: &Command,
    root: &Path,
    executables: &BTreeMap<String, PathBuf>,
    environment: BTreeMap<OsString, OsString>,
) -> IvyResult<RootProcess> {
    let resolved = resolve_command(command, root, executables)?;
    let mut child = tokio::process::Command::new(resolved.executable)
        .args(resolved.args)
        .current_dir(resolved.cwd)
        .env_clear()
        .envs(environment)
        .stdin(Stdio::inherit())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(0x08000000)
        .kill_on_drop(false)
        .spawn()
        .map_err(io_error)?;
    let pid = child.id().expect("A newly spawned child has a process ID.");
    Ok(RootProcess {
        pid,
        stdout: child.stdout.take(),
        stderr: child.stderr.take(),
        child,
    })
}
