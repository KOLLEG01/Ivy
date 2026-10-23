//! Immutable v1 archive receive policy using native tar and shared host primitives.
use crate::{IvyError, IvyJson, IvyResult, artifact_manifest as artifact, config_file as config, native_path, paths,
    portable_native_files::portable_native_files_for_manifest, process::{self, LaunchContext}};
use ivy_contracts::json;
use std::{cell::Cell, collections::BTreeMap, fs, io::{self, Read}, path::{Path, PathBuf}, rc::Rc, sync::Arc, time::{Duration, Instant}};
const MAXIMUM_LIST_BYTES: usize = 8 * 1024 * 1024;
pub(super) const MAXIMUM_ENTRIES: usize = 50_000;
fn internal<T,E>(value:Result<T,E>)->IvyResult<T>{value.map_err(|_|IvyError::internal())}
fn require(value:bool,code:&str,message:&str)->IvyResult<()>{if value{Ok(())}else{Err(IvyError::new(code,message))}}
fn archive_error(error: io::Error) -> IvyError {
    if error.kind() == io::ErrorKind::TimedOut {
        IvyError::new("deadline_exceeded", "Package archive exceeded its finite deadline.").with_outcome(crate::Outcome::Unknown)
    } else { IvyError::new("artifact_changed", "Package archive headers are invalid.") }
}
fn default_tar() -> PathBuf {
    if cfg!(windows) { PathBuf::from(std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into())).join("System32/tar.exe") }
    else { PathBuf::from("/usr/bin/tar") }
}
pub(super) fn uses_native_archive(executables: &BTreeMap<String, PathBuf>) -> bool {
    // The platform default is implemented in Rust, including configurations that
    // explicitly name it. A different configured tool keeps its existing role.
    executables.get("tar").is_none_or(|configured| {
        let default = native_path::lexical(&default_tar());
        let configured = native_path::lexical(configured);
        native_path::within(&default, &configured) && native_path::within(&configured, &default)
    })
}
pub(super) async fn tar(root: &Path, executables: &BTreeMap<String, PathBuf>, args: Vec<std::ffi::OsString>,
    timeout_ms: u64, maximum_output_bytes: Option<usize>) -> IvyResult<Arc<process::ProcessResult>> {
    process::run_command(&process::Command { executable: "tar".into(), args, cwd: None, timeout_ms }, root,
        executables, LaunchContext { without_job: true, maximum_output_bytes, ..Default::default() }, None).await
}

fn safe_archive_entry(raw: &str) -> IvyResult<String> {
    let value = raw.strip_prefix("./").unwrap_or(raw);
    let value = value.strip_suffix('/').unwrap_or(value);
    require(!value.is_empty() && value.encode_utf16().count() <= 2048 && !value.contains(['\\', '\0']) && !value.starts_with('/')
        && value.split('/').all(|part| !part.is_empty() && part != "." && part != ".."),
        "artifact_changed", "Package archive contains an unsafe path.")?;
    require(value == "component.json" || value == "artifact" || value.starts_with("artifact/"),
        "artifact_changed", "Package archive contains an unexpected root.")?;
    #[cfg(windows)]
    for part in value.split('/') {
        let stem = part.split('.').next().unwrap_or("").to_ascii_uppercase();
        require(!part.contains(':') && !part.ends_with(['.', ' '])
            && !matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
            && !["COM", "LPT"].iter().any(|prefix| stem.strip_prefix(prefix).is_some_and(|n|
                matches!(n, "1"|"2"|"3"|"4"|"5"|"6"|"7"|"8"|"9"|"¹"|"²"|"³"))),
            "artifact_changed", "Package archive contains an unrepresentable Windows path.")?;
    }
    Ok(value.to_owned())
}

#[derive(Clone, Copy)]
enum ReceiveLimit { Metadata, Deadline }
struct ReceiveBounds { deadline: Instant, metadata: Cell<Option<usize>>, failure: Cell<Option<ReceiveLimit>> }
impl ReceiveBounds {
    fn check(&self) -> io::Result<()> {
        if Instant::now() < self.deadline { return Ok(()); }
        self.failure.set(Some(ReceiveLimit::Deadline));
        Err(io::Error::new(io::ErrorKind::TimedOut, "Package archive deadline"))
    }
}
struct ReceiveReader<R> { reader: R, bounds: Rc<ReceiveBounds> }
impl<R: Read> Read for ReceiveReader<R> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.bounds.check()?;
        let length = self.bounds.metadata.get().map_or(bytes.len(), |left| bytes.len().min(left + 1));
        let count = self.reader.read(&mut bytes[..length])?;
        if let Some(left) = self.bounds.metadata.get() {
            if count > left {
                self.bounds.failure.set(Some(ReceiveLimit::Metadata));
                return Err(io::Error::other("Package archive metadata is too large."));
            }
            self.bounds.metadata.set(Some(left - count));
        }
        Ok(count)
    }
}
fn read_archive(source: PathBuf, extraction: Option<PathBuf>, timeout: Duration) -> IvyResult<Vec<String>> {
    let bounds = Rc::new(ReceiveBounds { deadline: Instant::now() + timeout, metadata: Cell::new(None), failure: Cell::new(None) });
    let result = read_archive_inner(source, extraction, bounds.clone());
    match bounds.failure.get() {
        Some(ReceiveLimit::Metadata) => Err(IvyError::new("limit_exceeded", "Package archive metadata is too large.")),
        Some(ReceiveLimit::Deadline) => Err(IvyError::new("deadline_exceeded", "Package archive exceeded its finite deadline.").with_outcome(crate::Outcome::Unknown)),
        None => result,
    }
}
fn read_archive_inner(source: PathBuf, extraction: Option<PathBuf>, bounds: Rc<ReceiveBounds>) -> IvyResult<Vec<String>> {
    let file = internal(fs::File::open(source))?;
    require(internal(file.metadata())?.len() <= crate::package_validation::MAXIMUM_ARCHIVE_BYTES,
        "artifact_changed", "Package archive size is invalid.")?;
    let mut archive = tar::Archive::new(ReceiveReader { reader: flate2::read::GzDecoder::new(file), bounds: bounds.clone() });
    let extraction = extraction.map(|root| internal(paths::realpath(&root))).transpose()?;
    let mut names = Vec::new(); let mut list_bytes = 0; let mut directories = Vec::new();
    // Bound extension allocation separately from payload reads. Do not impose
    // the text-list limit on file bytes or accumulate payloads in memory.
    bounds.metadata.set(Some(MAXIMUM_LIST_BYTES + 4096));
    for entry in archive.entries().map_err(archive_error)? {
        let mut entry = entry.map_err(archive_error)?;
        let bytes = entry.path_bytes();
        let raw = std::str::from_utf8(&bytes).map_err(|_| IvyError::new("artifact_changed", "Package archive path is not UTF-8."))?;
        let name = safe_archive_entry(raw)?;
        list_bytes += raw.len() + 1;
        require(list_bytes <= MAXIMUM_LIST_BYTES, "limit_exceeded", "Package archive file list is too large.")?;
        require(names.len() < MAXIMUM_ENTRIES, "artifact_changed", "Package archive contains too many entries.")?;
        let kind = entry.header().entry_type();
        require(kind.is_file() || kind.is_dir() && entry.size() == 0,
            "artifact_changed", "Package archive accepts regular files and directories only.")?;
        names.push(name);
        bounds.metadata.set(None);
        if let Some(root) = &extraction {
            if kind.is_dir() { directories.push(entry); }
            else { require(internal(entry.unpack_in(root))?, "artifact_changed", "Package archive entry leaves its extraction root.")?; }
        } else { io::copy(&mut entry, &mut io::sink()).map_err(archive_error)?; }
        bounds.metadata.set(Some(MAXIMUM_LIST_BYTES + 4096));
    }
    if let Some(root) = &extraction {
        // Like tar's archive unpacker, apply directory modes after descendants.
        directories.sort_by(|a, b| b.path_bytes().cmp(&a.path_bytes()));
        for mut directory in directories {
            bounds.check().map_err(archive_error)?;
            require(internal(directory.unpack_in(root))?, "artifact_changed", "Package archive directory leaves its extraction root.")?;
        }
    } else { drop(directories); }
    bounds.metadata.set(None);
    io::copy(&mut archive.into_inner(), &mut io::sink()).map_err(archive_error)?;
    Ok(names)
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
    // Hash the validated files with bounded workers; file ordering is not required.
    crate::concurrency::map_concurrent(files, 4, |path, _| artifact::file_hash(path)).await?;
    Ok(())
}

pub async fn validate_package_archive(archive: PathBuf, expected_hash: String, expected_manifest: IvyJson,
    extraction_root: PathBuf, executables: BTreeMap<String, PathBuf>) -> IvyResult<()> {
    internal(tokio::spawn(async move {
        artifact::verify_transferred_archive(archive.clone(), &expected_hash).await?;
        let native = uses_native_archive(&executables);
        // Preflight every archive before any configured extractor can write.
        let source = archive.clone();
        let entries = internal(tokio::task::spawn_blocking(move || read_archive(source, None, Duration::from_secs(60))).await)??;
        require(entries.len() > 1 && entries.len() <= MAXIMUM_ENTRIES && entries.iter().any(|entry| entry == "component.json")
            && entries.iter().any(|entry| entry == "artifact/dist/build-info.json"), "artifact_changed", "Package archive is incomplete.")?;
        for file in portable_native_files_for_manifest(&expected_manifest)? {
            require(entries.iter().any(|entry| entry == &format!("artifact/{file}")), "artifact_missing", "Portable package is missing its Windows runtime payload.")?;
        }
        recreate(&extraction_root).await?;
        if native {
            let root = extraction_root.clone();
            internal(tokio::task::spawn_blocking(move || read_archive(archive, Some(root), Duration::from_secs(180))).await)??;
        } else {
            tar(&extraction_root, &executables, vec!["-xzf".into(), archive.clone().into_os_string(), "-C".into(),
                extraction_root.clone().into_os_string()], 180_000, None).await?;
            // A successful tool exit must not admit codepage-mangled filenames.
            for entry in &entries {
                let path = extraction_root.join(entry);
                let metadata = tokio::fs::symlink_metadata(&path).await.map_err(|_| IvyError::new("artifact_changed", "Package extractor did not preserve an archive path."))?;
                require(!metadata.file_type().is_symlink() && (metadata.is_file() || metadata.is_dir()),
                    "artifact_changed", "Package extractor did not preserve an archive entry.")?;
            }
        }
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
                    let path = internal(paths::absolute(&artifact_root.join(native_path::to_native(selected.unwrap()))))?;
                    let metadata = internal(tokio::fs::symlink_metadata(&path).await)?;
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
