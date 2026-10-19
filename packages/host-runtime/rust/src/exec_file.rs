//! Bounded UTF-8 execFile behavior for host preflight and source tools. Artifact
//! command restrictions remain in process::spawn; both share the same tree owner.
use crate::{IvyError, IvyResult, node_path, process::{self, LaunchContext, OutputStream, ResolvedCommand}, process_output::Utf8Decoder};
use ivy_contracts::JsString;
use std::{collections::BTreeMap, ffi::OsString, path::{Path, PathBuf}, sync::{Arc,Mutex}, time::Duration};
use tokio::{io::{AsyncRead, AsyncReadExt}, sync::mpsc};
use tokio_util::sync::CancellationToken;

pub struct FileCommand {
    pub executable: OsString,
    pub args: Vec<OsString>,
    pub cwd: Option<PathBuf>,
    pub timeout_ms: u32,
    /// Each stream has its own limit, as in Node execFile.
    pub maximum_buffer_bytes: usize,
}
#[derive(Default)]
pub struct FileOptions {
    /// None inherits the complete executing process environment.
    pub environment: Option<BTreeMap<OsString, OsString>>,
    pub job_launcher: Option<PathBuf>,
    pub cancellation: CancellationToken,
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

pub(crate) async fn exec_file_detailed(command: FileCommand, bootstrap_root: PathBuf, options: FileOptions) -> IvyResult<FileExecution> {
    if command.timeout_ms == 0 || command.timeout_ms > 3_600_000 || command.maximum_buffer_bytes > 64 * 1024 * 1024 {
        return Err(IvyError::new("invalid_arguments", "Exec-file requires bounded time and output limits."));
    }
    tokio::spawn(run(command, bootstrap_root, options, false)).await.map_err(|_| IvyError::internal())
}

/// Source's original spawnSync closes stdin and counts raw bytes across both
/// streams. Keep that private policy separate from public execFile semantics.
pub(crate) async fn source_output(command: FileCommand, bootstrap_root: PathBuf) -> IvyResult<FileResult> {
    Ok(tokio::spawn(run(command, bootstrap_root, FileOptions::default(), true)).await.map_err(|_| IvyError::internal())?.result)
}

async fn collect<R: AsyncRead + Unpin>(mut pipe: R, maximum: usize, stream: OutputStream, overflow: mpsc::Sender<OutputStream>, stop_capture: CancellationToken, text: Arc<Mutex<Vec<u16>>>, combined: Option<Arc<std::sync::atomic::AtomicUsize>>) -> std::io::Result<()> {
    let mut decoder = Utf8Decoder::preserving_bom();
    let mut length = 0;
    let mut bytes = vec![0u8; 65536];
    loop {
        let read = pipe.read(&mut bytes).await?;
        let chunk = decoder.decode(&bytes[..read], read == 0);
        if !stop_capture.is_cancelled() {
            let mut text=text.lock().unwrap_or_else(|p|p.into_inner());
            if let Some(total) = &combined {
                if total.fetch_add(read, std::sync::atomic::Ordering::Relaxed).saturating_add(read) > maximum {
                    stop_capture.cancel(); let _ = overflow.try_send(stream);
                    continue;
                }
            }
            let chunk_bytes = if combined.is_some() { 0 } else { String::from_utf16_lossy(&chunk).len() };
            if length + chunk_bytes > maximum {
                // execFile counts encoded bytes but slices the overflowing
                // decoded chunk by UTF-16 units, including a split surrogate.
                text.extend_from_slice(&chunk[..chunk.len().min(maximum - length)]);
                stop_capture.cancel(); let _ = overflow.try_send(stream);
            } else { length += chunk_bytes; text.extend(chunk); }
        }
        if read == 0 { break; }
    }
    Ok(())
}

async fn run(command: FileCommand, bootstrap_root: PathBuf, options: FileOptions, synchronous_input: bool) -> FileExecution {
    if options.cancellation.is_cancelled() { return empty(FileFailure::Cancelled).into() }
    let environment = options.environment.unwrap_or_else(|| std::env::vars_os().collect());
    let (command,environment,resolved) = match tokio::task::spawn_blocking(move || {
        let resolved=resolve(&command,&environment);(command,environment,resolved)
    }).await {Ok(value)=>value,Err(_)=>return empty(FileFailure::Unknown).into()};
    let resolved = match resolved { Ok(value) => value, Err(error) => return FileExecution { result: empty(FileFailure::Spawn), spawn_error: Some(error) } };
    if options.cancellation.is_cancelled() { return empty(FileFailure::Cancelled).into() }
    let launch = LaunchContext { raw_output: true, job_launcher: options.job_launcher, ..Default::default() };
    let deadline = tokio::time::Instant::now() + Duration::from_millis(command.timeout_ms as u64);
    let mut child = match process::spawn_resolved_detailed(resolved, &bootstrap_root, launch, Some(environment), Some(command.executable.clone())).await {
        Ok(value) => value, Err(error) => return FileExecution { result: empty(if error.error.outcome == ivy_contracts::Outcome::Unknown { FileFailure::Unknown } else { FileFailure::Spawn }), spawn_error: error.source },
    };
    // execFile leaves stdin open; spawnSync's absent input is immediate EOF.
    if synchronous_input { drop(child.stdin.take()); }
    let (overflow, mut exceeded) = mpsc::channel(2);
    let stop_capture = CancellationToken::new();
    let out=Arc::new(Mutex::new(Vec::new()));let err=Arc::new(Mutex::new(Vec::new()));
    let combined = synchronous_input.then(|| Arc::new(std::sync::atomic::AtomicUsize::new(0)));
    let mut stdout = tokio::spawn(collect(child.stdout.take().unwrap(), command.maximum_buffer_bytes, OutputStream::Stdout, overflow.clone(), stop_capture.clone(),out.clone(),combined.clone()));
    let mut stderr = tokio::spawn(collect(child.stderr.take().unwrap(), command.maximum_buffer_bytes, OutputStream::Stderr, overflow.clone(), stop_capture.clone(),err.clone(),combined));
    let completion = child.completion.clone();
    let mut failure = tokio::select! {
        biased;
        stream = exceeded.recv() => stream.map(FileFailure::MaxBuffer),
        _ = options.cancellation.cancelled() => Some(FileFailure::Cancelled),
        _ = tokio::time::sleep_until(deadline) => Some(FileFailure::Timeout),
        _ = completion.wait() => None,
    };
    drop(overflow);
    let result = if failure.is_some() { stop_capture.cancel(); child.stop(Duration::ZERO).await.ok() } else { Some(completion.wait().await) };
    let drain = async { ((&mut stdout).await, (&mut stderr).await) };
    let captured = tokio::time::timeout(Duration::from_secs(2), drain).await;
    if !matches!(captured,Ok((Ok(Ok(())),Ok(Ok(()))))) {
        stdout.abort();stderr.abort();failure=Some(FileFailure::Unknown);
    }
    let stdout=JsString(std::mem::take(&mut *out.lock().unwrap_or_else(|p|p.into_inner())));
    let stderr=JsString(std::mem::take(&mut *err.lock().unwrap_or_else(|p|p.into_inner())));
    if failure.is_none() { failure = exceeded.try_recv().ok().map(FileFailure::MaxBuffer); }
    let Some(result) = result else { return FileResult { stdout, stderr, exit_code: None, signal: None, failure: Some(FileFailure::Unknown) }.into() };
    if result.error_code.is_some() { failure = Some(FileFailure::Unknown); }
    else if failure.is_none() && (result.exit_code != Some(0) || result.signal.is_some()) { failure = Some(FileFailure::Exit); }
    FileResult { stdout, stderr, exit_code: result.exit_code, signal: result.signal.clone(), failure }.into()
}

fn resolve(command: &FileCommand, environment: &BTreeMap<OsString, OsString>) -> std::io::Result<ResolvedCommand> {
    let paths = node_path::Context::current()?;
    let cwd = command.cwd.as_deref().unwrap_or(Path::new("."));
    let cwd = node_path::to_native(&paths.resolve(&[node_path::from_native(cwd)]));
    if !cwd.is_dir() { return Err(std::io::Error::from(std::io::ErrorKind::NotFound)) }
    let executable = resolve_executable(&command.executable, &cwd, environment)?;
    Ok(ResolvedCommand { executable, cwd, args: command.args.clone() })
}

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
fn resolve_executable(file: &std::ffi::OsStr, cwd: &Path, environment: &BTreeMap<OsString, OsString>) -> std::io::Result<PathBuf> {
    use std::os::windows::ffi::OsStrExt;
    let file: Vec<u16> = file.encode_wide().collect();
    if file.is_empty() || file == [46] || file.contains(&0) { return Err(std::io::Error::from(std::io::ErrorKind::NotFound)) }
    let lower=String::from_utf16_lossy(&file).trim_end_matches([' ','.']).to_ascii_lowercase();
    if lower.ends_with(".cmd") || lower.ends_with(".bat") { return Err(std::io::Error::from(std::io::ErrorKind::InvalidInput)) }
    let separator = |c: &u16| matches!(*c, 47 | 92 | 58);
    let start = file.iter().rposition(separator).map_or(0, |i|i+1);
    let name = &file[start..];
    let extension = name.iter().position(|c|*c==46).is_some_and(|i|i+1<name.len());
    let context = node_path::Context { cwd: node_path::from_native(cwd), ..node_path::Context::current()? };
    let candidate = |directory: &[u16], exact: bool| -> Option<PathBuf> {
        let mut parts = directory.to_vec();
        if !parts.is_empty() && !parts.last().is_some_and(separator) { parts.push(92); }
        parts.extend_from_slice(name);
        let test = |units: Vec<u16>| { let path = node_path::to_native(&context.resolve(&[JsString(units)])); path.is_file().then_some(path) };
        if exact { if let Some(found) = test(parts.clone()) { return Some(found) } }
        if parts.last() != Some(&46) { parts.push(46); }
        for extension in ["com", "exe"] { let mut value = parts.clone();value.extend(extension.encode_utf16());if let Some(found) = test(value) { return Some(found) } }
        None
    };
    if start != 0 { return candidate(&file[..start], extension).ok_or_else(|| std::io::Error::from(std::io::ErrorKind::NotFound)) }
    // This API observes the executing process's NoDefaultCurrentDirectory...,
    // as does libuv's NeedCurrentDirectoryForExePathW, not a child-env override.
    #[link(name="kernel32")] unsafe extern "system" { fn NeedCurrentDirectoryForExePathW(name: *const u16) -> i32; }
    if unsafe { NeedCurrentDirectoryForExePathW([0u16].as_ptr()) } != 0 { if let Some(found)=candidate(&[],extension) { return Ok(found) } }
    let path = environment.iter().find(|(key,_)|key.to_string_lossy().eq_ignore_ascii_case("PATH")).map(|(_,value)|value.encode_wide().collect::<Vec<_>>()).unwrap_or_default();
    let mut start=0;
    while start < path.len() {
        if path[start]==59 { start+=1;continue; }
        let mut end=start;
        if matches!(path[start],34|39) { end=start+1;while end<path.len() && path[end]!=path[start] { end+=1; } }
        while end<path.len() && path[end]!=59 { end+=1; }
        let mut directory=&path[start..end];
        if directory.first().is_some_and(|c|matches!(*c,34|39)) { directory=&directory[1..]; }
        if directory.last().is_some_and(|c|matches!(*c,34|39)) { directory=&directory[..directory.len()-1]; }
        if !directory.is_empty() { if let Some(found)=candidate(directory,extension) { return Ok(found) } }
        start=end.saturating_add(1);
    }
    Err(std::io::Error::from(std::io::ErrorKind::NotFound))
}
