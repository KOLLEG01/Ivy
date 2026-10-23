//! Host paths use Rust's native path syntax and OS filename representation.
//! The UTF-16 conversion is only the boundary to public configuration strings.
use ivy_contracts::JsString;
use std::{
    ffi::OsStr,
    io,
    path::{Component, Path, PathBuf},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Style {
    Windows,
    Posix,
}
impl Style {
    pub fn native() -> Self {
        if cfg!(windows) {
            Self::Windows
        } else {
            Self::Posix
        }
    }
}

/// Collapse lexical dot segments without reading the filesystem. Link-sensitive
/// I/O must use the original native path, or canonicalize it, instead.
pub(crate) fn lexical(path: &Path) -> PathBuf {
    let mut result = PathBuf::new();
    for part in path.components() {
        match part {
            Component::CurDir => (),
            Component::ParentDir => parent(&mut result),
            // Verbatim Windows paths retain these as normal components.
            Component::Normal(value) if value == "." => (),
            Component::Normal(value) if value == ".." => parent(&mut result),
            _ => result.push(part.as_os_str()),
        }
    }
    if result.as_os_str().is_empty() {
        result.push(".");
    }
    result
}
fn parent(path: &mut PathBuf) {
    if matches!(path.components().next_back(), Some(Component::Normal(_))) {
        path.pop();
    } else if !path.has_root() {
        path.push("..");
    }
}

pub fn is_absolute(_style: Style, path: &JsString) -> bool {
    to_native(path).is_absolute()
}
pub fn dirname(_style: Style, path: &JsString) -> JsString {
    let path = to_native(path);
    from_native(
        path.parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| {
                if path.has_root() {
                    &path
                } else {
                    Path::new(".")
                }
            }),
    )
}
pub fn basename(_style: Style, path: &JsString) -> JsString {
    let path = to_native(path);
    from_native(Path::new(path.file_name().unwrap_or_default()))
}
pub fn normalize(_style: Style, path: &JsString) -> JsString {
    from_native(&lexical(&to_native(path)))
}
pub fn join(_style: Style, parts: &[JsString]) -> JsString {
    let mut path = PathBuf::new();
    for part in parts {
        path.push(to_native(part));
    }
    from_native(&lexical(&path))
}

/// A host working directory. `style` describes the host for layout metadata;
/// operations always use the executing platform's native path syntax.
#[derive(Clone)]
pub struct Context {
    pub style: Style,
    pub cwd: JsString,
}
impl Context {
    pub fn current() -> io::Result<Self> {
        Ok(Self {
            style: Style::native(),
            cwd: from_native(&std::env::current_dir()?),
        })
    }
    pub fn resolve(&self, parts: &[JsString]) -> JsString {
        let mut path = to_native(&self.cwd);
        for part in parts {
            path.push(to_native(part));
        }
        // An invalid path is retained for the actual I/O operation to reject.
        // Resolution never substitutes another existing file on failure.
        let absolute = std::path::absolute(&path).unwrap_or(path);
        from_native(&lexical(&absolute))
    }
    pub fn relative(&self, from: &JsString, to: &JsString) -> JsString {
        let from = to_native(&self.resolve(std::slice::from_ref(from)));
        let to = to_native(&self.resolve(std::slice::from_ref(to)));
        from_native(&relative(&from, &to))
    }
    /// Lexical containment. Resolve links separately before admitting effects.
    pub fn inside(&self, root: &JsString, path: &JsString) -> bool {
        within(
            &to_native(&self.resolve(std::slice::from_ref(root))),
            &to_native(&self.resolve(std::slice::from_ref(path))),
        )
    }
    pub fn filesystem_path(&self, path: &JsString) -> PathBuf {
        // Keep dot segments for the OS to resolve through directory links.
        let path = to_native(path);
        if path.as_os_str().is_empty() {
            path
        } else {
            to_native(&self.cwd).join(path)
        }
    }
}

pub(crate) fn relative(from: &Path, to: &Path) -> PathBuf {
    let from: Vec<_> = from.components().collect();
    let dest: Vec<_> = to.components().collect();
    let common = from
        .iter()
        .zip(&dest)
        .take_while(|(a, b)| same_component(**a, **b))
        .count();
    if common == 0 {
        return to.to_owned();
    }
    let mut result = PathBuf::new();
    for _ in &from[common..] {
        result.push("..");
    }
    for part in &dest[common..] {
        result.push(part.as_os_str());
    }
    result
}
pub(crate) fn within(root: &Path, path: &Path) -> bool {
    let mut path = path.components();
    root.components().all(|part| {
        path.next()
            .is_some_and(|candidate| same_component(part, candidate))
    })
}
fn same_component(a: Component<'_>, b: Component<'_>) -> bool {
    #[cfg(windows)]
    if let (Component::Prefix(a), Component::Prefix(b)) = (a, b) {
        use std::path::Prefix::*;
        return match (a.kind(), b.kind()) {
            (Disk(a) | VerbatimDisk(a), Disk(b) | VerbatimDisk(b)) => a.eq_ignore_ascii_case(&b),
            (UNC(a, sa) | VerbatimUNC(a, sa), UNC(b, sb) | VerbatimUNC(b, sb)) => {
                same_name(a, b) && same_name(sa, sb)
            }
            _ => same_name(a.as_os_str(), b.as_os_str()),
        };
    }
    std::mem::discriminant(&a) == std::mem::discriminant(&b)
        && same_name(a.as_os_str(), b.as_os_str())
}
fn same_name(a: &OsStr, b: &OsStr) -> bool {
    if a == b {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Globalization::{CSTR_EQUAL, CompareStringOrdinal};
        let a: Vec<_> = a.encode_wide().collect();
        let b: Vec<_> = b.encode_wide().collect();
        let (Ok(a_len), Ok(b_len)) = (i32::try_from(a.len()), i32::try_from(b.len())) else {
            return false;
        };
        // SAFETY: explicit lengths describe live UTF-16 slices; embedded NUL
        // and unpaired surrogates remain data rather than string terminators.
        unsafe { CompareStringOrdinal(a.as_ptr(), a_len, b.as_ptr(), b_len, 1) == CSTR_EQUAL }
    }
    #[cfg(not(windows))]
    {
        a == b
    }
}

pub fn from_native(path: &Path) -> JsString {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        JsString(path.as_os_str().encode_wide().collect())
    }
    #[cfg(not(windows))]
    {
        JsString::from(path.to_string_lossy().as_ref())
    }
}
pub fn to_native(path: &JsString) -> PathBuf {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStringExt;
        std::ffi::OsString::from_wide(&path.0).into()
    }
    #[cfg(not(windows))]
    {
        String::from_utf16_lossy(&path.0).into()
    }
}
pub fn filesystem_path(path: &JsString) -> PathBuf {
    to_native(path)
}
pub fn filesystem_name(value: &OsStr) -> JsString {
    from_native(Path::new(value))
}
pub(crate) fn lowercase(value: &JsString) -> JsString {
    let mut output = Vec::with_capacity(value.0.len());
    for unit in char::decode_utf16(value.0.iter().copied()) {
        match unit {
            Ok(character) => for lower in character.to_lowercase() {
                output.extend_from_slice(lower.encode_utf16(&mut [0; 2]));
            },
            Err(unit) => output.push(unit.unpaired_surrogate()),
        }
    }
    JsString(output)
}

// Source-compatible names for consumers migrating to the native path API.
pub use filesystem_name as node_utf8;
pub fn node_filesystem_path(path: &JsString) -> io::Result<PathBuf> {
    Ok(to_native(path))
}
