//! Node string-path text reads with an opened-handle identity fence.
use crate::{
    filesystem_error,
    node_path::{self, Context},
    paths,
};
use ivy_contracts::JsString;
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read},
    path::PathBuf,
    sync::Arc,
};

#[derive(Debug)]
pub struct TextFileError {
    pub name: &'static str,
    pub message: JsString,
    pub code: Option<&'static str>,
    source: Option<io::Error>,
}
impl std::fmt::Display for TextFileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&String::from_utf16_lossy(&self.message.0))
    }
}
impl std::error::Error for TextFileError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source.as_ref().map(|e| e as &dyn std::error::Error)
    }
}
fn invalid(message: &'static str) -> TextFileError {
    TextFileError {
        name: "Error",
        message: message.into(),
        code: None,
        source: None,
    }
}
fn worker(error: impl std::fmt::Display) -> TextFileError {
    TextFileError {
        name: "Error",
        message: error.to_string().into(),
        code: None,
        source: None,
    }
}
fn native(
    error: io::Error,
    syscall: &'static str,
    path: Option<&JsString>,
    context: &Context,
) -> TextFileError {
    let formatted = filesystem_error::operation_message(&error, syscall, path, context);
    TextFileError {
        name: "Error",
        message: formatted
            .as_ref()
            .map(|(_, message)| message.clone())
            .unwrap_or_else(|| error.to_string().into()),
        code: formatted.map(|(code, _)| code),
        source: Some(error),
    }
}
fn optional<T>(result: Result<T, TextFileError>) -> Result<Option<T>, TextFileError> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(error) if matches!(error.code, Some("ENOENT" | "ENOTDIR")) => Ok(None),
        Err(error) => Err(error),
    }
}
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, TextFileError> + Send + 'static,
) -> Result<T, TextFileError> {
    tokio::task::spawn_blocking(work).await.map_err(worker)?
}
fn physical(path: &JsString, context: &Context) -> Result<PathBuf, TextFileError> {
    if path.0.contains(&0) {
        return Err(null_path(path));
    }
    Ok(context.filesystem_path(path))
}
fn context() -> Result<Arc<Context>, TextFileError> {
    Context::current().map(Arc::new).map_err(worker)
}
async fn canonical(path: JsString, context: Arc<Context>) -> Result<JsString, TextFileError> {
    blocking(move || {
        let physical = physical(&path, &context)?;
        fs::canonicalize(physical)
            .map(paths::node_filesystem_result)
            .map(|p| node_path::from_native(&p))
            .map_err(|error| native(error, "realpath", Some(&path), &context))
    })
    .await
}

/// Node-spelled realpath. Only ENOENT/ENOTDIR become None; other failures retain
/// their diagnostics and native cause. No filesystem or account fallback.
pub async fn realpath(path: JsString) -> Result<Option<JsString>, TextFileError> {
    optional(canonical(path, context()?).await)
}

#[derive(Default)]
struct Stages {
    #[cfg(test)]
    callback: Option<Arc<dyn Fn(&str) -> io::Result<()> + Send + Sync>>,
}
impl Stages {
    fn at(&self, stage: &str) -> Result<(), TextFileError> {
        #[cfg(test)]
        if let Some(callback) = &self.callback {
            callback(stage).map_err(worker)?;
        }
        let _ = stage;
        Ok(())
    }
}

/// Read through the opened file after canonical-before, regular-file/size and
/// concurrent canonical-after/lstat identity checks. The supplied directory is
/// already verified by the caller. The original bound applies at opened stat;
/// this does not invent a second bound or lock against later content writes.
pub async fn read_regular(
    verified_directory: JsString,
    path: JsString,
    maximum_bytes: usize,
) -> Result<Option<JsString>, TextFileError> {
    tokio::spawn(read(
        verified_directory,
        path,
        maximum_bytes,
        Stages::default(),
    ))
    .await
    .map_err(worker)?
}
async fn read(
    directory: JsString,
    path: JsString,
    maximum_bytes: usize,
    stages: Stages,
) -> Result<Option<JsString>, TextFileError> {
    let context = context()?;
    let Some(before) = optional(canonical(path.clone(), context.clone()).await)? else {
        return Ok(None);
    };
    if !inside(&context, &directory, &before) {
        return Err(invalid(
            "Workflow transcript file is outside the verified transcript directory",
        ));
    }
    stages.at("before-open")?;
    let selected = path.clone();
    let ctx = context.clone();
    let Some(file) = optional(
        blocking(move || {
            open(&physical(&selected, &ctx)?).map_err(|e| native(e, "open", Some(&selected), &ctx))
        })
        .await,
    )?
    else {
        return Ok(None);
    };
    let file = Arc::new(file);
    let result = async {
        let handle = file.clone();
        let ctx = context.clone();
        let opened =
            blocking(move || observation(&handle).map_err(|e| native(e, "fstat", None, &ctx)))
                .await?;
        if !opened.regular {
            return Err(invalid("Workflow transcript entry is not a regular file"));
        }
        if opened.bytes > maximum_bytes as u64 {
            return Err(invalid(
                "Workflow transcript entry exceeds the configured size limit",
            ));
        }
        stages.at("after-stat")?;
        let selected = path.clone();
        let ctx = context.clone();
        let after = canonical(path.clone(), context.clone());
        let current = blocking(move || {
            let physical = physical(&selected, &ctx)?;
            lstat(&physical).map_err(|e| native(e, "lstat", Some(&selected), &ctx))
        });
        let (after, current) = tokio::try_join!(after, current)?;
        if after != before
            || !inside(&context, &directory, &after)
            || !current.regular
            || current.symlink
            || current.dev != opened.dev
            || current.ino != opened.ino
        {
            return Err(invalid(
                "Workflow transcript entry changed during validation",
            ));
        }
        stages.at("before-read")?;
        let handle = file.clone();
        let ctx = context.clone();
        let bytes = blocking(move || {
            let mut bytes = Vec::new();
            let mut handle = &*handle;
            handle
                .read_to_end(&mut bytes)
                .map_err(|e| native(e, "read", None, &ctx))?;
            Ok(bytes)
        })
        .await?;
        Ok(JsString::from(String::from_utf8_lossy(&bytes).as_ref()))
    }
    .await;
    // Match finally: a close failure supersedes the result, including a missing
    // path observed after open. Dropping an observer leaves this owned read alive.
    let file =
        Arc::try_unwrap(file).map_err(|_| invalid("Filesystem read still owns its file handle"))?;
    let ctx = context.clone();
    blocking(move || close(file).map_err(|e| native(e, "close", None, &ctx))).await?;
    optional(result)
}
fn inside(context: &Context, directory: &JsString, path: &JsString) -> bool {
    let child = context.relative(directory, path);
    !child.0.is_empty()
        && child.0 != [46, 46]
        && !child
            .0
            .starts_with(&[46, 46, if cfg!(windows) { 92 } else { 47 }])
        && !node_path::is_absolute(context.style, &child)
}
fn open(path: &PathBuf) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // Node's O_NOFOLLOW is undefined on Windows (bitwise flags become zero).
        // Open normally, with libuv's directory/backup semantics; lstat below
        // refuses a link and compares the actual opened handle's identity.
        options.custom_flags(windows_sys::Win32::Storage::FileSystem::FILE_FLAG_BACKUP_SEMANTICS);
    }
    options.open(path)
}
struct Observation {
    regular: bool,
    symlink: bool,
    bytes: u64,
    dev: f64,
    ino: f64,
}
fn observation(file: &File) -> io::Result<Observation> {
    let metadata = file.metadata()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Ok(Observation {
            regular: metadata.is_file(),
            symlink: metadata.file_type().is_symlink(),
            bytes: metadata.len(),
            dev: metadata.dev() as f64,
            ino: metadata.ino() as f64,
        })
    }
    #[cfg(windows)]
    {
        let info = super::handle_information(file)?;
        Ok(Observation {
            regular: metadata.is_file(),
            symlink: metadata.file_type().is_symlink(),
            bytes: metadata.len(),
            dev: info.dwVolumeSerialNumber as f64,
            ino: (((info.nFileIndexHigh as u64) << 32) | info.nFileIndexLow as u64) as f64,
        })
    }
}
fn lstat(path: &PathBuf) -> io::Result<Observation> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let metadata = fs::symlink_metadata(path)?;
        Ok(Observation {
            regular: metadata.is_file(),
            symlink: metadata.file_type().is_symlink(),
            bytes: metadata.len(),
            dev: metadata.dev() as f64,
            ino: metadata.ino() as f64,
        })
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES,
            FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
        };
        let file = OpenOptions::new()
            .access_mode(FILE_READ_ATTRIBUTES)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .open(path)?;
        observation(&file)
    }
}
fn close(file: File) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::fd::IntoRawFd;
        if unsafe { libc::close(file.into_raw_fd()) } == 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if matches!(error.raw_os_error(), Some(libc::EINTR | libc::EINPROGRESS)) {
            Ok(())
        } else {
            Err(error)
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::IntoRawHandle;
        if unsafe { windows_sys::Win32::Foundation::CloseHandle(file.into_raw_handle()) } != 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
}

fn null_path(path: &JsString) -> TextFileError {
    let mut inspected = inspect_path(&path.0[..path.0.len().min(10_000)]);
    if inspected.len() > 128 {
        inspected.truncate(128);
        inspected.extend("...".encode_utf16());
    }
    let mut message: Vec<_> =
        "The argument 'path' must be a string, Uint8Array, or URL without null bytes. Received "
            .encode_utf16()
            .collect();
    message.extend(inspected);
    TextFileError {
        name: "TypeError",
        message: JsString(message),
        code: Some("ERR_INVALID_ARG_VALUE"),
        source: None,
    }
}
fn inspect_path(units: &[u16]) -> Vec<u16> {
    fn quoted(units: &[u16]) -> Vec<u16> {
        let quote = if units.contains(&39) && !units.contains(&34) {
            34
        } else if units.contains(&39)
            && units.contains(&34)
            && !units.contains(&96)
            && !units.windows(2).any(|v| v == [36, 123])
        {
            96
        } else {
            39
        };
        let mut output = vec![quote];
        let mut i = 0;
        while i < units.len() {
            let c = units[i];
            let escape = match c {
                8 => Some("\\b".to_owned()),
                9 => Some("\\t".to_owned()),
                10 => Some("\\n".to_owned()),
                12 => Some("\\f".to_owned()),
                13 => Some("\\r".to_owned()),
                92 => Some("\\\\".to_owned()),
                c if c == quote => Some(format!("\\{}", char::from_u32(c as u32).unwrap())),
                c if c < 32 || (127..160).contains(&c) => Some(format!("\\x{c:02X}")),
                c if (0xd800..=0xdfff).contains(&c) => {
                    if c <= 0xdbff
                        && units
                            .get(i + 1)
                            .is_some_and(|c| (0xdc00..=0xdfff).contains(c))
                    {
                        output.extend_from_slice(&units[i..i + 2]);
                        i += 2;
                        continue;
                    } else {
                        Some(format!("\\u{c:04x}"))
                    }
                }
                _ => None,
            };
            if let Some(escape) = escape {
                output.extend(escape.encode_utf16())
            } else {
                output.push(c)
            }
            i += 1;
        }
        output.push(quote);
        output
    }
    if units.len() > 76 {
        let mut output = Vec::new();
        let mut start = 0;
        for end in units
            .iter()
            .enumerate()
            .filter_map(|(i, c)| (*c == 10).then_some(i + 1))
            .chain(std::iter::once(units.len()))
            .filter(|end| *end > 0)
        {
            if end == start {
                continue;
            }
            if start > 0 {
                output.extend(" +\n  ".encode_utf16())
            }
            output.extend(quoted(&units[start..end]));
            start = end;
        }
        output
    } else {
        quoted(units)
    }
}

#[cfg(test)]
#[path = "../../../../../tests/rust/host-regular-text-cases.rs"]
mod tests;
