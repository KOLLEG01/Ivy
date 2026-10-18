//! Immutable v1 package transport. System tar remains the receive boundary.
use crate::{IvyError, IvyJson, IvyResult, artifact, config, node_path, paths,
    portable_native_files::portable_native_files, process::{self, LaunchContext}};
use ivy_contracts::{core_schema::validate_host, json};
use std::{collections::{BTreeMap, BTreeSet}, fs, io::{self, Read, Write}, path::{Path, PathBuf}, sync::Arc, time::{Duration, Instant}};

pub const MAXIMUM_ARCHIVE_BYTES: u64 = 256 * 1024 * 1024;
const MAXIMUM_LIST_BYTES: usize = 8 * 1024 * 1024;
const MAXIMUM_ENTRIES: usize = 50_000;
fn internal<T, E>(value: Result<T, E>) -> IvyResult<T> { value.map_err(|_| IvyError::internal()) }
fn require(value: bool, code: &str, message: &str) -> IvyResult<()> {
    if value { Ok(()) } else { Err(IvyError::new(code, message)) }
}
fn string(value: Option<&IvyJson>) -> Option<String> { value.and_then(|value| value.as_str().ok()) }
fn safe_integer(value: Option<&IvyJson>, minimum: f64, maximum: f64) -> bool {
    value.and_then(IvyJson::as_f64).is_some_and(|n| n.is_finite() && n.fract() == 0.0 && n >= minimum && n <= maximum)
}
fn hash(value: Option<&IvyJson>) -> bool {
    string(value).is_some_and(|s| s.len() == 71 && s.starts_with("sha256:")
        && s[7..].bytes().all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)))
}
fn id(value: Option<&IvyJson>) -> bool {
    string(value).is_some_and(|s| (1..=128).contains(&s.len()) && s.as_bytes()[0].is_ascii_lowercase()
        && s.bytes().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-'))
}

pub fn validate_package_authorization(value: &IvyJson) -> IvyResult<IvyJson> {
    require(matches!(value, IvyJson::Object(_)), "invalid_arguments", "Package upload authorization needs an object.")?;
    let null = IvyJson::Null;
    let manifest = value.get("manifest").unwrap_or(&null);
    validate_host("ReleaseManifest", manifest)?;
    json::compare_versions(&json::js_string(value.get("version").unwrap_or(&IvyJson::Null)).to_utf8().unwrap_or_default(), "0.0.0")?;
    require(id(value.get("componentId")) && value.get("componentId") == manifest.get("componentId")
        && value.get("version") == manifest.get("version") && hash(value.get("buildId"))
        && value.get("buildId") == manifest.get("buildId") && hash(value.get("archiveHash"))
        && safe_integer(value.get("bytes"), 1.0, MAXIMUM_ARCHIVE_BYTES as f64),
        "invalid_arguments", "Package upload identity does not match its manifest.")?;
    Ok(IvyJson::object(["componentId", "version", "buildId", "archiveHash", "bytes", "manifest"]
        .into_iter().map(|key| (key, value.get(key).unwrap().clone()))))
}

/// Inspect stable identities before parsing another component's newer manifest.
pub fn validate_package_catalog_index(value: &IvyJson) -> IvyResult<IvyJson> {
    require(matches!(value, IvyJson::Object(_)), "invalid_arguments", "Package catalog needs an object.")?;
    let packages = value.get("packages").and_then(IvyJson::as_array);
    require(value.get("schemaVersion") == Some(&IvyJson::Number(1.0))
        && safe_integer(value.get("revision"), 0.0, 9_007_199_254_740_991.0)
        && packages.is_some_and(|packages| packages.len() <= 4096), "invalid_arguments", "Package catalog is invalid.")?;
    for entry in packages.unwrap() {
        require(matches!(entry, IvyJson::Object(_)) && id(entry.get("componentId"))
            && entry.get("version").is_some_and(|value| value.as_js_string().is_some()),
            "invalid_arguments", "Package catalog entry identity is invalid.")?;
        json::compare_versions(&entry.required("version")?.as_js_string().unwrap().to_utf8().unwrap_or_default(), "0.0.0")?;
    }
    Ok(value.clone())
}

pub fn validate_package_catalog(value: &IvyJson) -> IvyResult<IvyJson> {
    validate_package_catalog_index(value)?;
    for entry in value.required("packages")?.as_array().unwrap() {
        let authorized = validate_package_authorization(entry)?;
        let published = entry.get("publishedAt").and_then(IvyJson::as_js_string);
        let publisher = entry.get("publisherPrincipalId").and_then(IvyJson::as_js_string);
        require(safe_integer(entry.get("revision"), 1.0, 9_007_199_254_740_991.0)
            && published.is_some_and(|value| ivy_sdk::clock::parse_date(value).is_finite())
            && publisher.is_some_and(|value| (1..=256).contains(&value.0.len()))
            && json::canonical(authorized.required("manifest")?, usize::MAX)? == json::canonical(entry.required("manifest")?, usize::MAX)?,
            "invalid_arguments", "Package catalog entry is invalid.")?;
    }
    Ok(value.clone())
}

fn archive_executables(mut executables: BTreeMap<String, PathBuf>) -> BTreeMap<String, PathBuf> {
    executables.entry("tar".into()).or_insert_with(|| {
        if cfg!(windows) { PathBuf::from(std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into())).join("System32/tar.exe") }
        else { PathBuf::from("/usr/bin/tar") }
    });
    executables
}
async fn tar(root: &Path, executables: &BTreeMap<String, PathBuf>, args: Vec<std::ffi::OsString>,
    timeout_ms: u64, maximum_output_bytes: Option<usize>) -> IvyResult<Arc<process::ProcessResult>> {
    process::run_command(&process::Command { executable: "tar".into(), args, cwd: None, timeout_ms }, root,
        executables, LaunchContext { without_job: true, maximum_output_bytes, ..Default::default() }, None).await
}

#[derive(Clone, Debug)]
pub struct PackageArchive { pub path: PathBuf, pub hash: String, pub bytes: u64 }
async fn archive_result(destination: PathBuf) -> IvyResult<PackageArchive> {
    let metadata = internal(tokio::fs::symlink_metadata(&destination).await)?;
    require(metadata.is_file() && !metadata.file_type().is_symlink() && metadata.len() > 0
        && metadata.len() <= MAXIMUM_ARCHIVE_BYTES, "artifact_changed", "Package archive size is invalid.")?;
    Ok(PackageArchive { hash: artifact::file_hash(destination.clone()).await?, path: destination, bytes: metadata.len() })
}
pub async fn create_package_archive(candidate: IvyJson, executables: BTreeMap<String, PathBuf>) -> IvyResult<PackageArchive> {
    // Keep the process and its result owner alive if a caller stops waiting.
    internal(tokio::spawn(async move {
        let artifact_root = config::path_value(candidate.required("artifactRoot")?)?;
        let root = artifact_root.parent().ok_or_else(IvyError::internal)?;
        let destination = root.join("package.tar.gz");
        let result = tar(root, &archive_executables(executables), vec!["-czf".into(), destination.clone().into_os_string(),
            "-C".into(), root.as_os_str().into(), "artifact".into(), "component.json".into()], 180_000, None).await?;
        require(!result.truncated, "artifact_changed", "Package archive command output was truncated.")?;
        archive_result(destination).await
    }).await)?
}

#[derive(Debug)]
struct ArchiveTooLarge;
impl std::fmt::Display for ArchiveTooLarge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str("Package archive size is invalid.") }
}
impl std::error::Error for ArchiveTooLarge {}
fn archive_io<T>(value: io::Result<T>) -> IvyResult<T> {
    value.map_err(|error| {
        let mut cause: Option<&(dyn std::error::Error + 'static)> = Some(&error);
        while let Some(current) = cause {
            if current.is::<ArchiveTooLarge>() { return IvyError::new("artifact_changed", "Package archive size is invalid."); }
            // io::Error::source can skip its custom payload's outer type. Inspect
            // get_ref so tar's contextual wrappers retain our bounded failures.
            if let Some(error) = current.downcast_ref::<io::Error>() {
                if error.kind() == io::ErrorKind::TimedOut {
                    return IvyError::new("deadline_exceeded", "Package archive exceeded its finite deadline.").with_outcome(crate::Outcome::Unknown);
                }
                cause = error.get_ref().map(|inner| inner as &(dyn std::error::Error + 'static));
            } else { cause = current.source(); }
        }
        IvyError::internal()
    })
}
fn check_deadline(deadline: Instant) -> io::Result<()> {
    if Instant::now() < deadline { Ok(()) } else { Err(io::Error::new(io::ErrorKind::TimedOut, "Package archive deadline")) }
}
struct ArchiveWriter { file: fs::File, bytes: u64, deadline: Instant }
impl Write for ArchiveWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        check_deadline(self.deadline)?;
        if bytes.len() as u64 > MAXIMUM_ARCHIVE_BYTES - self.bytes { return Err(io::Error::other(ArchiveTooLarge)); }
        let count = self.file.write(bytes)?; self.bytes += count as u64; Ok(count)
    }
    fn flush(&mut self) -> io::Result<()> { check_deadline(self.deadline)?; self.file.flush() }
}
struct ArchiveReader { file: fs::File, deadline: Instant }
impl Read for ArchiveReader {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> { check_deadline(self.deadline)?; self.file.read(bytes) }
}
fn native_archive(artifact_root: PathBuf, linux_executables: Vec<PathBuf>) -> IvyResult<PathBuf> {
    let deadline = Instant::now() + Duration::from_secs(180);
    let actual_root = internal(paths::realpath(&artifact_root))?;
    let package_root = artifact_root.parent().ok_or_else(IvyError::internal)?;
    let mut selected = BTreeSet::new();
    let context = internal(node_path::Context::current())?;
    for executable in linux_executables {
        let local = context.relative(&node_path::from_native(&artifact_root), &node_path::from_native(&executable));
        let local = local.to_utf8().unwrap_or_default().replace('\\', "/"); let parts: Vec<_> = local.split('/').collect();
        require(executable.is_absolute() && parts.len() == 4 && parts[0] == "dist" && parts[1] == "native"
            && matches!(parts[2], "linux-x64" | "linux-arm64") && !parts[3].is_empty()
            && parts[3].bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-')),
            "invalid_arguments", "Linux package executable must be an explicit contained native target file.")?;
        let metadata = internal(fs::symlink_metadata(&executable))?;
        let actual = internal(paths::realpath(&executable))?;
        require(metadata.is_file() && !metadata.file_type().is_symlink() && metadata.len() > 0
            && internal(paths::inside(&actual_root, &actual))? && selected.insert(local),
            "invalid_arguments", "Linux package executable must be a unique nonempty contained regular file.")?;
    }
    let mut entries = Vec::new(); let mut directories = vec![(artifact_root.clone(), PathBuf::from("artifact"))];
    let manifest = package_root.join("component.json");
    let metadata = internal(fs::symlink_metadata(&manifest))?;
    require(metadata.is_file() && !metadata.file_type().is_symlink(), "artifact_changed", "Package manifest is not a regular file.")?;
    entries.push((manifest, PathBuf::from("component.json"), false));
    while let Some((directory, local)) = directories.pop() {
        archive_io(check_deadline(deadline))?;
        let metadata = internal(fs::symlink_metadata(&directory))?;
        require(metadata.is_dir() && !metadata.file_type().is_symlink(), "artifact_changed", "Native package accepts contained files and directories only.")?;
        entries.push((directory.clone(), local.clone(), true));
        for entry in internal(fs::read_dir(directory))? {
            let entry = internal(entry)?; let path = entry.path(); let kind = internal(entry.file_type())?;
            require(!kind.is_symlink() && internal(paths::inside(&actual_root, &internal(paths::realpath(&path))?))?,
                "artifact_changed", "Native package accepts contained files and directories only.")?;
            if kind.is_dir() { directories.push((path, local.join(entry.file_name()))); }
            else {
                require(kind.is_file(), "artifact_changed", "Native package found an unsupported entry.")?;
                entries.push((path, local.join(entry.file_name()), false));
            }
            require(entries.len() + directories.len() <= MAXIMUM_ENTRIES, "limit_exceeded", "Package archive contains too many entries.")?;
        }
    }
    let destination = package_root.join("package.tar.gz");
    match fs::symlink_metadata(&destination) {
        Ok(metadata) => require(metadata.is_file() && !metadata.file_type().is_symlink(), "artifact_changed", "Package archive must be a regular file.")?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}, Err(_) => return Err(IvyError::internal()),
    }
    let output = ArchiveWriter { file: internal(fs::File::create(&destination))?, bytes: 0, deadline };
    let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(output, flate2::Compression::default()));
    builder.follow_symlinks(false);
    for (source, name, directory) in entries {
        archive_io(check_deadline(deadline))?;
        let metadata = internal(fs::symlink_metadata(&source))?;
        require(!metadata.file_type().is_symlink() && metadata.is_dir() == directory && (directory || metadata.is_file())
            && (name == Path::new("component.json") || internal(paths::inside(&actual_root, &internal(paths::realpath(&source))?))?),
            "artifact_changed", "Native package entry changed during archive creation.")?;
        let mut header = tar::Header::new_gnu(); header.set_metadata(&metadata);
        #[cfg(windows)] header.set_mode(match (directory, metadata.permissions().readonly()) { (true, false) => 0o777, (true, true) => 0o555, (false, false) => 0o666, (false, true) => 0o444 });
        let local = name.strip_prefix("artifact").ok().and_then(|path| path.to_str()).unwrap_or("").replace('\\', "/");
        if selected.contains(&local) { header.set_mode(0o755); }
        let utf8_name = name.to_str().ok_or_else(IvyError::internal)?.replace('\\', "/");
        if !utf8_name.is_ascii() {
            // PAX path is explicitly UTF-8; raw GNU names are locale-dependent
            // in the unchanged Windows receiver. The standard library writes it.
            archive_io(builder.append_pax_extensions([("path", utf8_name.as_bytes())]))?;
        }
        if directory { header.set_size(0); archive_io(builder.append_data(&mut header, &name, io::empty()))?; }
        else {
            let file = internal(fs::File::open(&source))?;
            require(internal(file.metadata())?.len() == metadata.len(), "artifact_changed", "Native package file changed during archive creation.")?;
            archive_io(builder.append_data(&mut header, &name, ArchiveReader { file, deadline }))?;
        }
    }
    let output = archive_io(builder.into_inner())?;
    archive_io(output.finish())?;
    Ok(destination)
}

/// Consume stageNativePayloads' exact absolute linuxExecutables. No source modes
/// are changed. This explicit native writer emits the same v1 artifact/manifest
/// roots; ordinary callers keep create_package_archive's original tar behavior.
pub async fn create_package_archive_with_linux_executables(candidate: IvyJson, linux_executables: Vec<PathBuf>) -> IvyResult<PackageArchive> {
    internal(tokio::spawn(async move {
        let root = config::path_value(candidate.required("artifactRoot")?)?;
        let destination = internal(tokio::task::spawn_blocking(move || native_archive(root, linux_executables)).await)??;
        archive_result(destination).await
    }).await)?
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
        for file in portable_native_files(&expected_manifest.required("componentId")?.as_str()?) {
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
        for file in portable_native_files(&manifest.required("componentId")?.as_str()?) {
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
                let selected = command.get("args").and_then(IvyJson::as_array).and_then(|args| args.first()).and_then(IvyJson::as_js_string);
                if command.get("executable") == Some(&IvyJson::from("node")) && selected.is_some_and(|entry| entry.0.starts_with(&[100,105,115,116,47])) {
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
