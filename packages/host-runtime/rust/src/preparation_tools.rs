//! Explicit nested-build tool environment; no arbitrary parent credentials.
use crate::{
    IvyError, IvyJson, IvyResult, config,
    node_path::{self, Style},
    node_runtime::NodeRuntime,
    paths,
};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
};

#[derive(Debug)]
pub struct PreparationTools {
    pub environment: BTreeMap<OsString, OsString>,
}
fn require(ok: bool, message: &str) -> IvyResult<()> {
    if ok {
        Ok(())
    } else {
        Err(IvyError::new("invalid_arguments", message))
    }
}
fn internal<T, E>(value: Result<T, E>) -> IvyResult<T> {
    value.map_err(|_| IvyError::internal())
}

/// The executing Host's discovered intentional Node runtime is explicit. It
/// replaces TS process.execPath; the current Rust binary is never a Node tool.
pub async fn preparation_tools(
    mut executables: IvyJson,
    bootstrap_root: PathBuf,
    temporary_root: PathBuf,
    node: impl Into<Option<NodeRuntime>>,
) -> IvyResult<PreparationTools> {
    let node = node.into();
    internal(
        tokio::task::spawn_blocking(move || {
            let temporary = internal(paths::realpath(&temporary_root))?;
            let mut environment: BTreeMap<OsString, OsString> = ["TEMP", "TMP", "TMPDIR"]
                .into_iter()
                .map(|key| (key.into(), temporary.as_os_str().to_owned()))
                .collect();
            require(
                executables.as_object().is_some(),
                "Preparation tool mappings require an object.",
            )?;
            if let Some(node) = node {
                executables.insert("$node", crate::layout::path_json(&node.executable));
            }
            if cfg!(windows) {
                executables.insert(
                    "$job",
                    crate::layout::path_json(&crate::native_plan::native_binary_path(
                        &bootstrap_root,
                        "ivy-job",
                    )),
                );
            }
            let mut compilers = BTreeMap::new();
            let mut selected_node = None;
            for (name, requested) in executables.as_object().ok_or_else(IvyError::internal)? {
                let requested = config::path_value(requested)?;
                require(
                    node_path::is_absolute(Style::native(), &node_path::from_native(&requested)),
                    "Preparation tool mappings require absolute executable paths.",
                )?;
                let path = internal(paths::realpath(&requested))?;
                let metadata = internal(fs::metadata(&path))?;
                let lower = path.as_os_str().to_string_lossy().to_ascii_lowercase();
                require(
                    metadata.is_file()
                        && ![".cmd", ".bat", ".ps1"]
                            .iter()
                            .any(|suffix| lower.ends_with(*suffix)),
                    "Preparation tools require direct executable files.",
                )?;
                if name == &"node".into() || (name == &"$node".into() && selected_node.is_none()) {
                    selected_node = Some(path.clone());
                }
                if name == &"dotnet".into() {
                    environment.insert("IVY_DOTNET".into(), path.into_os_string());
                } else if name == &"gcc".into() {
                    environment.insert("IVY_EVS_CC".into(), path.into_os_string());
                } else if ["cargo", "rustc", "cl"]
                    .iter()
                    .any(|key| name == &(*key).into())
                {
                    compilers.insert(name.to_utf8()?, path);
                }
            }
            compiler_environment(&mut environment, &compilers, selected_node.as_deref())?;
            Ok(PreparationTools { environment })
        })
        .await,
    )?
}

fn compiler_environment(
    environment: &mut BTreeMap<OsString, OsString>,
    tools: &BTreeMap<String, PathBuf>,
    node: Option<&Path>,
) -> IvyResult<()> {
    let mut search = Vec::new();
    if let Some(node) = node {
        // npm lifecycle commands use bare node through the Windows shell.
        // Admit ordinary spelling only when it names the same selected file.
        #[cfg(windows)]
        let node = PathBuf::from(crate::platform::windows_arguments::application_path(
            node.as_os_str(),
        ));
        search.push(node.parent().ok_or_else(IvyError::internal)?.to_path_buf());
    }
    for (tool, key) in [("cargo", "IVY_CARGO"), ("rustc", "RUSTC"), ("cl", "CC")] {
        if let Some(path) = tools.get(tool) {
            environment.insert(key.into(), path.as_os_str().to_owned());
            search.push(path.parent().ok_or_else(IvyError::internal)?.to_path_buf());
        }
    }
    if tools.contains_key("cargo") {
        if let Some(home) = std::env::var_os("CARGO_HOME").filter(|value| !value.is_empty()) {
            let home = PathBuf::from(home);
            require(
                home.is_absolute() && home.is_dir(),
                "CARGO_HOME must identify the selected Cargo cache directory.",
            )?;
            environment.insert("CARGO_HOME".into(), home.into_os_string());
        }
    }
    #[cfg(windows)]
    if let Some(compiler) = tools.get("cl") {
        let include = std::env::var_os("INCLUDE").filter(|value| !value.is_empty());
        let library = std::env::var_os("LIB").filter(|value| !value.is_empty());
        if include.is_none() || library.is_none() {
            let (includes, libraries, bin) = windows_compiler_paths(compiler)?;
            environment.insert("INCLUDE".into(), include.unwrap_or(includes));
            environment.insert("LIB".into(), library.unwrap_or(libraries));
            search.push(bin);
        } else {
            environment.insert("INCLUDE".into(), include.unwrap());
            environment.insert("LIB".into(), library.unwrap());
        }
    }
    if !search.is_empty() {
        search.extend(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        ));
        environment.insert(
            "PATH".into(),
            std::env::join_paths(search).map_err(|_| {
                IvyError::new(
                    "invalid_arguments",
                    "Preparation tool search paths are invalid.",
                )
            })?,
        );
    }
    Ok(())
}

#[cfg(windows)]
fn windows_compiler_paths(compiler: &Path) -> IvyResult<(OsString, OsString, PathBuf)> {
    let arch = compiler
        .parent()
        .and_then(Path::file_name)
        .ok_or_else(IvyError::internal)?;
    require(
        ["x64", "x86", "arm64"].iter().any(|value| arch == *value),
        "The selected MSVC compiler needs its target architecture directory.",
    )?;
    let msvc = msvc_search_path(compiler.ancestors().nth(4).ok_or_else(IvyError::internal)?);
    let sdk = std::env::var_os("WindowsSdkDir")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("ProgramFiles(x86)")
                .map(|root| PathBuf::from(root).join("Windows Kits/10"))
        })
        .ok_or_else(|| {
            IvyError::new(
                "invalid_arguments",
                "Select the installed Windows SDK with WindowsSdkDir.",
            )
        })?;
    let sdk = msvc_search_path(&sdk);
    let pinned = std::env::var_os("WindowsSDKVersion")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    let mut versions = internal(fs::read_dir(sdk.join("Include")))?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let version = entry.file_name();
            let numbers = version
                .to_str()?
                .split('.')
                .map(str::parse::<u32>)
                .collect::<Result<Vec<_>, _>>()
                .ok()?;
            (numbers.len() == 4 && pinned.as_ref().is_none_or(|pin| pin == Path::new(&version)))
                .then_some((numbers, version))
        })
        .collect::<Vec<_>>();
    versions.sort_by(|left, right| right.0.cmp(&left.0));
    for (_, version) in versions {
        let includes = [
            msvc.join("include"),
            sdk.join("Include").join(&version).join("ucrt"),
            sdk.join("Include").join(&version).join("shared"),
            sdk.join("Include").join(&version).join("um"),
        ];
        let libraries = [
            msvc.join("lib").join(arch),
            sdk.join("Lib").join(&version).join("ucrt").join(arch),
            sdk.join("Lib").join(&version).join("um").join(arch),
        ];
        let bin = sdk.join("bin").join(&version).join(arch);
        if includes
            .iter()
            .chain(libraries.iter())
            .all(|path| path.is_dir())
            && bin.is_dir()
        {
            return Ok((
                internal(std::env::join_paths(includes))?,
                internal(std::env::join_paths(libraries))?,
                bin,
            ));
        }
    }
    Err(IvyError::new(
        "invalid_arguments",
        "The selected MSVC compiler needs a complete Windows SDK for its target architecture.",
    ))
}

/// MSVC's INCLUDE/LIB parser requires DOS/UNC search paths. Keep every UTF-16
/// unit after the namespace prefix; canonical filesystem paths remain unchanged.
#[cfg(windows)]
fn msvc_search_path(path: &Path) -> PathBuf {
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use std::path::{Component, Prefix};
    let units = path.as_os_str().encode_wide().collect::<Vec<_>>();
    match path.components().next() {
        Some(Component::Prefix(prefix)) => match prefix.kind() {
            Prefix::VerbatimDisk(_) => PathBuf::from(OsString::from_wide(&units[4..])),
            Prefix::VerbatimUNC(_, _) => {
                let mut unc = vec![b'\\' as u16, b'\\' as u16];
                unc.extend_from_slice(&units[8..]);
                PathBuf::from(OsString::from_wide(&unc))
            }
            _ => path.to_path_buf(),
        },
        _ => path.to_path_buf(),
    }
}
