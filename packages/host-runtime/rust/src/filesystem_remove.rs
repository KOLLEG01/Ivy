//! Native `rmSync(path, { recursive: true, force: true })`, with zero retries.
//! The private cause belongs to this operation, not libuv unlink/rmdir.
use crate::{
    filesystem_error::NodeFilesystemError,
    node_path::{self, Context, Style},
};
use ivy_contracts::JsString;
use std::{fs, io, path::Path};

#[derive(Debug)]
pub struct RemoveFailure {
    pub source: io::Error,
    /// None means that no exact Node cause is asserted for the native failure.
    pub node: Option<NodeFilesystemError>,
}
impl std::fmt::Display for RemoveFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.source.fmt(f)
    }
}
impl std::error::Error for RemoveFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}
fn unmapped(source: io::Error) -> RemoveFailure {
    RemoveFailure { source, node: None }
}

/// Synchronous, string-path, default-retry removal. The caller owns admission
/// of the root. Buffer/URL paths, options validation and the Node permission
/// model are not implemented by this private native primitive.
pub fn remove_sync_force(path: &JsString) -> Result<(), RemoveFailure> {
    let context = Context::current().map_err(unmapped)?;
    remove_sync_force_at(path, &context)
}

/// Capture the native cwd/drive context before removal; it also determines the
/// root reported in an rm failure. Completed child deletions are not rolled back.
pub fn remove_sync_force_at(path: &JsString, context: &Context) -> Result<(), RemoveFailure> {
    if context.style != Style::native() || path.0.contains(&0) {
        return Err(unmapped(io::ErrorKind::InvalidInput.into()));
    }
    let physical = context.filesystem_path(path);
    match remove_all(&physical) {
        Ok(()) => Ok(()),
        Err(error) if no_such_file(&error) => Ok(()),
        Err(source) => {
            let node = cause(&source, &physical);
            Err(RemoveFailure { source, node })
        }
    }
}

fn cause(error: &io::Error, root: &Path) -> Option<NodeFilesystemError> {
    // MSVC path::string uses the process ANSI/OEM code page. Node's native
    // error path can throw before producing any JS cause for non-ASCII roots.
    // Removal still uses the full Node filesystem spelling; do not fabricate
    // a Unicode cause from an unavailable or lossy native conversion.
    #[cfg(windows)]
    if !root.to_str().is_some_and(|value| value.is_ascii()) {
        return None;
    }
    let raw = error.raw_os_error()?;
    #[cfg(windows)]
    let (code, errno, description) = match raw {
        // MSVC system_category -> errc::permission_denied, followed by Node's
        // Windows EPERM branch. This is deliberately not libuv uv_translate.
        5 | 12 | 16 | 19 | 32 | 82 | 998 | 10013 => ("EPERM", 1, "Permission denied"),
        145 => ("ENOTEMPTY", 41, "Directory not empty"),
        _ => return None,
    };
    #[cfg(unix)]
    let (code, errno, description) = match raw {
        libc::EPERM => ("EPERM", raw, "Operation not permitted"),
        libc::EACCES => ("EACCES", raw, "Permission denied"),
        libc::ENOTEMPTY => ("ENOTEMPTY", raw, "Directory not empty"),
        libc::ENOTDIR => ("ENOTDIR", raw, "Not a directory"),
        _ => return None,
    };
    #[cfg(not(any(windows, unix)))]
    return None;
    #[cfg(any(windows, unix))]
    {
        let path = node_path::from_native(root);
        let text = String::from_utf16_lossy(&path.0);
        let message = format!("{code}, {description}: {text} '{text}'").into();
        // ErrnoException keeps the namespace and uses a comma, unlike UVException.
        Some(NodeFilesystemError {
            code,
            errno,
            syscall: "rm",
            path,
            message,
        })
    }
}

#[cfg(windows)]
fn no_such_file(error: &io::Error) -> bool {
    matches!(error.raw_os_error(), Some(2 | 3 | 53 | 67 | 123))
}
#[cfg(not(windows))]
fn no_such_file(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::NotFound
}

#[cfg(windows)]
fn remove_all(path: &Path) -> io::Result<()> {
    match remove_one(path) {
        Err(error) if error.raw_os_error() == Some(145) => remove_directory(path),
        other => other,
    }
}

#[cfg(windows)]
fn remove_directory(path: &Path) -> io::Result<()> {
    let entries = match fs::read_dir(path) {
        Ok(entries) => Some(entries),
        Err(error) if missing_windows(&error) => None,
        Err(error) => return Err(error),
    };
    if let Some(entries) = entries {
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) if missing_windows(&error) => break,
                Err(error) => return Err(error),
            };
            if entry.file_type()?.is_dir() {
                remove_directory(&entry.path())?;
            } else {
                remove_one(&entry.path())?;
            }
        }
    }
    // MSVC remove_all retries directory retirement ten times without sleeping,
    // separately from Node's user-selected (here zero) operation retries.
    for attempt in 0..10 {
        match remove_one(path) {
            Err(error) if attempt < 9 && matches!(error.raw_os_error(), Some(5 | 145)) => (),
            result => return result,
        }
    }
    unreachable!()
}

#[cfg(windows)]
fn missing_windows(error: &io::Error) -> bool {
    matches!(
        error.raw_os_error(),
        Some(2 | 3 | 53 | 64 | 123 | 267)
    )
}

#[cfg(windows)]
fn remove_one(path: &Path) -> io::Result<()> {
    use std::os::windows::{fs::OpenOptionsExt, io::AsRawHandle};
    use windows_sys::Win32::Storage::FileSystem::{
        DELETE, FILE_ATTRIBUTE_READONLY, FILE_BASIC_INFO, FILE_DISPOSITION_FLAG_DELETE,
        FILE_DISPOSITION_FLAG_IGNORE_READONLY_ATTRIBUTE, FILE_DISPOSITION_FLAG_POSIX_SEMANTICS,
        FILE_DISPOSITION_INFO, FILE_DISPOSITION_INFO_EX, FILE_FLAG_BACKUP_SEMANTICS,
        FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ,
        FILE_SHARE_WRITE, FILE_WRITE_ATTRIBUTES, FileBasicInfo, FileDispositionInfo,
        FileDispositionInfoEx, GetFileInformationByHandleEx, SetFileInformationByHandle,
    };
    let open = |access| {
        fs::OpenOptions::new()
            .access_mode(access)
            .share_mode(FILE_SHARE_DELETE | FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)
    };
    let (file, attributes) = match open(DELETE | FILE_READ_ATTRIBUTES | FILE_WRITE_ATTRIBUTES) {
        Ok(file) => (file, true),
        Err(error) if error.raw_os_error() == Some(5) => (open(DELETE)?, false),
        Err(error) if missing_windows(&error) => return Ok(()),
        Err(error) => return Err(error),
    };
    let handle = file.as_raw_handle();
    let ex = |flags| -> io::Result<()> {
        let info = FILE_DISPOSITION_INFO_EX { Flags: flags };
        if unsafe {
            SetFileInformationByHandle(
                handle,
                FileDispositionInfoEx,
                &info as *const _ as *const _,
                std::mem::size_of_val(&info) as u32,
            )
        } != 0
        {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    };
    let unsupported = |error: &io::Error| matches!(error.raw_os_error(), Some(1 | 50 | 87));
    match ex(FILE_DISPOSITION_FLAG_DELETE
        | FILE_DISPOSITION_FLAG_POSIX_SEMANTICS
        | FILE_DISPOSITION_FLAG_IGNORE_READONLY_ATTRIBUTE)
    {
        Ok(()) => return Ok(()),
        Err(error) if unsupported(&error) => (),
        Err(error) => return Err(error),
    }
    let delete = || -> io::Result<()> {
        match ex(FILE_DISPOSITION_FLAG_DELETE | FILE_DISPOSITION_FLAG_POSIX_SEMANTICS) {
            Ok(()) => Ok(()),
            Err(error) if unsupported(&error) => {
                let info = FILE_DISPOSITION_INFO { DeleteFile: true };
                if unsafe {
                    SetFileInformationByHandle(
                        handle,
                        FileDispositionInfo,
                        &info as *const _ as *const _,
                        std::mem::size_of_val(&info) as u32,
                    )
                } != 0
                {
                    Ok(())
                } else {
                    Err(io::Error::last_os_error())
                }
            }
            Err(error) => Err(error),
        }
    };
    let error = match delete() {
        Ok(()) => return Ok(()),
        Err(error) => error,
    };
    if error.raw_os_error() != Some(5) || !attributes {
        return Err(error);
    }
    let mut basic: FILE_BASIC_INFO = unsafe { std::mem::zeroed() };
    if unsafe {
        GetFileInformationByHandleEx(
            handle,
            FileBasicInfo,
            &mut basic as *mut _ as *mut _,
            std::mem::size_of_val(&basic) as u32,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    if basic.FileAttributes & FILE_ATTRIBUTE_READONLY == 0 {
        return Err(error);
    }
    let set = |basic: &FILE_BASIC_INFO| -> io::Result<()> {
        if unsafe {
            SetFileInformationByHandle(
                handle,
                FileBasicInfo,
                basic as *const _ as *const _,
                std::mem::size_of_val(basic) as u32,
            )
        } != 0
        {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    };
    basic.FileAttributes &= !FILE_ATTRIBUTE_READONLY;
    set(&basic)?;
    match delete() {
        Err(error) if error.raw_os_error() == Some(5) => {
            basic.FileAttributes |= FILE_ATTRIBUTE_READONLY;
            set(&basic)?;
            Err(error)
        }
        result => result,
    }
}

#[cfg(not(windows))]
fn remove_all(path: &Path) -> io::Result<()> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if no_such_file(&error) => return Ok(()),
        #[cfg(unix)]
        Err(error) if error.raw_os_error() == Some(libc::ENOTDIR) => return Ok(()),
        Err(error) => return Err(error),
    };
    if metadata.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
}
