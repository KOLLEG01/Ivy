use crate::{IvyError, IvyResult, JsString, checked_native_contract::native_versions};
use std::path::PathBuf;

/// Select an existing platform catalog, otherwise return the default path.
/// Platform/architecture defaults describe this process; paths use the owning
/// OS's native syntax and filename representation.
pub fn native_catalog_path(
    root: &JsString,
    version: &JsString,
    platform: Option<&JsString>,
    architecture: Option<&JsString>,
) -> IvyResult<JsString> {
    let default_platform = JsString::from(match std::env::consts::OS {
        "windows" => "win32",
        "macos" => "darwin",
        v => v,
    });
    let default_architecture = JsString::from(match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        "x86" => "ia32",
        v => v,
    });
    let platform = platform.unwrap_or(&default_platform);
    let architecture = architecture.unwrap_or(&default_architecture);
    let valid = |value: &JsString| {
        !value.0.is_empty()
            && value
                .0
                .iter()
                .all(|c| (97..=122).contains(c) || (48..=57).contains(c))
    };
    if !native_versions()?.contains(version) || !valid(platform) || !valid(architecture) {
        return Err(IvyError::new(
            "native_version_unsupported",
            "Supported native platform and version required.",
        ));
    }
    let root: PathBuf = {
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStringExt;
            std::ffi::OsString::from_wide(&root.0).into()
        }
        #[cfg(not(windows))]
        {
            root.to_utf8()?.into()
        }
    };
    let directory = root
        .join("specs")
        .join("native")
        .join(format!("codex-{}", version.to_utf8()?));
    let selected = directory.join(format!(
        "catalog.{}-{}.json",
        platform.to_utf8()?,
        architecture.to_utf8()?
    ));
    let path = if selected.exists() {
        selected
    } else {
        directory.join("catalog.json")
    };
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        Ok(JsString(path.as_os_str().encode_wide().collect()))
    }
    #[cfg(not(windows))]
    {
        Ok(JsString::from(
            path.to_str().ok_or_else(IvyError::internal)?,
        ))
    }
}
