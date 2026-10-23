//! Bounded UTF-8 execFile behavior for host preflight and source tools. Artifact
//! command restrictions remain in process::spawn; both share the same tree owner.
use crate::{IvyError, IvyResult, node_path, process::{self, LaunchContext, OutputStream, ResolvedCommand}, process_output::Utf8Decoder};
use ivy_contracts::JsString;
use std::{collections::BTreeMap, ffi::OsString, path::{Path, PathBuf}, sync::{Arc,Mutex}, time::Duration};
use tokio::{io::{AsyncRead, AsyncReadExt, AsyncWriteExt}, sync::mpsc};
use tokio_util::sync::CancellationToken;

pub struct FileCommand {
    pub executable: OsString,
    pub args: Vec<OsString>,
    pub cwd: Option<PathBuf>,
    pub timeout_ms: u32,
    /// Each decoded UTF-8 stream has its own byte limit.
    pub maximum_buffer_bytes: usize,
}
#[derive(Default)]
pub struct FileOptions {
    /// None inherits the complete executing process environment.
    pub environment: Option<BTreeMap<OsString, OsString>>,
    pub job_launcher: Option<PathBuf>,
    pub cancellation: CancellationToken,
}

/// Direct, hidden launch with ignored stdin/stdout/stderr and no shell.
pub struct SpawnCommand {
    pub executable: OsString,
    pub args: Vec<OsString>,
    /// None inherits the executing process's working directory.
    pub cwd: Option<PathBuf>,
}
#[derive(Default)]
pub struct SpawnOptions {
    /// None inherits the complete executing process environment, without filtering.
    pub environment: Option<BTreeMap<OsString, OsString>>,
    pub job_launcher: Option<PathBuf>,
}
impl SpawnOptions {
    /// Select the Job helper from the caller's already admitted immutable artifact.
    /// Target and legacy layouts are explicit; a missing helper never falls back.
    pub fn for_component(artifact_root: &Path, layout: crate::portable_native_files::NativeHelperLayout) -> IvyResult<Self> {
        #[cfg(windows)]
        let job_launcher = Some(crate::portable_native_files::component_native_helper(artifact_root, "ivy-job.exe", layout)?);
        #[cfg(not(windows))]
        let job_launcher = { let _ = (artifact_root, layout); None };
        Ok(Self { job_launcher, ..Default::default() })
    }
}

/// Native executable/cwd lookup and exact argv share the file resolver. All three
/// streams are actual null devices, so no pipe/capture or stdin writer is created.
/// The caller owns its deadline/domain errors; retain OwnedProcess and await its
/// completion/stop plus owned_process_tree_stopped before claiming retirement.
/// Dropping OwnedProcess requests tree retirement; dropping a completion observer
/// alone does not. On Windows bootstrap_root supplies the legacy helper default;
/// production component callers should use SpawnOptions::for_component.
pub async fn spawn_ignored(command: SpawnCommand, bootstrap_root: PathBuf, options: SpawnOptions) -> Result<process::OwnedProcess, process::SpawnFailure> {
    let environment = options.environment.unwrap_or_else(|| std::env::vars_os().collect());
    let (command, environment, resolved) = tokio::task::spawn_blocking(move || {
        let resolved = resolve_parts(&command.executable, &command.args, command.cwd.as_deref(), &environment);
        (command, environment, resolved)
    }).await.map_err(|_| process::SpawnFailure::from(IvyError::internal()))?;
    let resolved = resolved.map_err(|source| process::SpawnFailure {
        error: IvyError::new("spawn_failed", "Command could not start."), source: Some(source),
    })?;
    let launch = LaunchContext { raw_output: true, job_launcher: options.job_launcher, ..Default::default() };
    process::spawn_resolved_ignored(resolved, &bootstrap_root, launch, environment, command.executable).await
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileFailure { Spawn, Exit, Timeout, Cancelled, MaxBuffer(OutputStream), Unknown }
#[derive(Debug)]
pub struct FileResult {
    pub stdout: JsString,
    pub stderr: JsString,
    pub exit_code: Option<i32>,
    pub signal: Option<String>,
    pub failure: Option<FileFailure>,
}
fn empty(failure: FileFailure) -> FileResult { FileResult { stdout: "".into(), stderr: "".into(), exit_code: None, signal: None, failure: Some(failure) } }

pub(crate) struct FileExecution {
    pub result: FileResult,
    pub spawn_error: Option<std::io::Error>,
}
impl From<FileResult> for FileExecution {
    fn from(result: FileResult) -> Self { Self { result, spawn_error: None } }
}

/// Detached ownership ensures a dropped waiter cannot leave a live process
/// without timeout/cancellation/retirement handling. Failure retains both streams.
pub async fn exec_file(command: FileCommand, bootstrap_root: PathBuf, options: FileOptions) -> IvyResult<FileResult> {
    Ok(exec_file_detailed(command, bootstrap_root, options).await?.result)
}

/// Preserve an original execFile call that leaves timeout unset. This private
/// path keeps output limits, cancellation and the same native process owner;
/// public FileCommand callers still require a finite positive deadline.
pub(crate) async fn exec_file_without_timeout(mut command: FileCommand, bootstrap_root: PathBuf, options: FileOptions) -> IvyResult<FileResult> {
    if command.maximum_buffer_bytes > 64 * 1024 * 1024 { return Err(IvyError::new("invalid_arguments", "Exec-file requires bounded time and output limits.")); }
    command.timeout_ms = 0;
    Ok(tokio::spawn(run(command, bootstrap_root, options, Input::Open)).await.map_err(|_| IvyError::internal())?.result)
}

pub(crate) async fn exec_file_detailed(command: FileCommand, bootstrap_root: PathBuf, options: FileOptions) -> IvyResult<FileExecution> {
    execute(command, bootstrap_root, options, Input::Open).await
}

/// The retained Windows scheduler adapter accepts one UTF-8 JSON request on
/// stdin. Keep its write/EOF under the same deadline and tree owner as output;
/// a rejected request closing the pipe early does not replace its actual result.
pub(crate) async fn exec_file_input(command: FileCommand, bootstrap_root: PathBuf, options: FileOptions, input: Vec<u8>) -> IvyResult<FileResult> {
    Ok(execute(command, bootstrap_root, options, Input::Bytes(input)).await?.result)
}

enum Input { Open, Closed, Bytes(Vec<u8>) }

async fn execute(command: FileCommand, bootstrap_root: PathBuf, options: FileOptions, input: Input) -> IvyResult<FileExecution> {
    if command.timeout_ms == 0 || command.timeout_ms > 3_600_000 || command.maximum_buffer_bytes > 64 * 1024 * 1024 {
        return Err(IvyError::new("invalid_arguments", "Exec-file requires bounded time and output limits."));
    }
    tokio::spawn(run(command, bootstrap_root, options, input)).await.map_err(|_| IvyError::internal())
}

/// Source tools close stdin and count raw bytes across both streams.
/// Public exec-file capture instead limits each decoded UTF-8 stream.
pub(crate) async fn source_output(command: FileCommand, bootstrap_root: PathBuf) -> IvyResult<FileResult> {
    Ok(tokio::spawn(run(command, bootstrap_root, FileOptions::default(), Input::Closed)).await.map_err(|_| IvyError::internal())?.result)
}

async fn collect<R: AsyncRead + Unpin>(
    mut pipe: R,
    maximum: usize,
    stream: OutputStream,
    overflow: mpsc::Sender<OutputStream>,
    stop_capture: CancellationToken,
    text: Arc<Mutex<String>>,
    combined: Option<Arc<std::sync::atomic::AtomicUsize>>,
) -> std::io::Result<()> {
    let mut decoder = Utf8Decoder::preserving_bom();
    let mut bytes = vec![0u8; 65536];
    loop {
        let read = pipe.read(&mut bytes).await?;
        if !stop_capture.is_cancelled() {
            if combined.as_ref().is_some_and(|total| {
                total
                    .fetch_add(read, std::sync::atomic::Ordering::Relaxed)
                    .saturating_add(read)
                    > maximum
            }) {
                stop_capture.cancel();
                let _ = overflow.try_send(stream);
            } else {
                let chunk = decoder.decode_text(&bytes[..read], read == 0);
                let mut text = text.lock().unwrap_or_else(|p| p.into_inner());
                let available = if combined.is_some() {
                    chunk.len()
                } else {
                    maximum.saturating_sub(text.len())
                };
                let mut end = available.min(chunk.len());
                while !chunk.is_char_boundary(end) {
                    end -= 1;
                }
                text.push_str(&chunk[..end]);
                if chunk.len() > available {
                    stop_capture.cancel();
                    let _ = overflow.try_send(stream);
                }
            }
        }
        if read == 0 {
            break;
        }
    }
    Ok(())
}

async fn run(command: FileCommand, bootstrap_root: PathBuf, options: FileOptions, input: Input) -> FileExecution {
    let synchronous_input = matches!(&input, Input::Closed);
    if options.cancellation.is_cancelled() { return empty(FileFailure::Cancelled).into() }
    let environment = options.environment.unwrap_or_else(|| std::env::vars_os().collect());
    let (command,environment,resolved) = match tokio::task::spawn_blocking(move || {
        let resolved=resolve(&command,&environment);(command,environment,resolved)
    }).await {Ok(value)=>value,Err(_)=>return empty(FileFailure::Unknown).into()};
    let resolved = match resolved { Ok(value) => value, Err(error) => return FileExecution { result: empty(FileFailure::Spawn), spawn_error: Some(error) } };
    if options.cancellation.is_cancelled() { return empty(FileFailure::Cancelled).into() }
    let launch = LaunchContext { raw_output: true, job_launcher: options.job_launcher, ..Default::default() };
    let deadline = (command.timeout_ms != 0).then(|| tokio::time::Instant::now() + Duration::from_millis(command.timeout_ms as u64));
    let mut child = match process::spawn_resolved_detailed(resolved, &bootstrap_root, launch, Some(environment), Some(command.executable.clone())).await {
        Ok(value) => value, Err(error) => return FileExecution { result: empty(if error.error.outcome == ivy_contracts::Outcome::Unknown { FileFailure::Unknown } else { FileFailure::Spawn }), spawn_error: error.source },
    };
    // execFile leaves stdin open; spawnSync's absent input is immediate EOF.
    let writer = match input {
        Input::Open => None,
        Input::Closed => { drop(child.stdin.take()); None },
        Input::Bytes(bytes) => child.stdin.take().map(|mut pipe| tokio::spawn(async move {
            let _ = pipe.write_all(&bytes).await;
            let _ = pipe.shutdown().await;
        })),
    };
    let (overflow, mut exceeded) = mpsc::channel(2);
    let stop_capture = CancellationToken::new();
    let out=Arc::new(Mutex::new(String::new()));let err=Arc::new(Mutex::new(String::new()));
    let combined = synchronous_input.then(|| Arc::new(std::sync::atomic::AtomicUsize::new(0)));
    let mut stdout = tokio::spawn(collect(child.stdout.take().unwrap(), command.maximum_buffer_bytes, OutputStream::Stdout, overflow.clone(), stop_capture.clone(),out.clone(),combined.clone()));
    let mut stderr = tokio::spawn(collect(child.stderr.take().unwrap(), command.maximum_buffer_bytes, OutputStream::Stderr, overflow.clone(), stop_capture.clone(),err.clone(),combined));
    let completion = child.completion.clone();
    let mut failure = tokio::select! {
        biased;
        stream = exceeded.recv() => stream.map(FileFailure::MaxBuffer),
        _ = options.cancellation.cancelled() => Some(FileFailure::Cancelled),
        _ = async { match deadline { Some(at) => tokio::time::sleep_until(at).await, None => std::future::pending::<()>().await } } => Some(FileFailure::Timeout),
        _ = completion.wait() => None,
    };
    drop(overflow);
    let result = if failure.is_some() { stop_capture.cancel(); child.stop(Duration::ZERO).await.ok() } else { Some(completion.wait().await) };
    let drain = async { ((&mut stdout).await, (&mut stderr).await) };
    let captured = tokio::time::timeout(Duration::from_secs(2), drain).await;
    if !matches!(captured,Ok((Ok(Ok(())),Ok(Ok(()))))) {
        stdout.abort();stderr.abort();failure=Some(FileFailure::Unknown);
    }
    if let Some(writer) = writer { writer.abort(); let _ = writer.await; }
    let stdout=std::mem::take(&mut *out.lock().unwrap_or_else(|p|p.into_inner())).into();
    let stderr=std::mem::take(&mut *err.lock().unwrap_or_else(|p|p.into_inner())).into();
    if failure.is_none() { failure = exceeded.try_recv().ok().map(FileFailure::MaxBuffer); }
    let Some(result) = result else { return FileResult { stdout, stderr, exit_code: None, signal: None, failure: Some(FileFailure::Unknown) }.into() };
    if result.error_code.is_some() { failure = Some(FileFailure::Unknown); }
    else if failure.is_none() && (result.exit_code != Some(0) || result.signal.is_some()) { failure = Some(FileFailure::Exit); }
    FileResult { stdout, stderr, exit_code: result.exit_code, signal: result.signal.clone(), failure }.into()
}

fn resolve(command: &FileCommand, environment: &BTreeMap<OsString, OsString>) -> std::io::Result<ResolvedCommand> {
    resolve_parts(&command.executable, &command.args, command.cwd.as_deref(), environment)
}

fn resolve_parts(file: &std::ffi::OsStr, args: &[OsString], cwd: Option<&Path>, environment: &BTreeMap<OsString, OsString>) -> std::io::Result<ResolvedCommand> {
    let paths = node_path::Context::current()?;
    let cwd = cwd.unwrap_or(Path::new("."));
    let cwd = node_path::to_native(&paths.resolve(&[node_path::from_native(cwd)]));
    if !cwd.is_dir() { return Err(std::io::Error::from(std::io::ErrorKind::NotFound)) }
    let executable = resolve_executable(file, &cwd, environment)?;
    Ok(ResolvedCommand { executable, cwd, args: args.to_vec() })
}

#[cfg(test)]
#[path = "../../../../tests/rust/host-exec-file-deadline.rs"]
mod deadline_cases;

#[cfg(unix)]
fn resolve_executable(file: &std::ffi::OsStr, cwd: &Path, environment: &BTreeMap<OsString, OsString>) -> std::io::Result<PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    if file.as_bytes().contains(&b'/') { return Ok(cwd.join(file)) }
    let path = environment.get(std::ffi::OsStr::new("PATH")).cloned().unwrap_or_else(|| "/usr/bin:/bin".into());
    let mut denied = false;
    for directory in std::env::split_paths(&path) {
        let candidate = cwd.join(directory).join(file);
        use std::os::unix::fs::PermissionsExt;
        if let Ok(metadata) = std::fs::metadata(&candidate) {
            if metadata.is_file() && metadata.permissions().mode() & 0o111 != 0 { return Ok(candidate) }
            denied = true;
        }
    }
    Err(std::io::Error::from(if denied { std::io::ErrorKind::PermissionDenied } else { std::io::ErrorKind::NotFound }))
}

#[cfg(windows)]
fn resolve_executable(
    file: &std::ffi::OsStr,
    cwd: &Path,
    environment: &BTreeMap<OsString, OsString>,
) -> std::io::Result<PathBuf> {
    use std::{ffi::OsStr, io, os::windows::ffi::OsStrExt};

    let bytes = file.as_encoded_bytes();
    if file.is_empty() || file == "." || bytes.contains(&0) {
        return Err(io::ErrorKind::NotFound.into());
    }
    let end = bytes
        .iter()
        .rposition(|byte| !matches!(*byte, b' ' | b'.'))
        .map_or(0, |position| position + 1);
    if [b".cmd", b".bat"].iter().any(|suffix| {
        bytes[..end]
            .get(end.saturating_sub(suffix.len())..)
            .is_some_and(|value| value.eq_ignore_ascii_case(*suffix))
    }) {
        return Err(io::ErrorKind::InvalidInput.into());
    }

    let name = Path::new(file)
        .file_name()
        .unwrap_or_default()
        .as_encoded_bytes();
    let exact = name
        .iter()
        .position(|byte| *byte == b'.')
        .is_some_and(|position| position + 1 < name.len());
    let candidate = |path: PathBuf| -> Option<PathBuf> {
        let path = std::path::absolute(path).ok()?;
        if exact && path.is_file() {
            return Some(path);
        }
        for extension in ["com", "exe"] {
            let mut name = path.as_os_str().to_os_string();
            if !name.as_encoded_bytes().ends_with(b".") {
                name.push(".");
            }
            name.push(extension);
            let path = PathBuf::from(name);
            if path.is_file() {
                return Some(path);
            }
        }
        None
    };
    if bytes
        .iter()
        .any(|byte| matches!(*byte, b'/' | b'\\' | b':'))
    {
        return candidate(cwd.join(file)).ok_or_else(|| io::ErrorKind::NotFound.into());
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn NeedCurrentDirectoryForExePathW(name: *const u16) -> i32;
    }
    let filename: Vec<u16> = file.encode_wide().chain([0]).collect();
    // SAFETY: the checked filename is NUL-terminated and lives through the call.
    // The executing account's Windows policy controls current-directory search.
    if unsafe { NeedCurrentDirectoryForExePathW(filename.as_ptr()) } != 0 {
        if let Some(found) = candidate(cwd.join(file)) {
            return Ok(found);
        }
    }
    let path = environment
        .iter()
        .find(|(key, _)| key.as_encoded_bytes().eq_ignore_ascii_case(b"PATH"))
        .map_or(OsStr::new(""), |(_, value)| value.as_os_str());
    for directory in std::env::split_paths(path).filter(|path| !path.as_os_str().is_empty()) {
        if let Some(found) = candidate(cwd.join(directory).join(file)) {
            return Ok(found);
        }
    }
    Err(io::ErrorKind::NotFound.into())
}
