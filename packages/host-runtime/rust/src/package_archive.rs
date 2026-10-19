//! Immutable v1 package transport. System tar remains the receive boundary.
use crate::{IvyError, IvyJson, IvyResult, artifact, config, node_path, paths,
    package_receive::{archive_executables, tar, MAXIMUM_ENTRIES}};
use std::{cell::Cell, collections::{BTreeMap, BTreeSet}, fs, io::{self, Read, Write}, path::{Path, PathBuf}, rc::Rc, time::{Duration, Instant}};

pub use crate::package_receive::validate_package_archive;
pub use crate::package_validation::{MAXIMUM_ARCHIVE_BYTES, validate_package_authorization, validate_package_catalog_index, validate_package_catalog};
fn internal<T, E>(value: Result<T, E>) -> IvyResult<T> { value.map_err(|_| IvyError::internal()) }
fn require(value: bool, code: &str, message: &str) -> IvyResult<()> {
    if value { Ok(()) } else { Err(IvyError::new(code, message)) }
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

#[derive(Clone, Copy)]
enum ArchiveLimit { Bytes, Deadline }
// These objects stay on one blocking thread. Retain the first limit failure at
// the operation boundary: compression/tar wrappers can discard custom I/O types.
struct ArchiveBounds { deadline: Instant, failure: Cell<Option<ArchiveLimit>> }
impl ArchiveBounds {
    fn failed(&self, reason: ArchiveLimit) { if self.failure.get().is_none() { self.failure.set(Some(reason)); } }
    fn check(&self) -> io::Result<()> {
        if Instant::now() < self.deadline { Ok(()) }
        else { self.failed(ArchiveLimit::Deadline); Err(io::Error::new(io::ErrorKind::TimedOut, "Package archive deadline")) }
    }
}
struct ArchiveWriter { file: fs::File, bytes: u64, bounds: Rc<ArchiveBounds> }
impl Write for ArchiveWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.bounds.check()?;
        if bytes.len() as u64 > MAXIMUM_ARCHIVE_BYTES - self.bytes {
            self.bounds.failed(ArchiveLimit::Bytes); return Err(io::Error::other("Package archive size is invalid."));
        }
        let count = self.file.write(bytes)?; self.bytes += count as u64; Ok(count)
    }
    fn flush(&mut self) -> io::Result<()> { self.bounds.check()?; self.file.flush() }
}
struct ArchiveReader { file: fs::File, bounds: Rc<ArchiveBounds> }
impl Read for ArchiveReader {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> { self.bounds.check()?; self.file.read(bytes) }
}
fn native_archive(artifact_root: PathBuf, linux_executables: Vec<PathBuf>) -> IvyResult<PathBuf> {
    let bounds = Rc::new(ArchiveBounds { deadline: Instant::now() + Duration::from_secs(180), failure: Cell::new(None) });
    let result = native_archive_inner(artifact_root, linux_executables, bounds.clone());
    match bounds.failure.get() {
        Some(ArchiveLimit::Bytes) => Err(IvyError::new("artifact_changed", "Package archive size is invalid.")),
        Some(ArchiveLimit::Deadline) => Err(IvyError::new("deadline_exceeded", "Package archive exceeded its finite deadline.").with_outcome(crate::Outcome::Unknown)),
        None => result,
    }
}
fn native_archive_inner(artifact_root: PathBuf, linux_executables: Vec<PathBuf>, bounds: Rc<ArchiveBounds>) -> IvyResult<PathBuf> {
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
        internal(bounds.check())?;
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
    let output = ArchiveWriter { file: internal(fs::File::create(&destination))?, bytes: 0, bounds: bounds.clone() };
    let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(output, flate2::Compression::default()));
    builder.follow_symlinks(false);
    for (source, name, directory) in entries {
        internal(bounds.check())?;
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
            internal(builder.append_pax_extensions([("path", utf8_name.as_bytes())]))?;
        }
        if directory { header.set_size(0); internal(builder.append_data(&mut header, &name, io::empty()))?; }
        else {
            let file = internal(fs::File::open(&source))?;
            require(internal(file.metadata())?.len() == metadata.len(), "artifact_changed", "Native package file changed during archive creation.")?;
            internal(builder.append_data(&mut header, &name, ArchiveReader { file, bounds: bounds.clone() }))?;
        }
    }
    let output = internal(builder.into_inner())?;
    internal(output.finish())?;
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
