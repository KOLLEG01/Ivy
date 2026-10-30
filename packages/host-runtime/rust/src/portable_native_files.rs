/// Windows payloads required in the universal package on Windows hosts.
pub fn portable_native_files(component_id: &str) -> &'static [&'static str] {
    match component_id {
        "host-executor" => &["dist/native/ivy-job.exe", "dist/native/ivy-host-job.exe"],
        "service-manager" | "agent-manager" | "phone-bridge" => &["dist/native/ivy-job.exe"],
        _ => &[],
    }
}

/// Native source artifacts colocate helpers with their selected executable.
pub fn portable_native_files_for_manifest(
    manifest: &crate::IvyJson,
) -> crate::IvyResult<Vec<String>> {
    if !cfg!(windows) {
        return Ok(Vec::new());
    }
    let files = portable_native_files(&manifest.required("componentId")?.as_str()?);
    let directory = if crate::native_plan::is_native_plan(manifest) {
        let executable = manifest
            .required("entrypoint")?
            .required("executable")?
            .as_str()?;
        Some(
            executable
                .rsplit_once('/')
                .ok_or_else(crate::IvyError::internal)?
                .0
                .to_owned(),
        )
    } else {
        None
    };
    Ok(files
        .iter()
        .map(|file| match &directory {
            Some(directory) => format!("{directory}/{}", file.rsplit('/').next().unwrap()),
            None => (*file).to_owned(),
        })
        .collect())
}

/// The caller supplies its admitted package layout; a missing target payload
/// never falls back to an older or differently targeted helper.
#[derive(Clone, Copy)]
pub enum NativeHelperLayout {
    Legacy,
    Target,
}

pub(crate) fn native_platform() -> crate::IvyResult<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "x86_64") => Ok("win32-x64"),
        ("windows", "aarch64") => Ok("win32-arm64"),
        ("linux", "x86_64") => Ok("linux-x64"),
        ("linux", "aarch64") => Ok("linux-arm64"),
        _ => Err(crate::IvyError::new(
            "unsupported_platform",
            "Native package does not support this host target.",
        )),
    }
}

/// Resolve one helper from the same explicit immutable artifact root. This is
/// path admission only; retain the existing candidate/build/launch admission.
/// Target uses stageNativePayloads' current OS/arch directory; Legacy preserves
/// the ordinary flat dist/native layout. Pass the exact basename, with suffix.
pub fn component_native_helper(
    artifact_root: &std::path::Path,
    name: &str,
    layout: NativeHelperLayout,
) -> crate::IvyResult<std::path::PathBuf> {
    use crate::{IvyError, paths};
    let invalid = || {
        IvyError::new(
            "invalid_arguments",
            "Native helper requires an absolute artifact root and one explicit basename.",
        )
    };
    if !artifact_root.is_absolute()
        || name.is_empty()
        || name.len() > 128
        || !name.as_bytes()[0].is_ascii_alphanumeric()
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
    {
        return Err(invalid());
    }
    let missing = || {
        IvyError::new(
            "artifact_missing",
            "Native helper must be a direct regular file in its admitted artifact.",
        )
    };
    let root = paths::absolute(artifact_root).map_err(|_| missing())?;
    let actual_root = paths::realpath(&root).map_err(|_| missing())?;
    if !(paths::inside(&root, &actual_root).map_err(|_| missing())?
        && paths::inside(&actual_root, &root).map_err(|_| missing())?)
    {
        return Err(missing());
    }
    let relative = match layout {
        NativeHelperLayout::Legacy => format!("dist/native/{name}"),
        NativeHelperLayout::Target => format!("dist/native/{}/{name}", native_platform()?),
    };
    let helper = paths::direct_native_file(&root, &relative)?;
    let actual_helper = paths::realpath(&helper).map_err(|_| missing())?;
    if !(paths::inside(&helper, &actual_helper).map_err(|_| missing())?
        && paths::inside(&actual_helper, &helper).map_err(|_| missing())?)
    {
        return Err(missing());
    }
    // Keep the admitted root's spelling after checking its resolved identity.
    // Windows canonical paths add a verbatim prefix absent from the OS plan.
    Ok(helper)
}
