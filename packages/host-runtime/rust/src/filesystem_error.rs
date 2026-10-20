//! Private-cause formatting for Node 24 filesystem string-path failures.
//! This does not change IvyError, wire redaction, argument validation or I/O policy.
use crate::node_path::{self, Context, Style};
use ivy_contracts::JsString;
use std::{io, path::PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Syscall {
    Scandir,
    Stat,
    Rmdir,
    Unlink,
    Lstat,
}
impl Syscall {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Scandir => "scandir",
            Self::Stat => "stat",
            Self::Rmdir => "rmdir",
            Self::Unlink => "unlink",
            Self::Lstat => "lstat",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NodeFilesystemError {
    pub code: &'static str,
    /// Node/libuv errno, not the Windows GetLastError value.
    pub errno: i32,
    pub syscall: &'static str,
    pub path: JsString,
    pub message: JsString,
}

/// Format a native filesystem failure against the current Node path context.
/// Unknown/non-OS errors and NUL arguments return None; they have no asserted
/// Node I/O message. Capturing Context before I/O and using format_at avoids a
/// changed cwd/drive cwd between the operation and formatting on Windows.
pub fn format(
    error: &io::Error,
    syscall: Syscall,
    logical_path: &JsString,
) -> io::Result<Option<NodeFilesystemError>> {
    let Some(mapping) = mapping(error, syscall) else {
        return Ok(None);
    };
    if logical_path.0.contains(&0) {
        return Ok(None);
    }
    Ok(Some(render(
        mapping,
        syscall,
        node_path::node_filesystem_path(logical_path)?,
    )))
}

/// The supplied context must be the native context used for the failed I/O.
/// Only string-path Node filesystem errors are represented, without dest/fd.
pub fn format_at(
    error: &io::Error,
    syscall: Syscall,
    logical_path: &JsString,
    context: &Context,
) -> Option<NodeFilesystemError> {
    if context.style != Style::native() || logical_path.0.contains(&0) {
        return None;
    }
    Some(render(
        mapping(error, syscall)?,
        syscall,
        context.filesystem_path(logical_path),
    ))
}

fn render(
    (code, errno, description): (&'static str, i32, &'static str),
    syscall: Syscall,
    path: PathBuf,
) -> NodeFilesystemError {
    let path = path.to_string_lossy();
    #[cfg(windows)]
    let path = if let Some(rest) = path.strip_prefix("\\\\?\\UNC\\") {
        format!("\\\\{rest}")
    } else {
        path.strip_prefix("\\\\?\\").unwrap_or(&path).to_owned()
    };
    let message = format!("{code}: {description}, {} '{path}'", syscall.as_str());
    NodeFilesystemError {
        code,
        errno,
        syscall: syscall.as_str(),
        path: JsString(path.encode_utf16().collect()),
        message: message.into(),
    }
}

fn mapping(error: &io::Error, syscall: Syscall) -> Option<(&'static str, i32, &'static str)> {
    let raw = error.raw_os_error()?;
    // Deliberately bounded mappings: no ErrorKind/OS-localized text fallback.
    #[cfg(windows)]
    let (code, errno) = match raw {
        267 if syscall == Syscall::Scandir => ("ENOTDIR", -4052),
        2 | 3 | 15 | 123 | 126 | 161 | 203 | 267 | 4392 | 11001 | 11004 => ("ENOENT", -4058),
        5 | 1314 => ("EPERM", -4048),
        740 | 1920 | 10013 => ("EACCES", -4092),
        32 | 33 | 231 => ("EBUSY", -4082),
        145 => ("ENOTEMPTY", -4051),
        80 | 183 => ("EEXIST", -4075),
        13 | 87 | 122 | 1464 | 10022 => ("EINVAL", -4071),
        4 | 10024 => ("EMFILE", -4066),
        8 | 14 => ("ENOMEM", -4057),
        111 | 206 => ("ENAMETOOLONG", -4064),
        39 | 82 | 112 | 277 | 1100 => ("ENOSPC", -4055),
        19 => ("EROFS", -4043),
        1921 => ("ELOOP", -4067),
        1 => ("EISDIR", -4068),
        23 | 31 | 110 | 1117 => ("EIO", -4070),
        _ => return None,
    };
    #[cfg(unix)]
    let (code, errno) = {
        let _ = syscall;
        let code = match raw {
            libc::ENOENT => "ENOENT",
            libc::ENOTDIR => "ENOTDIR",
            libc::EACCES => "EACCES",
            libc::EPERM => "EPERM",
            libc::EBUSY => "EBUSY",
            libc::ENOTEMPTY => "ENOTEMPTY",
            libc::EEXIST => "EEXIST",
            libc::EINVAL => "EINVAL",
            libc::EMFILE => "EMFILE",
            libc::ENFILE => "ENFILE",
            libc::ENOMEM => "ENOMEM",
            libc::ENAMETOOLONG => "ENAMETOOLONG",
            libc::ENOSPC => "ENOSPC",
            libc::EROFS => "EROFS",
            libc::ELOOP => "ELOOP",
            libc::EISDIR => "EISDIR",
            libc::EIO => "EIO",
            _ => return None,
        };
        (code, -raw)
    };
    #[cfg(not(any(windows, unix)))]
    return None;
    #[cfg(any(windows, unix))]
    Some((code, errno, description(code)?))
}

/// Reuse the same native/libuv classification for a reader's path or descriptor
/// operation. Descriptor errors have no path suffix in Node's diagnostic.
pub(crate) fn operation_message(
    error: &io::Error,
    syscall: &'static str,
    path: Option<&JsString>,
    context: &Context,
) -> Option<(&'static str, JsString)> {
    let mapping = mapping(error, Syscall::Stat)?;
    let (code, _, description) = mapping;
    let suffix = match path {
        Some(path) => {
            let path = render(mapping, Syscall::Stat, context.filesystem_path(path)).path;
            format!(" '{path}'", path=String::from_utf16_lossy(&path.0))
        },
        None => String::new(),
    };
    Some((code, format!("{code}: {description}, {syscall}{suffix}").into()))
}

fn description(code: &str) -> Option<&'static str> {
    Some(match code {
        "ENOENT" => "no such file or directory",
        "ENOTDIR" => "not a directory",
        "EACCES" => "permission denied",
        "EPERM" => "operation not permitted",
        "EBUSY" => "resource busy or locked",
        "ENOTEMPTY" => "directory not empty",
        "EEXIST" => "file already exists",
        "EINVAL" => "invalid argument",
        "EMFILE" => "too many open files",
        "ENFILE" => "file table overflow",
        "ENOMEM" => "not enough memory",
        "ENAMETOOLONG" => "name too long",
        "ENOSPC" => "no space left on device",
        "EROFS" => "read-only file system",
        "ELOOP" => "too many symbolic links encountered",
        "EISDIR" => "illegal operation on a directory",
        "EIO" => "i/o error",
        _ => return None,
    })
}
