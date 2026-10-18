//! Pinned optional npm platform payloads; system tar owns archive extraction.
use crate::{IvyError, IvyJson, IvyResult, config, filesystem, node_path::{self, Style}, process::{self, LaunchContext}};
use ivy_contracts::{JsString, json};
use sha2::{Digest, Sha512};
use base64::{Engine, engine::general_purpose::STANDARD};
use std::{collections::{BTreeMap, BTreeSet}, fs, io::Write, path::{Path, PathBuf}};

pub const MAXIMUM_PORTABLE_PACKAGE_BYTES: usize = 64 * 1024 * 1024;
fn require(ok: bool, code: &str, message: &str) -> IvyResult<()> {
    if ok { Ok(()) } else { Err(IvyError::new(code, message)) }
}
fn internal<T, E>(value: Result<T, E>) -> IvyResult<T> { value.map_err(|_| IvyError::internal()) }
fn fs_path(path: &Path) -> PathBuf { node_path::filesystem_path(&node_path::from_native(path)) }
fn local(root: &Path, location: &JsString) -> PathBuf {
    let mut parts = vec![node_path::from_native(root)];
    parts.extend(location.0.split(|c| *c == 47).map(|part| JsString(part.to_vec())));
    node_path::to_native(&node_path::join(Style::native(), &parts))
}
fn intersects(value: Option<&IvyJson>, supported: &[&str]) -> bool {
    value.is_none() || value.and_then(IvyJson::as_array).is_some_and(|values| values.iter()
        .any(|entry| entry.as_js_string().is_some_and(|entry| supported.iter().any(|value| entry == &JsString::from(*value)))))
}
fn integrity(value: &JsString) -> bool {
    let prefix: Vec<_> = "sha512-".encode_utf16().collect();
    let Some(suffix) = value.0.strip_prefix(prefix.as_slice()) else { return false };
    let data = suffix.iter().take_while(|c| **c != 61).count();
    data > 0 && suffix.len() - data <= 2 && suffix[..data].iter().all(|c| matches!(*c, 65..=90 | 97..=122 | 48..=57 | 43 | 47))
        && suffix[data..].iter().all(|c| *c == 61)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PortableDependency {
    pub location: JsString,
    pub resolved: JsString,
    pub integrity: JsString,
    pub version: JsString,
}

/// The selected set comes from exact runtime lock locations. Preserve its input
/// order and the original stat-before-metadata behavior, including existing dirs.
pub async fn portable_dependency_plan(lock: IvyJson, selected: Vec<JsString>, artifact_root: PathBuf) -> IvyResult<Vec<PortableDependency>> {
    internal(tokio::task::spawn_blocking(move || {
        if selected.is_empty() { return Ok(Vec::new()) }
        if lock.is_null() { return Err(IvyError::internal()) }
        let packages = lock.get("packages");
        require(packages.is_some_and(|value| matches!(value, IvyJson::Object(_) | IvyJson::Array(_))),
            "invalid_arguments", "Portable packaging requires exact npm package metadata.")?;
        let packages = packages.unwrap();
        let mut missing = Vec::new(); let mut seen = BTreeSet::new();
        for location in selected {
            if !seen.insert(location.clone()) { continue }
            match fs::metadata(fs_path(&local(&artifact_root, &location))) {
                Ok(_) => continue,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
                Err(_) => return Err(IvyError::internal()),
            }
            let metadata = packages.get_key(&location);
            let os = metadata.and_then(|value| value.get("os"));
            let cpu = metadata.and_then(|value| value.get("cpu"));
            if os.is_none() && cpu.is_none() || !intersects(os, &["win32", "linux"]) || !intersects(cpu, &["x64", "arm64"]) { continue }
            let metadata = metadata.unwrap();
            let resolved = metadata.get("resolved").and_then(IvyJson::as_js_string);
            let checksum = metadata.get("integrity").and_then(IvyJson::as_js_string);
            let version = metadata.get("version").and_then(IvyJson::as_js_string);
            require(resolved.is_some_and(|value| value.0.starts_with(&"https://".encode_utf16().collect::<Vec<_>>()))
                && checksum.is_some_and(integrity) && version.is_some(),
                "artifact_missing", "Portable runtime dependency lacks pinned registry bytes.")?;
            missing.push(PortableDependency { location, resolved: resolved.unwrap().clone(), integrity: checksum.unwrap().clone(), version: version.unwrap().clone() });
        }
        Ok(missing)
    }).await)?
}

/// Check the raw response header before reading/decoding its body. Fetch keeps
/// that header even when Content-Encoding changes the resulting byte length.
pub fn check_portable_response(successful: bool, declared_length: Option<&str>) -> IvyResult<()> {
    require(successful, "service_unavailable", "Pinned portable runtime dependency is unavailable.")?;
    let declared = json::number_from_string(&declared_length.unwrap_or("0").into());
    require(declared == 0.0 || declared.is_nan() || declared <= MAXIMUM_PORTABLE_PACKAGE_BYTES as f64,
        "content_too_large", "Portable runtime dependency exceeds its package limit.")
}
pub fn verify_portable_bytes(value: &PortableDependency, bytes: &[u8]) -> IvyResult<()> {
    require(!bytes.is_empty() && bytes.len() <= MAXIMUM_PORTABLE_PACKAGE_BYTES
        && JsString::from(format!("sha512-{}", STANDARD.encode(Sha512::digest(bytes)))) == value.integrity,
        "artifact_changed", "Portable runtime dependency checksum differs from the lockfile.")
}
pub fn portable_archive_entries(stdout: &str, truncated: bool) -> IvyResult<()> {
    let entries: Vec<_> = stdout.split_inclusive('\n').map(|line| {
        line.strip_suffix('\n').map_or(line, |line| line.strip_suffix('\r').unwrap_or(line))
    }).filter(|line| !line.is_empty()).collect();
    require(!truncated && !entries.is_empty() && entries.len() <= 50_000 && entries.iter().all(|raw| {
        let entry = raw.strip_suffix('/').unwrap_or(raw);
        entry == "package" || entry.starts_with("package/") && !entry.contains('\\')
            && entry.split('/').all(|part| part != ".." && part != ".")
    }), "artifact_changed", "Portable runtime dependency archive is unsafe.")
}
fn package_name(location: &JsString) -> JsString {
    let marker: Vec<_> = "/node_modules/".encode_utf16().collect();
    let nested = location.0.windows(marker.len()).rposition(|part| part == marker);
    let suffix = &location.0[nested.map_or(13.min(location.0.len()), |index| index + marker.len())..];
    let mut parts = suffix.split(|c| *c == 47);
    let first = parts.next().unwrap_or_default(); let mut name = first.to_vec();
    if first.first() == Some(&64) { if let Some(second) = parts.next() { name.push(47); name.extend_from_slice(second); } }
    JsString(name)
}
async fn tar(root: &Path, executables: &BTreeMap<String, PathBuf>, args: Vec<std::ffi::OsString>) -> IvyResult<std::sync::Arc<process::ProcessResult>> {
    process::run_command(&process::Command { executable: "tar".into(), args, cwd: None, timeout_ms: 60_000 }, root,
        executables, LaunchContext { without_job: true, ..Default::default() }, None).await
}
/// Bytes are the decoded fetch body. Keep archive creation outside the cleanup
/// region: a failed exclusive create must never remove another writer's file.
pub async fn install_portable_dependency(value: PortableDependency, bytes: Vec<u8>, artifact_root: PathBuf,
    temporary_root: PathBuf, executables: BTreeMap<String, PathBuf>) -> IvyResult<()> {
    internal(tokio::spawn(async move {
        verify_portable_bytes(&value, &bytes)?;
        let key = ivy_contracts::hash::digest(String::from_utf16_lossy(&value.location.0).as_bytes());
        let archive = local(&temporary_root, &format!("{}.tgz", &key[7..]).into());
        let destination = local(&artifact_root, &value.location);
        let output = archive.clone();
        internal(tokio::task::spawn_blocking(move || -> std::io::Result<()> {
            fs::OpenOptions::new().write(true).create_new(true).open(fs_path(&output))?.write_all(&bytes)
        }).await).and_then(internal)?;
        let result = async {
            let listing = tar(&temporary_root, &executables, vec!["-tzf".into(), archive.as_os_str().into()]).await?;
            portable_archive_entries(&listing.stdout, listing.truncated)?;
            let target = destination.clone();
            internal(tokio::task::spawn_blocking(move || fs::create_dir_all(fs_path(&target))).await).and_then(internal)?;
            tar(&temporary_root, &executables, vec!["-xzf".into(), archive.as_os_str().into(), "-C".into(), destination.as_os_str().into(), "--strip-components".into(), "1".into()]).await?;
            let installed = config::json_file(local(&destination, &"package.json".into()), 1024 * 1024).await?;
            if installed.is_null() { return Err(IvyError::internal()) }
            require(installed.get("name").and_then(IvyJson::as_js_string) == Some(&package_name(&value.location))
                && installed.get("version").and_then(IvyJson::as_js_string) == Some(&value.version),
                "build_mismatch", "Portable runtime dependency identity differs from the lockfile.")
        }.await;
        let _ = filesystem::remove_file_force(archive).await;
        result
    }).await)?
}
