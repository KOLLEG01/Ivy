//! Immutable v1 archive receive policy using native tar and shared host primitives.
use crate::{IvyError, IvyJson, IvyResult, artifact_manifest as artifact, config_file as config, node_path, paths,
    portable_native_files::portable_native_files_for_manifest, process::{self, LaunchContext}};
use ivy_contracts::json;
use std::{collections::BTreeMap, fs, path::{Path, PathBuf}, sync::Arc};
const MAXIMUM_LIST_BYTES: usize = 8 * 1024 * 1024;
pub(super) const MAXIMUM_ENTRIES: usize = 50_000;
fn internal<T,E>(value:Result<T,E>)->IvyResult<T>{value.map_err(|_|IvyError::internal())}
fn require(value:bool,code:&str,message:&str)->IvyResult<()>{if value{Ok(())}else{Err(IvyError::new(code,message))}}
pub(super) fn archive_executables(mut executables: BTreeMap<String, PathBuf>) -> BTreeMap<String, PathBuf> {
    executables.entry("tar".into()).or_insert_with(|| {
        if cfg!(windows) { PathBuf::from(std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into())).join("System32/tar.exe") }
        else { PathBuf::from("/usr/bin/tar") }
    });
    executables
}
pub(super) async fn tar(root: &Path, executables: &BTreeMap<String, PathBuf>, args: Vec<std::ffi::OsString>,
    timeout_ms: u64, maximum_output_bytes: Option<usize>) -> IvyResult<Arc<process::ProcessResult>> {
    process::run_command(&process::Command { executable: "tar".into(), args, cwd: None, timeout_ms }, root,
        executables, LaunchContext { without_job: true, maximum_output_bytes, ..Default::default() }, None).await
}

fn safe_archive_entry(raw: &str) -> IvyResult<String> {
    let value = raw.strip_prefix("./").unwrap_or(raw);
    let value = value.strip_suffix('/').unwrap_or(value);
    require(!value.is_empty() && value.encode_utf16().count() <= 2048 && !value.contains('\\') && !value.starts_with('/')
        && value.split('/').all(|part| !part.is_empty() && part != "." && part != ".."),
        "artifact_changed", "Package archive contains an unsafe path.")?;
    require(value == "component.json" || value == "artifact" || value.starts_with("artifact/"),
        "artifact_changed", "Package archive contains an unexpected root.")?;
    Ok(value.to_owned())
}
async fn recreate(root: &Path) -> IvyResult<()> {
    match tokio::fs::symlink_metadata(root).await {
        Ok(metadata) => crate::filesystem::remove_payload(root.to_owned(), metadata.is_dir(), 0).await?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
        Err(_) => return Err(IvyError::internal()),
    }
    internal(tokio::fs::create_dir_all(root).await)
}
async fn check_artifact_files(root: PathBuf) -> IvyResult<()> {
    let files = internal(tokio::task::spawn_blocking(move || {
        let mut directories = vec![root.clone()]; let mut files = Vec::new();
        while let Some(directory) = directories.pop() {
            for entry in internal(fs::read_dir(directory))? {
                let entry = internal(entry)?; let path = entry.path(); let kind = internal(entry.file_type())?;
                require(internal(paths::inside(&root, &path))? && !kind.is_symlink(), "artifact_changed", "Package comparison accepts contained files and directories only.")?;
                if kind.is_dir() { directories.push(path); }
                else { require(kind.is_file(), "artifact_changed", "Package comparison found an unsupported entry.")?; files.push(path); }
            }
        }
        require(files.len() <= MAXIMUM_ENTRIES, "limit_exceeded", "Package comparison exceeds its file limit.")?;
        Ok::<_, IvyError>(files)
    }).await)??;
    require(!files.is_empty(), "artifact_changed", "Package artifact is empty.")?;
    // The receiver used artifactFiles only for validation, not its locale-sorted
    // result. Preserve its bounded hash/read checks without inventing an ordering.
    crate::concurrency::map_concurrent(files, 4, |path, _| artifact::file_hash(path)).await?;
    Ok(())
}

pub async fn validate_package_archive(archive: PathBuf, expected_hash: String, expected_manifest: IvyJson,
    extraction_root: PathBuf, executables: BTreeMap<String, PathBuf>) -> IvyResult<()> {
    internal(tokio::spawn(async move {
        artifact::verify_transferred_archive(archive.clone(), &expected_hash).await?;
        let executables = archive_executables(executables);
        let listing = tar(archive.parent().ok_or_else(IvyError::internal)?, &executables,
            vec!["-tzf".into(), archive.clone().into_os_string()], 60_000, Some(MAXIMUM_LIST_BYTES)).await?;
        require(!listing.truncated, "limit_exceeded", "Package archive file list is too large.")?;
        let entries = listing.stdout.split('\n').map(|line| line.strip_suffix('\r').unwrap_or(line)).filter(|line| !line.is_empty())
            .map(safe_archive_entry).collect::<IvyResult<Vec<_>>>()?;
        require(entries.len() > 1 && entries.len() <= MAXIMUM_ENTRIES && entries.iter().any(|entry| entry == "component.json")
            && entries.iter().any(|entry| entry == "artifact/dist/build-info.json"), "artifact_changed", "Package archive is incomplete.")?;
        for file in portable_native_files_for_manifest(&expected_manifest)? {
            require(entries.iter().any(|entry| entry == &format!("artifact/{file}")), "artifact_missing", "Portable package is missing its Windows runtime payload.")?;
        }
        recreate(&extraction_root).await?;
        tar(&extraction_root, &executables, vec!["-xzf".into(), archive.clone().into_os_string(), "-C".into(),
            extraction_root.clone().into_os_string()], 180_000, None).await?;
        let actual_root = internal(paths::realpath(&extraction_root))?;
        let artifact_root = internal(paths::realpath(&extraction_root.join("artifact")))?;
        require(internal(paths::inside(&actual_root, &artifact_root))?, "artifact_changed", "Package artifact leaves its extraction root.")?;
        let component_path = extraction_root.join("component.json"); let metadata = internal(tokio::fs::symlink_metadata(&component_path).await)?;
        require(metadata.is_file() && !metadata.file_type().is_symlink(), "artifact_changed", "Package manifest is not a regular file.")?;
        let manifest = artifact::read_release_manifest(component_path).await?;
        require(json::canonical(&manifest, usize::MAX)? == json::canonical(&expected_manifest, usize::MAX)?,
            "build_mismatch", "Package manifest differs from its authorized identity.")?;
        for file in portable_native_files_for_manifest(&manifest)? {
            let path = artifact_root.join(file); let metadata = internal(tokio::fs::symlink_metadata(&path).await)?;
            require(metadata.is_file() && !metadata.file_type().is_symlink() && metadata.len() > 0
                && internal(paths::inside(&artifact_root, &internal(paths::realpath(&path))?))?, "artifact_changed", "Portable Windows runtime payload is invalid.")?;
        }
        check_artifact_files(artifact_root.clone()).await?;
        let info = config::json_file(artifact_root.join("dist/build-info.json"), config::CONFIGURATION_BYTES).await?;
        require(info.get("schemaVersion") == Some(&IvyJson::Number(1.0)) && info.get("componentId") == manifest.get("componentId")
            && info.get("version") == manifest.get("version") && info.get("buildId") == manifest.get("buildId"),
            "build_mismatch", "Package build stamp differs from its manifest.")?;
        if manifest.get("kind") != Some(&IvyJson::from("app")) {
            for command in [manifest.required("entrypoint")?, manifest.required("readiness")?.required("command")?] {
                let executable = command.get("executable").and_then(IvyJson::as_js_string);
                let native = executable.is_some_and(|entry| entry.0.starts_with(&"dist/native/".encode_utf16().collect::<Vec<_>>()));
                let selected = if native {
                    executable
                } else { command.get("args").and_then(IvyJson::as_array).and_then(|args| args.first()).and_then(IvyJson::as_js_string) };
                if (native || command.get("executable") == Some(&IvyJson::from("node"))) && selected.is_some_and(|entry| entry.0.starts_with(&[100,105,115,116,47])) {
                    let path = node_path::Context::current().map_err(|_| IvyError::internal())?.resolve(&[node_path::from_native(&artifact_root), selected.unwrap().clone()]);
                    let path = node_path::to_native(&path); let metadata = internal(tokio::fs::symlink_metadata(&path).await)?;
                    require(internal(paths::inside(&artifact_root, &path))? && metadata.is_file() && !metadata.file_type().is_symlink(),
                        "artifact_changed", "Package launch entry is unavailable.")?;
                }
            }
        } else {
            let app = manifest.required("app")?;
            require(app.get("appId") == manifest.get("componentId"), "build_mismatch", "App package identity differs from its component.")?;
            let app_root = artifact_root.join(config::path_value(app.required("dist")?)?);
            let entry = app_root.join(config::path_value(app.required("entryPath")?)?);
            let app_metadata = internal(tokio::fs::symlink_metadata(&app_root).await)?;
            let entry_metadata = internal(tokio::fs::symlink_metadata(&entry).await)?;
            require(internal(paths::inside(&artifact_root, &app_root))? && app_metadata.is_dir() && !app_metadata.file_type().is_symlink()
                && internal(paths::inside(&app_root, &entry))? && entry_metadata.is_file() && !entry_metadata.file_type().is_symlink(),
                "artifact_changed", "UI package bundle is incomplete.")?;
        }
        Ok(())
    }).await)?
}
