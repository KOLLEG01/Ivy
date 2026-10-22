//! One bounded process-tree owner shared by host actions and native workloads.
use crate::{paths, process_output::DiagnosticCapture};
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString, Outcome};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    num::NonZeroU64,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, atomic::{AtomicBool, Ordering}},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::{ChildStderr, ChildStdin, ChildStdout, Command as TokioCommand},
    sync::{mpsc, watch},
    task::JoinHandle,
};

#[cfg(windows)]
pub mod root;

#[cfg(all(windows, feature = "qualification-live-retirement"))]
pub mod qualification;

#[derive(Clone, Debug)]
pub struct Command {
    pub executable: String,
    pub args: Vec<OsString>,
    pub cwd: Option<PathBuf>,
    pub timeout_ms: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutputStream {
    Stdout,
    Stderr,
}
pub type OutputSink = Arc<dyn Fn(OutputStream, &str) + Send + Sync>;

#[derive(Default)]
pub struct LaunchContext {
    pub environment: BTreeMap<OsString, OsString>,
    pub job_launcher: Option<PathBuf>,
    pub redact: Vec<JsString>,
    pub on_output: Option<OutputSink>,
    pub raw_output: bool,
    pub maximum_output_bytes: Option<usize>,
    pub execution_cwd: Option<PathBuf>,
    pub without_job: bool,
    pub job_name: Option<String>,
    pub allow_windows_breakaway: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ProcessResult {
    pub exit_code: Option<i32>,
    pub signal: Option<String>,
    pub error_code: Option<String>,
    pub stdout: String,
    pub stderr: String,
    pub truncated: bool,
}

#[derive(Clone)]
pub struct Completion {
    raw: watch::Receiver<Option<Arc<ProcessResult>>>,
    #[cfg(all(windows, feature = "qualification-live-retirement"))]
    withheld: bool,
}
impl Completion {
    fn consumer_view(&self, result: Arc<ProcessResult>) -> Arc<ProcessResult> {
        #[cfg(all(windows, feature = "qualification-live-retirement"))]
        if self.withheld {
            return qualification::consumer_view(result);
        }
        result
    }
    pub fn current(&self) -> Option<Arc<ProcessResult>> {
        self.raw.borrow().clone().map(|result| self.consumer_view(result))
    }
    pub async fn wait(&self) -> Arc<ProcessResult> {
        let mut receiver = self.raw.clone();
        loop {
            if let Some(result) = receiver.borrow_and_update().clone() {
                return self.consumer_view(result);
            }
            if receiver.changed().await.is_err() {
                return Arc::new(ProcessResult {
                    exit_code: None,
                    signal: None,
                    error_code: Some("outcome_unknown".into()),
                    stdout: String::new(),
                    stderr: String::new(),
                    truncated: true,
                });
            }
        }
    }
}

pub struct OwnedProcess {
    pub pid: u32,
    pub stdin: Option<ChildStdin>,
    /// Raw protocol owners take these pipes; diagnostic mode consumes them privately.
    pub stdout: Option<ChildStdout>,
    pub stderr: Option<ChildStderr>,
    pub completion: Completion,
    stop: mpsc::Sender<Duration>,
    #[cfg(unix)]
    group_pid: u32,
    #[cfg(windows)]
    job_retired: Arc<std::sync::atomic::AtomicBool>,
    #[cfg(all(windows, feature = "qualification-live-retirement"))]
    qualification: qualification::Eligibility,
}

/// Optional aggregate committed-memory enforcement for one owned Windows Job.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProcessMemoryLimit {
    pub maximum_job_commit_bytes: NonZeroU64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemoryAccounting {
    WindowsJobCommitted,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemoryLimitNotice {
    WindowsJobMemoryLimit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemoryLimitEvidence {
    /// Installed root policy, never an amount consumed or exhausted.
    pub configured_bytes: u64,
    pub accounting: MemoryAccounting,
    /// Actual native notice on the owned private port/tree. A nested Job can
    /// originate it; this does not identify the exhausted Job or its threshold.
    pub native_notice: MemoryLimitNotice,
}

/// The existing process owner and completion, with a private native observation.
pub struct MemoryLimitedProcess {
    pub process: OwnedProcess,
    configured_bytes: u64,
    observed: Arc<AtomicBool>,
}
impl MemoryLimitedProcess {
    /// A positive native notice alone proves neither tree retirement nor pipe EOF.
    /// Absence is not evidence that no commitment was refused.
    pub fn memory_limit_evidence(&self) -> Option<MemoryLimitEvidence> {
        self.observed.load(Ordering::Acquire).then_some(MemoryLimitEvidence {
            configured_bytes: self.configured_bytes,
            accounting: MemoryAccounting::WindowsJobCommitted,
            native_notice: MemoryLimitNotice::WindowsJobMemoryLimit,
        })
    }
}

struct MemoryRequest {
    maximum: usize,
    observed: Arc<AtomicBool>,
}
impl OwnedProcess {
    /// Send termination immediately, escalate after grace, retain unknown after
    /// grace+2s. RuntimeOwner's cooperative control-file interval happens BEFORE this.
    pub async fn stop(&self, grace: Duration) -> IvyResult<Arc<ProcessResult>> {
        if grace > Duration::from_secs(60) {
            return Err(invalid(
                "Process stop grace must be finite and at most one minute.",
            ));
        }
        if self.completion.current().is_none() {
            let _ = self.stop.send(grace).await;
        }
        Ok(self.completion.wait().await)
    }
}

/// Observe only the tree created by this owner. Await completion before settling
/// a final stop record; a pending or unproved Windows Job always returns false.
/// Unix keeps the original ten 50ms probes and accepts only ESRCH as retirement.
pub async fn owned_process_tree_stopped(process: &OwnedProcess) -> IvyResult<bool> {
    #[cfg(all(windows, feature = "qualification-live-retirement"))]
    if process.completion.withheld {
        return Ok(false);
    }
    #[cfg(windows)]
    return Ok(process.job_retired.load(std::sync::atomic::Ordering::Acquire));
    #[cfg(unix)]
    {
        for _ in 0..10 {
            if unsafe { libc::kill(-(process.group_pid as i32), 0) } != 0 {
                return Ok(std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH));
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        Ok(false)
    }
    #[cfg(not(any(windows, unix)))]
    Ok(false)
}

fn invalid(message: &str) -> IvyError {
    IvyError::new("invalid_arguments", message)
}
fn io_error(_: std::io::Error) -> IvyError {
    IvyError::internal()
}

pub fn runtime_environment(extra: &BTreeMap<OsString, OsString>) -> BTreeMap<OsString, OsString> {
    const ALLOWED: &[&str] = &[
        "systemroot",
        "windir",
        "comspec",
        "path",
        "pathext",
        "temp",
        "tmp",
        "tmpdir",
        "userprofile",
        "home",
        "codex_home",
        "appdata",
        "localappdata",
        "programdata",
        "allusersprofile",
        "programfiles",
        "programfiles(x86)",
        "lang",
        "lc_all",
        "timezone",
    ];
    let overrides: Vec<_> = extra
        .keys()
        .map(|key| key.to_string_lossy().to_lowercase())
        .collect();
    let mut result: BTreeMap<_, _> = std::env::vars_os()
        .filter(|(key, _)| {
            let key = key.to_string_lossy().to_lowercase();
            ALLOWED.contains(&key.as_str()) && !overrides.contains(&key)
        })
        .collect();
    result.insert("NODE_NO_WARNINGS".into(), "1".into());
    result.extend(extra.clone());
    result
}

pub struct ResolvedCommand {
    pub executable: PathBuf,
    pub cwd: PathBuf,
    pub args: Vec<OsString>,
}
pub fn resolve_command(
    command: &Command,
    root: &Path,
    executables: &BTreeMap<String, PathBuf>,
) -> IvyResult<ResolvedCommand> {
    let base = paths::realpath(root).map_err(io_error)?;
    let cwd = paths::realpath(&base.join(command.cwd.as_deref().unwrap_or(Path::new("."))))
        .map_err(io_error)?;
    if !paths::inside(&base, &cwd).map_err(io_error)? {
        return Err(invalid(
            "Command working directory leaves its artifact/source root.",
        ));
    }
    let configured = executables.get(&command.executable);
    let executable = paths::realpath(
        &configured
            .cloned()
            .unwrap_or_else(|| base.join(&command.executable)),
    )
    .map_err(io_error)?;
    if !(configured.is_some_and(|path| path.is_absolute())
        || (configured.is_none()
            && !Path::new(&command.executable).is_absolute()
            && paths::inside(&base, &executable).map_err(io_error)?))
    {
        return Err(invalid(
            "Command executable must be inside the artifact or explicitly mapped by host configuration.",
        ));
    }
    // Match the whole-path suffix, including dot-only basenames such as `.CMD`.
    let bytes = executable.as_os_str().as_encoded_bytes();
    let script = bytes.len() >= 4 && [b".cmd", b".bat", b".ps1"].iter()
        .any(|suffix| bytes[bytes.len() - 4..].eq_ignore_ascii_case(*suffix));
    if !executable.is_file() || script {
        return Err(invalid(
            "Commands need a direct executable; shell scripts require an explicit interpreter and argument array.",
        ));
    }
    Ok(ResolvedCommand {
        executable,
        cwd,
        args: command.args.clone(),
    })
}

type Capture = Arc<Mutex<DiagnosticCapture>>;
fn capture_pipe<R: AsyncRead + Unpin + Send + 'static>(
    mut pipe: R,
    capture: Capture,
    stream: OutputStream,
    sink: Option<OutputSink>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut buffer = [0; 8192];
        loop {
            let count = match pipe.read(&mut buffer).await {
                Ok(count) => count,
                Err(_) => break,
            };
            if count == 0 {
                break;
            }
            capture.lock().unwrap_or_else(|p| p.into_inner()).push_to(
                &buffer[..count],
                false,
                |safe| {
                    if let Some(sink) = &sink {
                        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            sink(stream, safe)
                        }));
                    }
                },
            );
        }
    })
}

#[cfg(unix)]
fn signal_tree(pid: u32, hard: bool) {
    // Child starts its own group; never signal the caller's group.
    unsafe {
        libc::kill(
            -(pid as i32),
            if hard { libc::SIGKILL } else { libc::SIGTERM },
        );
    }
}

#[cfg(target_os = "linux")]
fn signal_name(signal: i32) -> String {
    let names = [
        (libc::SIGHUP, "SIGHUP"),
        (libc::SIGINT, "SIGINT"),
        (libc::SIGQUIT, "SIGQUIT"),
        (libc::SIGILL, "SIGILL"),
        (libc::SIGTRAP, "SIGTRAP"),
        (libc::SIGABRT, "SIGABRT"),
        (libc::SIGBUS, "SIGBUS"),
        (libc::SIGFPE, "SIGFPE"),
        (libc::SIGKILL, "SIGKILL"),
        (libc::SIGUSR1, "SIGUSR1"),
        (libc::SIGSEGV, "SIGSEGV"),
        (libc::SIGUSR2, "SIGUSR2"),
        (libc::SIGPIPE, "SIGPIPE"),
        (libc::SIGALRM, "SIGALRM"),
        (libc::SIGTERM, "SIGTERM"),
        (libc::SIGCHLD, "SIGCHLD"),
        (libc::SIGCONT, "SIGCONT"),
        (libc::SIGSTOP, "SIGSTOP"),
        (libc::SIGTSTP, "SIGTSTP"),
        (libc::SIGTTIN, "SIGTTIN"),
        (libc::SIGTTOU, "SIGTTOU"),
        (libc::SIGURG, "SIGURG"),
        (libc::SIGXCPU, "SIGXCPU"),
        (libc::SIGXFSZ, "SIGXFSZ"),
        (libc::SIGVTALRM, "SIGVTALRM"),
        (libc::SIGPROF, "SIGPROF"),
        (libc::SIGWINCH, "SIGWINCH"),
        (libc::SIGIO, "SIGIO"),
        (libc::SIGPWR, "SIGPWR"),
        (libc::SIGSYS, "SIGSYS"),
    ];
    names
        .iter()
        .find(|(number, _)| *number == signal)
        .map_or_else(|| format!("SIG{signal}"), |(_, name)| (*name).into())
}

#[cfg(all(unix, not(target_os = "linux")))]
fn signal_name(signal: i32) -> String {
    format!("SIG{signal}")
}

pub async fn spawn(
    command: &Command,
    root: &Path,
    executables: &BTreeMap<String, PathBuf>,
    context: LaunchContext,
) -> IvyResult<OwnedProcess> {
    validate_capture(&context)?;
    let resolved = resolve_command(command, root, executables)?;
    let process = spawn_resolved(resolved, root, context, None, None).await?;
    #[cfg(all(windows, feature = "qualification-live-retirement"))]
    let process = {
        let mut process = process;
        if process.qualification == qualification::Eligibility::AcknowledgedRawCandidate {
            process.qualification = qualification::Eligibility::OrdinaryReady;
        }
        process
    };
    Ok(process)
}

/// Use exactly context.environment while retaining ordinary executable/cwd
/// admission and the shared process-tree owner. No ambient keys are added.
/// With raw_output the caller owns both byte pipes, their limits, deadlines,
/// cancellation and stdin closure; no diagnostic stream is retained here.
/// Await stop/completion and owned_process_tree_stopped before claiming
/// retirement. The ordinary spawn API keeps its filtered runtime environment.
pub async fn spawn_exact_environment(
    command: &Command,
    root: &Path,
    executables: &BTreeMap<String, PathBuf>,
    mut context: LaunchContext,
) -> IvyResult<OwnedProcess> {
    validate_capture(&context)?;
    let resolved = resolve_command(command, root, executables)?;
    let environment = std::mem::take(&mut context.environment);
    spawn_resolved(resolved, root, context, Some(environment), None).await
}

/// Exact environment and existing owned retirement/IO, with an explicit Job cap.
/// Requested accounting is refused before launch on unsupported targets or escape
/// modes. Raw callers retain their own finite IO/deadline and EOF obligations.
pub async fn spawn_exact_environment_with_memory_limit(
    command: &Command,
    root: &Path,
    executables: &BTreeMap<String, PathBuf>,
    mut context: LaunchContext,
    memory_limit: ProcessMemoryLimit,
) -> Result<MemoryLimitedProcess, SpawnFailure> {
    if !cfg!(windows) || context.without_job || context.allow_windows_breakaway {
        return Err(invalid("Requested Job memory enforcement is unavailable for this launch.").into());
    }
    let configured_bytes = memory_limit.maximum_job_commit_bytes.get();
    let maximum = usize::try_from(configured_bytes)
        .map_err(|_| invalid("Requested Job memory limit is not representable."))?;
    validate_capture(&context)?;
    let resolved = resolve_command(command, root, executables)?;
    let environment = std::mem::take(&mut context.environment);
    let observed = Arc::new(AtomicBool::new(false));
    let process = spawn_resolved_with_stdio(
        resolved, root, context, Some(environment), None, false,
        Some(MemoryRequest { maximum, observed: observed.clone() }),
    ).await?;
    Ok(MemoryLimitedProcess { process, configured_bytes, observed })
}

fn validate_capture(context: &LaunchContext) -> IvyResult<()> {
    if context.redact.iter().any(|value| value.0.len() > 65536) {
        return Err(invalid("Redaction values exceed their bounded length."));
    }
    if !(65536..=16 * 1024 * 1024).contains(&context.maximum_output_bytes.unwrap_or(65536)) {
        return Err(invalid("Captured command output needs an explicit bounded size."));
    }
    Ok(())
}

/// Shared process-tree owner. Exec-file supplies its separate executable/argv0
/// semantics; ordinary and exact-environment commands keep resolve_command.
pub(crate) async fn spawn_resolved(
    resolved: ResolvedCommand,
    root: &Path,
    context: LaunchContext,
    exact_environment: Option<BTreeMap<OsString, OsString>>,
    argv0: Option<OsString>,
) -> IvyResult<OwnedProcess> {
    spawn_resolved_detailed(resolved, root, context, exact_environment, argv0)
        .await
        .map_err(|failure| failure.error)
}

/// Launch failure keeps native OS evidence separate from an unconfirmed launch.
#[derive(Debug)]
pub struct SpawnFailure {
    pub error: IvyError,
    pub source: Option<std::io::Error>,
}
impl From<IvyError> for SpawnFailure {
    fn from(error: IvyError) -> Self { Self { error, source: None } }
}

/// Retain the native spawn failure for execFile consumers without changing the
/// public process error or the shared process-tree owner.
pub(crate) async fn spawn_resolved_detailed(
    resolved: ResolvedCommand,
    root: &Path,
    context: LaunchContext,
    exact_environment: Option<BTreeMap<OsString, OsString>>,
    argv0: Option<OsString>,
) -> Result<OwnedProcess, SpawnFailure> {
    spawn_resolved_with_stdio(resolved, root, context, exact_environment, argv0, false, None).await
}

/// The same process-tree owner with null devices instead of caller/capture pipes.
pub(crate) async fn spawn_resolved_ignored(
    resolved: ResolvedCommand,
    root: &Path,
    context: LaunchContext,
    environment: BTreeMap<OsString, OsString>,
    argv0: OsString,
) -> Result<OwnedProcess, SpawnFailure> {
    spawn_resolved_with_stdio(resolved, root, context, Some(environment), Some(argv0), true, None).await
}

async fn spawn_resolved_with_stdio(
    resolved: ResolvedCommand,
    root: &Path,
    context: LaunchContext,
    exact_environment: Option<BTreeMap<OsString, OsString>>,
    argv0: Option<OsString>,
    ignored_stdio: bool,
    memory_request: Option<MemoryRequest>,
) -> Result<OwnedProcess, SpawnFailure> {
    let maximum = context.maximum_output_bytes.unwrap_or(65536);
    let use_job = cfg!(windows) && !context.without_job;
    if context
        .job_name
        .as_ref()
        .is_some_and(|name| !use_job || !crate::platform::windows_arguments::valid_job_name(name))
    {
        return Err(invalid(
            "A named process job requires the Windows job launcher.",
        ).into());
    }
    #[cfg(windows)]
    let launch_listener = if use_job {
        Some(match &memory_request {
            Some(memory) => crate::platform::windows_launch::Listener::with_memory_limit(Some(memory.maximum)),
            None => crate::platform::windows_launch::Listener::new(),
        }.map_err(io_error)?)
    } else {
        None
    };
    let mut process = if use_job {
        let launcher = context.job_launcher.clone()
            .unwrap_or_else(|| crate::native_plan::native_binary_path(root, "ivy-job"));
        let launcher = paths::realpath(&launcher).map_err(io_error)?;
        let mut process = TokioCommand::new(launcher);
        process.args([
            OsString::from(if memory_request.is_some() { "--memory-parent" } else { "--parent" }),
            std::process::id().to_string().into(),
        ]);
        if let Some(memory) = &memory_request {
            process.arg(memory.maximum.to_string());
        }
        if context.allow_windows_breakaway {
            process.arg("--allow-breakaway");
        }
        if let Some(name) = &context.job_name {
            process.args(["--job-name", name]);
        }
        #[cfg(windows)]
        if let Some(listener) = &launch_listener {
            process.args(["--launch-pipe", &listener.name]);
        }
        if let Some(argv0) = &argv0 { process.arg("--argv0").arg(argv0); }
        process.arg(&resolved.executable);
        process
    } else {
        TokioCommand::new(&resolved.executable)
    };
    #[cfg(unix)]
    if let Some(argv0) = argv0 {
        use std::os::unix::process::CommandExt;
        process.as_std_mut().arg0(argv0);
    }
    process
        .args(resolved.args)
        .current_dir(
            context
                .execution_cwd
                .as_ref()
                .map(|path| paths::realpath(path))
                .transpose()
                .map_err(io_error)?
                .unwrap_or(resolved.cwd),
        )
        .env_clear()
        .kill_on_drop(true)
        .envs(exact_environment.unwrap_or_else(|| runtime_environment(&context.environment)))
        .stdin(if ignored_stdio { std::process::Stdio::null() } else { std::process::Stdio::piped() })
        .stdout(if ignored_stdio { std::process::Stdio::null() } else { std::process::Stdio::piped() })
        .stderr(if ignored_stdio { std::process::Stdio::null() } else { std::process::Stdio::piped() });
    #[cfg(windows)]
    {
        process.creation_flags(0x08000000);
    }
    #[cfg(unix)]
    {
        // Like Node's detached spawn: a new session and process group. Only an
        // async-signal-safe syscall runs between fork and exec.
        unsafe {
            process.pre_exec(|| {
                if libc::setsid() < 0 {
                    Err(std::io::Error::last_os_error())
                } else {
                    Ok(())
                }
            });
        }
    }
    let mut child = process
        .spawn()
        .map_err(|source| SpawnFailure {
            error: IvyError::new("spawn_failed", "Command could not start."), source: Some(source),
        })?;
    let pid = child.id().expect("spawned process has an identity");
    // The launch handshake races acknowledgement against Child::wait(), which
    // closes any stdin still held by Child. Retain the caller's pipe first.
    let stdin = child.stdin.take();
    #[cfg(windows)]
    let mut launch_proof = if let Some(listener) = launch_listener {
        match tokio::time::timeout(Duration::from_secs(5), listener.accept(&mut child)).await {
            Ok(Ok(proof)) => Some(proof),
            _ => {
                let _ = child.start_kill();
                let _ = tokio::time::timeout(Duration::from_secs(2), child.wait()).await;
                return Err(IvyError::new(
                    "spawn_failed",
                    "Command launch acknowledgement failed.",
                )
                .with_outcome(Outcome::Unknown).into());
            }
        }
    } else {
        None
    };
    let mut stdout = child.stdout.take();
    let mut stderr = child.stderr.take();
    #[cfg(all(windows, feature = "qualification-live-retirement"))]
    let qualification = if use_job && launch_proof.is_some()
        && context.raw_output && !ignored_stdio && !context.allow_windows_breakaway
        && memory_request.is_none() && argv0.is_none() && context.job_name.is_none()
        && context.on_output.is_none() && stdin.is_some() && stdout.is_some() && stderr.is_some()
    {
        qualification::Eligibility::AcknowledgedRawCandidate
    } else {
        qualification::Eligibility::Unavailable
    };
    let make_capture = || {
        Arc::new(Mutex::new(DiagnosticCapture::new(
            context.redact.iter().map(|s| s.0.clone()).collect(),
            maximum,
        )))
    };
    let stdout_capture = make_capture();
    let stderr_capture = make_capture();
    let mut readers = Vec::new();
    if !ignored_stdio && !context.raw_output {
        readers.push(capture_pipe(
            stdout.take().unwrap(),
            stdout_capture.clone(),
            OutputStream::Stdout,
            context.on_output.clone(),
        ));
        readers.push(capture_pipe(
            stderr.take().unwrap(),
            stderr_capture.clone(),
            OutputStream::Stderr,
            context.on_output.clone(),
        ));
    }
    let (stop, mut requests) = mpsc::channel::<Duration>(1);
    let (finished, completion) = watch::channel(None);
    #[cfg(windows)]
    let job_retired = Arc::new(std::sync::atomic::AtomicBool::new(false));
    #[cfg(windows)]
    let retirement_observation = job_retired.clone();
    tokio::spawn(async move {
        let status;
        let mut unknown = false;
        let retirement_deadline;
        tokio::select! {
            observation = async {
                #[cfg(windows)]
                if memory_request.is_some() {
                    let proof = launch_proof.as_mut()
                        .ok_or_else(|| std::io::Error::other("Limited Job ownership is unavailable."))?;
                    loop {
                        if proof.poll_memory_limit()? {
                            return Ok(());
                        }
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                }
                std::future::pending::<std::io::Result<()>>().await
            } => {
                unknown = observation.is_err();
                retirement_deadline = tokio::time::Instant::now() + Duration::from_secs(2);
                #[cfg(windows)] {
                    if let Some(proof) = &launch_proof {
                        if proof.memory_limit_seen() {
                            if let Some(memory) = &memory_request {
                                memory.observed.store(true, Ordering::Release);
                            }
                        }
                        let _ = proof.terminate();
                    }
                }
                let _ = child.start_kill();
                match tokio::time::timeout_at(retirement_deadline, child.wait()).await {
                    Ok(result) => { status = result.ok(); }
                    Err(_) => { status = None; unknown = true; }
                }
            }
            result = child.wait() => {
                status = result.ok();
                retirement_deadline = tokio::time::Instant::now() + Duration::from_secs(2);
            }
            request = requests.recv() => {
                // Losing the handle does not silently prove completion. Its tree is
                // explicitly stopped and reaped, just like an explicit stop request.
                let grace = request.unwrap_or(Duration::ZERO);
                retirement_deadline = tokio::time::Instant::now() + grace + Duration::from_secs(2);
                #[cfg(windows)] {
                    if let Some(proof) = &launch_proof {
                        let _ = proof.terminate();
                    }
                    let _ = child.start_kill();
                }
                #[cfg(unix)] signal_tree(pid, false);
                match tokio::time::timeout(grace, child.wait()).await {
                    Ok(result) => { status = result.ok(); }
                    Err(_) => {
                        #[cfg(windows)] { let _ = child.start_kill(); }
                        #[cfg(unix)] signal_tree(pid, true);
                        match tokio::time::timeout_at(retirement_deadline, child.wait()).await {
                            Ok(result) => { status = result.ok(); }
                            Err(_) => { status = None; unknown = true; }
                        }
                    }
                }
            }
        }
        #[cfg(unix)]
        signal_tree(pid, true);
        #[cfg(windows)]
        if let Some(proof) = &mut launch_proof {
            match proof.retire(retirement_deadline).await {
                Ok(()) => retirement_observation.store(true, std::sync::atomic::Ordering::Release),
                Err(_) => unknown = true,
            }
            // A notice can race child exit/stop. Drain it through the same retained
            // owner before publishing Completion, even if retirement is unknown.
            if proof.memory_limit_seen() {
                if let Some(memory) = &memory_request {
                    memory.observed.store(true, Ordering::Release);
                }
            }
        }
        // Pipe draining shares the retirement deadline. Raw protocol owners drain
        // their own pipes; EOF alone never proves descendant retirement.
        if !readers.is_empty() {
            let drained = tokio::time::timeout_at(retirement_deadline, async {
                for reader in &mut readers {
                    let _ = reader.await;
                }
            })
            .await;
            if drained.is_err() {
                for reader in &readers {
                    reader.abort();
                }
                unknown = true;
            }
        }
        let finish_capture = |capture: Capture, stream| {
            let mut capture = capture.lock().unwrap_or_else(|p| p.into_inner());
            capture.push_to(&[], true, |safe| {
                if let Some(sink) = context.on_output.as_ref().filter(|_| !context.raw_output) {
                    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        sink(stream, safe)
                    }));
                }
            });
            (capture.text(), capture.truncated())
        };
        let (stdout, out_truncated) = finish_capture(stdout_capture, OutputStream::Stdout);
        let (stderr, err_truncated) = finish_capture(stderr_capture, OutputStream::Stderr);
        #[cfg(unix)]
        let signal = {
            use std::os::unix::process::ExitStatusExt;
            status.and_then(|s| s.signal()).map(signal_name)
        };
        #[cfg(not(unix))]
        let signal = None;
        let result = ProcessResult {
            exit_code: status.and_then(|s| s.code()),
            signal,
            error_code: if unknown || status.is_none() {
                Some("outcome_unknown".into())
            } else {
                None
            },
            stdout,
            stderr,
            truncated: out_truncated || err_truncated || unknown,
        };
        let _ = finished.send(Some(Arc::new(result)));
    });
    Ok(OwnedProcess {
        pid,
        stdin,
        stdout,
        stderr,
        completion: Completion {
            raw: completion,
            #[cfg(all(windows, feature = "qualification-live-retirement"))]
            withheld: false,
        },
        stop,
        #[cfg(unix)]
        group_pid: pid,
        #[cfg(windows)]
        job_retired,
        #[cfg(all(windows, feature = "qualification-live-retirement"))]
        qualification,
    })
}

pub async fn run_command(
    command: &Command,
    root: &Path,
    executables: &BTreeMap<String, PathBuf>,
    context: LaunchContext,
    input: Option<&[u8]>,
) -> IvyResult<Arc<ProcessResult>> {
    if input.is_some_and(|bytes| bytes.len() > 65536) {
        return Err(IvyError::new(
            "limit_exceeded",
            "Command input exceeds its bounded size.",
        ));
    }
    let deadline = tokio::time::Instant::now() + Duration::from_millis(command.timeout_ms);
    let mut process = tokio::time::timeout_at(deadline, spawn(command, root, executables, context))
        .await
        .map_err(|_| {
            IvyError::new("deadline_exceeded", "Command exceeded its finite deadline.")
                .with_outcome(Outcome::Unknown)
        })??;
    let completion = process.completion.clone();
    // Include blocked stdin in the command deadline. EPIPE is only evidence for
    // the child, never a reason to terminate its supervising parent.
    let result = tokio::time::timeout_at(deadline, async {
        if let Some(input) = input {
            if let Some(mut stdin) = process.stdin.take() {
                let _ = stdin.write_all(input).await;
                let _ = stdin.shutdown().await;
            }
        }
        completion.wait().await
    })
    .await;
    let result = match result {
        Ok(result) => result,
        Err(_) => {
            let _ = process.stop(Duration::from_millis(250)).await;
            return Err(IvyError::new(
                "deadline_exceeded",
                "Command exceeded its finite deadline.",
            )
            .with_outcome(Outcome::Unknown));
        }
    };
    if result.exit_code != Some(0) || result.error_code.is_some() {
        return Err(IvyError::new(
            result.error_code.as_deref().unwrap_or("command_failed"),
            "Command failed; bounded diagnostics record the outcome.",
        )
        .with_outcome(Outcome::Unknown)
        .with_details(IvyJson::object([
            (
                "exitCode",
                result
                    .exit_code
                    .map_or(IvyJson::Null, |v| IvyJson::Number(v as f64)),
            ),
            (
                "signal",
                result.signal.clone().map_or(IvyJson::Null, IvyJson::from),
            ),
        ])));
    }
    Ok(result)
}
