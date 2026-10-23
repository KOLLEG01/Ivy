//! Bounded native text reads with containment and opened-file identity checks.
use crate::native_path::{self, Context};
use ivy_contracts::JsString;
#[cfg(test)]
use std::sync::Arc;
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read},
    path::Path,
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
fn native(error: io::Error) -> TextFileError {
    TextFileError {
        name: "Error",
        message: error.to_string().into(),
        code: super::node_error_code(&error),
        source: Some(error),
    }
}
fn optional<T>(result: Result<T, TextFileError>) -> Result<Option<T>, TextFileError> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(error)
            if error.source.as_ref().is_some_and(|source| {
                matches!(
                    source.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
                )
            }) =>
        {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}
fn physical(path: &JsString, context: &Context) -> Result<std::path::PathBuf, TextFileError> {
    if path.0.contains(&0) {
        return Err(TextFileError {
            name: "TypeError",
            message: "Filesystem path contains NUL.".into(),
            code: Some("ERR_INVALID_ARG_VALUE"),
            source: Some(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Filesystem path contains NUL.",
            )),
        });
    }
    Ok(context.filesystem_path(path))
}

/// Missing paths and non-directory ancestors return None. Other errors retain
/// the operating system's diagnostic and cause.
pub async fn realpath(path: JsString) -> Result<Option<JsString>, TextFileError> {
    let context = Context::current().map_err(native)?;
    tokio::task::spawn_blocking(move || {
        optional(
            fs::canonicalize(physical(&path, &context)?)
                .map(|p| native_path::from_native(&p))
                .map_err(native),
        )
    })
    .await
    .map_err(|error| native(io::Error::other(error)))?
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
            callback(stage).map_err(native)?;
        }
        let _ = stage;
        Ok(())
    }
}

/// Read a regular file beneath an already verified canonical directory. A single
/// blocking operation owns the handle through validation and the bounded read.
pub async fn read_regular(
    verified_directory: JsString,
    path: JsString,
    maximum_bytes: usize,
) -> Result<Option<JsString>, TextFileError> {
    let context = Context::current().map_err(native)?;
    tokio::task::spawn_blocking(move || {
        optional(read(
            &context,
            &verified_directory,
            &path,
            maximum_bytes,
            Stages::default(),
        ))
    })
    .await
    .map_err(|error| native(io::Error::other(error)))?
}
fn read(
    context: &Context,
    directory: &JsString,
    path: &JsString,
    maximum_bytes: usize,
    stages: Stages,
) -> Result<JsString, TextFileError> {
    let physical = physical(path, context)?;
    let before = fs::canonicalize(&physical).map_err(native)?;
    if !inside(context, directory, &before) {
        return Err(invalid(
            "Workflow transcript file is outside the verified transcript directory",
        ));
    }
    stages.at("before-open")?;
    let file = open(&physical).map_err(native)?;
    let opened = observation(&file).map_err(native)?;
    if !opened.regular || opened.symlink {
        return Err(invalid("Workflow transcript entry is not a regular file"));
    }
    if opened.bytes > maximum_bytes as u64 {
        return Err(invalid(
            "Workflow transcript entry exceeds the configured size limit",
        ));
    }
    stages.at("after-stat")?;
    let after = fs::canonicalize(&physical).map_err(native)?;
    let current = lstat(&physical).map_err(native)?;
    if after != before
        || !inside(context, directory, &after)
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
    let mut bytes = Vec::new();
    file.take((maximum_bytes as u64).saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(native)?;
    if bytes.len() > maximum_bytes {
        return Err(invalid(
            "Workflow transcript entry exceeds the configured size limit",
        ));
    }
    Ok(String::from_utf8_lossy(&bytes).as_ref().into())
}
fn inside(context: &Context, directory: &JsString, path: &Path) -> bool {
    let child = context.relative(directory, &native_path::from_native(path));
    !child.0.is_empty()
        && child.0 != [46, 46]
        && !child
            .0
            .starts_with(&[46, 46, if cfg!(windows) { 92 } else { 47 }])
        && !native_path::is_absolute(context.style, &child)
}
fn open(path: &Path) -> io::Result<File> {
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
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
        };
        options.custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT);
    }
    options.open(path)
}
struct Observation {
    regular: bool,
    symlink: bool,
    bytes: u64,
    dev: u64,
    ino: u64,
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
            dev: metadata.dev(),
            ino: metadata.ino(),
        })
    }
    #[cfg(windows)]
    {
        let info = super::handle_information(file)?;
        Ok(Observation {
            regular: metadata.is_file(),
            symlink: metadata.file_type().is_symlink(),
            bytes: metadata.len(),
            dev: u64::from(info.dwVolumeSerialNumber),
            ino: (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
        })
    }
}
fn lstat(path: &Path) -> io::Result<Observation> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let metadata = fs::symlink_metadata(path)?;
        Ok(Observation {
            regular: metadata.is_file(),
            symlink: metadata.file_type().is_symlink(),
            bytes: metadata.len(),
            dev: metadata.dev(),
            ino: metadata.ino(),
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

#[cfg(test)]
#[path = "../../../../../tests/rust/host-regular-text-cases.rs"]
mod tests;
