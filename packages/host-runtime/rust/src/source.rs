//! Fixed Git source snapshots and runtime-discovered component build plans.
use crate::{IvyError, IvyJson, IvyResult, config::{read_json, path_value, FileError, CONFIGURATION_BYTES}, paths::inside};
use ivy_contracts::{JsString, core_schema::validate_host};
use std::{collections::{BTreeMap, BTreeSet}, fs, path::{Path, PathBuf}, sync::{Arc, OnceLock}};
use crate::{concurrency::map_concurrent, exec_file::{source_output, FileCommand}, host_config::HostContext, layout, native_path::{self, Style}, paths};

fn internal<T, E>(result: Result<T, E>) -> IvyResult<T> { result.map_err(|_| IvyError::internal()) }
fn require(ok: bool, code: &str, message: &str) -> IvyResult<()> {
    if ok { Ok(()) } else { Err(IvyError::new(code, message)) }
}
fn text(value: &IvyJson) -> IvyResult<JsString> { value.as_js_string().cloned().ok_or_else(IvyError::internal) }
fn field(value: &IvyJson, key: &str) -> IvyResult<JsString> { text(value.required(key)?) }
async fn blocking<T: Send + 'static>(work: impl FnOnce() -> IvyResult<T> + Send + 'static) -> IvyResult<T> {
    internal(tokio::task::spawn_blocking(work).await)?
}

/// Metadata validation deliberately does not walk or rehash the fixed snapshot.
pub async fn verify_snapshot_metadata(snapshot: IvyJson, staging_root: impl Into<PathBuf>) -> IvyResult<IvyJson> {
    let staging_root = staging_root.into();
    internal(tokio::task::spawn_blocking(move || {
        validate_host("SourceSnapshot", &snapshot)?;
        let source_root = path_value(snapshot.required("sourceRoot")?)?;
        let manifest_path = path_value(snapshot.required("manifestPath")?)?;
        if !internal(inside(&staging_root, &source_root))?
            || !internal(inside(&staging_root, &manifest_path))? {
            return Err(IvyError::new("target_conflict", "Source snapshot is outside this host staging root."));
        }
        let manifest = read_json(&manifest_path, 8 * 1024 * 1024).map_err(FileError::error)?;
        validate_host("SnapshotManifest", &manifest)?;
        let recorded = manifest.required("snapshot")?;
        if recorded.get("snapshotId") != snapshot.get("snapshotId") || recorded.get("sourceRoot") != snapshot.get("sourceRoot") {
            return Err(IvyError::new("artifact_changed", "Snapshot metadata changed."));
        }
        Ok(manifest)
    }).await)?
}

const ROOT_FILES: [&str; 11] = ["package.json", "package-lock.json", "tsconfig.json", "tsconfig.web.json", "tsconfig.runtime.json", "tsconfig.test.json", "README.md", "LICENSE", ".npmrc", ".gitattributes", ".gitignore"];
const TOP_LEVELS: [&str; 9] = ["packages", "services", "ui", "instructions", "tools", "specs", "docs", "tests", "acceptance"];
fn same(units: &[u16], value: &str) -> bool { units.iter().copied().eq(value.encode_utf16()) }
fn rust_root_inputs() -> IvyResult<&'static Vec<JsString>> {
    // The build owner supplies the same bounded list to Cargo input/cache and
    // test selection. Never recursively admit .cargo credentials or downloads.
    static INPUTS: OnceLock<IvyResult<Vec<JsString>>> = OnceLock::new();
    INPUTS.get_or_init(|| serde_json::from_str::<Vec<String>>(include_str!("../../../../tools/build/rust-root-inputs.json"))
        .map(|paths| paths.into_iter().map(JsString::from).collect()).map_err(|_| IvyError::internal())).as_ref().map_err(Clone::clone)
}
fn allowed(path: &JsString, rust_inputs: &[JsString]) -> bool {
    let first = path.0.split(|c| *c == 47).next().unwrap_or_default();
    if !ROOT_FILES.iter().any(|v| same(&path.0, v)) && !TOP_LEVELS.iter().any(|v| same(first, v)) && !rust_inputs.contains(path) { return false; }
    !path.0.split(|c| *c == 47).any(|part| {
        ["node_modules", "dist", ".local", "config.json", ".env"].iter().any(|v| same(part, v))
            || part.starts_with(&[46,101,110,118,46]) && !part.iter().any(|c| matches!(*c, 10 | 13 | 0x2028 | 0x2029))
    })
}
async fn source_paths(source: &Path, executable: PathBuf, bootstrap_root: PathBuf) -> IvyResult<Vec<JsString>> {
    let git = source_output(FileCommand {
        executable: executable.into_os_string(),
        args: ["ls-files", "--cached", "--others", "--exclude-standard", "-z"].map(Into::into).to_vec(),
        cwd: Some(source.to_owned()), timeout_ms: 10_000, maximum_buffer_bytes: 64 * 1024 * 1024,
    }, bootstrap_root).await?;
    require(git.failure.is_none() && git.exit_code == Some(0), "invalid_arguments", "Source must be an accessible Git checkout.")?;
    // Unique public paths retain their stable protocol ordering.
    let rust_inputs = rust_root_inputs()?;
    let paths: BTreeSet<_> = git.stdout.0.split(|c| *c == 0).filter(|v| !v.is_empty()).map(|v| JsString(v.to_vec())).filter(|path| allowed(path, rust_inputs)).collect();
    require(paths.len() <= 20_000, "limit_exceeded", "Source copy exceeds its file limit.")?;
    Ok(paths.into_iter().collect())
}
struct SourceFile { path: PathBuf, absolute: PathBuf, bytes: u64, mode: u32 }
type CheckedDirectories = Arc<tokio::sync::Mutex<BTreeMap<PathBuf, Arc<tokio::sync::OnceCell<IvyResult<()>>>>>>;
async fn source_files(source: PathBuf, paths: Vec<JsString>, required: BTreeSet<JsString>) -> IvyResult<Vec<SourceFile>> {
    let checked: CheckedDirectories = Arc::new(tokio::sync::Mutex::new(BTreeMap::new()));
    let files = map_concurrent(paths, 8, |path, _| {
        let source = source.clone(); let checked = checked.clone(); let required = required.contains(&path);
        async move {
            let source_for_file = source.clone();
            let file = blocking(move || {
                let path = native_path::to_native(&path);
                let absolute = source_for_file.join(&path);
                let metadata = match fs::symlink_metadata(&absolute) {
                    Ok(value) => value,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound && !required => return Ok(None),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Err(IvyError::new("artifact_missing", "Declared build input is missing.")),
                    Err(_) => return Err(IvyError::internal()),
                };
                require(internal(inside(&source_for_file, &absolute))?
                    && metadata.is_file() && !metadata.file_type().is_symlink(), "invalid_arguments", "Source input must be a contained regular file.")?;
                #[cfg(unix)] let mode = { use std::os::unix::fs::MetadataExt; metadata.mode() & 0o777 };
                #[cfg(not(unix))] let mode = 0;
                Ok(Some(SourceFile { path, absolute, bytes: metadata.len(), mode }))
            }).await?;
            let Some(file) = file else { return Ok(None) };
            let directory = file.absolute.parent().ok_or_else(IvyError::internal)?.to_owned();
            let ready = checked.lock().await.entry(directory.clone()).or_default().clone();
            ready.get_or_init(|| blocking(move || {
                let actual = internal(paths::realpath(&directory))?;
                require(internal(inside(&source, &actual))?, "invalid_arguments", "Source input leaves its checkout.")
            })).await.clone()?;
            require(file.bytes <= 64 * 1024 * 1024, "limit_exceeded", "Source copy exceeds its per-file byte limit.")?;
            Ok(Some(file))
        }
    }).await?;
    Ok(files.into_iter().flatten().collect())
}

/// Capture runs to completion even if a caller drops its waiter. No accepted
/// journal record refers to mutable checkout inputs or to an incoming directory.
pub async fn capture_source(original: JsString, config: IvyJson, bootstrap_root: PathBuf) -> IvyResult<IvyJson> {
    internal(tokio::spawn(capture(original, config, bootstrap_root, None)).await)?
}
/// Preparation selects one component without changing the general snapshot API.
pub async fn capture_component_source(
    original: JsString,
    config: IvyJson,
    bootstrap_root: PathBuf,
    component: JsString,
) -> IvyResult<IvyJson> {
    crate::preparation_inputs::validate_component_id(&component)?;
    internal(tokio::spawn(capture(original, config, bootstrap_root, Some(component))).await)?
}
pub(crate) async fn component_source_inputs(
    root: PathBuf,
    component: JsString,
) -> IvyResult<Vec<JsString>> {
    blocking(move || {
        crate::build_inputs::source_inputs(
            &root,
            &component.to_utf8()?,
            crate::portable_native_files::native_platform()?,
        )
    })
    .await
}
async fn capture(original: JsString, config: IvyJson, bootstrap_root: PathBuf, component: Option<JsString>) -> IvyResult<IvyJson> {
    let executable = config.get("executables").and_then(|v| v.get("git")).and_then(IvyJson::as_js_string)
        .filter(|v| !v.0.is_empty()).map(native_path::to_native).ok_or_else(|| IvyError::new("invalid_arguments", "Host preparation requires an explicit Git executable."))?;
    let original = native_path::to_native(&original);
    require(original.is_absolute(), "invalid_arguments", "Source must be an explicit absolute checkout path.")?;
    let source_config = config.clone();
    let source = blocking(move || {
        let source = internal(paths::realpath(&original))?;
        let mut roots = layout::storage_overlap_roots(&source_config, || Ok(HostContext::current()?.account_home))?;
        roots.extend([field(&source_config, "artifactRoot")?, field(&source_config, "stagingRoot")?]);
        let config_path = path_value(&layout::host_configuration_path_for_style(&source_config, Style::native())?)?;
        let mut overlap = false;
        for root in roots {
            let root = native_path::to_native(&root);
            if internal(inside(&root, &source))? || internal(inside(&source, &root))? { overlap = true; break; }
        }
        require(!overlap && !internal(inside(&source, &config_path))?,
            "target_conflict", "Release source must stay separate from private service state, internal projects, configuration, artifacts and staging.")?;
        Ok(source)
    }).await?;
    let required = match component {
        Some(component) => component_source_inputs(source.clone(), component).await?,
        None => Vec::new(),
    };
    let mut paths = source_paths(&source, executable, bootstrap_root).await?;
    paths.extend(required.iter().cloned());
    let paths: Vec<_> = paths.into_iter().collect::<BTreeSet<_>>().into_iter().collect();
    require(paths.len() <= 20_000, "limit_exceeded", "Source copy exceeds its file limit.")?;
    let snapshot_id = uuid::Uuid::new_v4().to_string();
    require(paths.contains(&"package.json".into()) && paths.contains(&"package-lock.json".into()), "invalid_arguments", "A source candidate needs its package and dependency lock.")?;
    let files = source_files(source.clone(), paths, required.into_iter().collect()).await?;
    let bytes: u64 = files.iter().map(|file| file.bytes).sum();
    require(bytes <= 1024 * 1024 * 1024, "limit_exceeded", "Source copy exceeds its byte limit.")?;
    let staging = path_value(config.required("stagingRoot")?)?;
    let directory = staging.clone(); blocking(move || internal(fs::create_dir_all(directory))).await?;
    crate::storage_space::require_storage_space(staging.clone(), (bytes * 2).max(64 * 1024 * 1024) as f64).await?;
    let final_root = staging.join("sources").join(&snapshot_id);
    let incoming = staging.join(format!("incoming-{}", uuid::Uuid::new_v4()));
    let result = publish(files, source, snapshot_id, staging, incoming.clone(), final_root).await;
    if result.is_err() {
        blocking(move || match fs::remove_dir_all(&incoming) {
            Ok(()) => Ok(()), Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()), Err(_) => Err(IvyError::internal()),
        }).await?;
    }
    result
}
async fn publish(files: Vec<SourceFile>, original: PathBuf, snapshot_id: String, staging: PathBuf, incoming: PathBuf, final_root: PathBuf) -> IvyResult<IvyJson> {
    let source_root = incoming.join("source");
    let root = source_root.clone(); blocking(move || internal(fs::create_dir_all(root))).await?;
    // Preserve directory admission order and drain bounded workers on failure.
    let mut seen = BTreeSet::new();
    let mut directories = Vec::new();
    for file in &files {
        let destination = source_root.join(&file.path);
        let directory = destination.parent().ok_or_else(IvyError::internal)?.to_owned();
        if seen.insert(directory.clone()) { directories.push(directory); }
    }
    map_concurrent(directories, 8, |directory, _| blocking(move || internal(fs::create_dir_all(directory)))).await?;
    map_concurrent(files, 8, |file, _| {
        let root = source_root.clone();
        blocking(move || {
            let destination = root.join(&file.path);
            require(internal(inside(&root, &destination))?, "invalid_arguments", "Source file path leaves its snapshot.")?;
            internal(fs::copy(&file.absolute, &destination))?;
            #[cfg(unix)] { use std::os::unix::fs::PermissionsExt; internal(fs::set_permissions(destination, fs::Permissions::from_mode(file.mode)))?; }
            #[cfg(not(unix))] let _ = file.mode;
            Ok(())
        })
    }).await?;
    let snapshot = IvyJson::object([
        ("snapshotId", snapshot_id.into()), ("sourceRoot", layout::path_json(&final_root.join("source"))),
        ("originalRoot", layout::path_json(&original)), ("manifestPath", layout::path_json(&final_root.join("snapshot.json"))),
        ("capturedAt", chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true).into()),
    ]);
    let manifest_path = incoming.join("snapshot.json");
    crate::config::atomic_json(manifest_path, IvyJson::object([("schemaVersion", IvyJson::Number(1.0)), ("snapshot", snapshot.clone())])).await?;
    let parent = final_root.parent().ok_or_else(IvyError::internal)?.to_owned();
    blocking(move || internal(fs::create_dir_all(parent))).await?;
    require(internal(inside(&staging, &incoming))? && internal(inside(&staging, &final_root))?, "invalid_arguments", "Snapshot promotion leaves its staging root.")?;
    blocking(move || internal(fs::rename(&incoming, &final_root))).await?;
    verify_snapshot_metadata(snapshot.clone(), staging).await?;
    Ok(snapshot)
}

pub async fn verify_snapshot(snapshot: IvyJson, staging_root: impl Into<PathBuf>) -> IvyResult<()> {
    verify_snapshot_metadata(snapshot, staging_root).await.map(|_| ())
}

pub async fn source_build_plan(root: impl Into<PathBuf>, component_id: &str) -> IvyResult<IvyJson> {
    let root = root.into(); let component_id = component_id.to_owned();
    internal(tokio::task::spawn_blocking(move || {
        if !component_id.bytes().next().is_some_and(|c| c.is_ascii_lowercase())
            || !component_id.bytes().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-') {
            return Err(IvyError::new("invalid_arguments", "Component ID is not a source component name."));
        }
        let mut plans = Vec::new();
        for owner in ["services", "ui"] {
            let plan = match read_json(&root.join(owner).join(&component_id).join("deploy.json"), CONFIGURATION_BYTES) {
                Ok(value) => value,
                Err(FileError::Missing) => continue,
                Err(error) => return Err(error.error()),
            };
            validate_host("BuildPlan", &plan)?;
            if plan.get("componentId") != Some(&IvyJson::from(component_id.clone())) {
                return Err(IvyError::new("invalid_arguments", "Source deploy plan does not match its component directory."));
            }
            plans.push(plan);
        }
        match plans.len() {
            0 => Err(IvyError::new("not_found", "Source component has no deploy plan.")),
            1 => crate::native_plan::materialize_native_plan_for_host(plans.remove(0)),
            _ => Err(IvyError::new("target_conflict", "Source component has more than one deploy plan.")),
        }
    }).await)?
}
