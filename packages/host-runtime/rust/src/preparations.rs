//! Durable preparation identities and records, independent of build execution.
use crate::{IvyError, IvyJson, IvyResult, config::{read_json, write_json, FileError}, paths::{absolute, realpath}};
use ivy_contracts::core_schema::validate_host;
use std::{fs, path::{Path, PathBuf}};

fn internal<T, E>(result: Result<T, E>) -> IvyResult<T> { result.map_err(|_| IvyError::internal()) }
pub fn preparation_id_for_build(build_id: &str) -> IvyResult<String> {
    if build_id.len() != 71 || !build_id.starts_with("sha256:")
        || !build_id[7..].bytes().all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)) {
        return Err(IvyError::new("build_mismatch", "Preparation needs an exact release build identity."));
    }
    let hash = &build_id[7..]; let digit = hash.as_bytes()[16];
    let value = if digit <= b'9' { digit - b'0' } else { digit - b'a' + 10 };
    Ok(format!("{}-{}-5{}-{:x}{}-{}", &hash[..8], &hash[8..12], &hash[13..16], (value & 3) | 8, &hash[17..20], &hash[20..32]))
}
fn build_name(name: &str) -> bool {
    let Some(id) = name.strip_prefix("build-") else { return false };
    id.len() == 36 && id.bytes().enumerate().all(|(i, c)| {
        if [8, 13, 18, 23].contains(&i) { c == b'-' } else { c.is_ascii_digit() || (b'a'..=b'f').contains(&c) }
    })
}
pub(crate) fn owned_root(staging_root: &Path, root: &Path) -> IvyResult<PathBuf> {
    let valid_name = root.file_name().and_then(|v| v.to_str()).is_some_and(build_name);
    if !valid_name || internal(absolute(root))?.parent() != Some(internal(absolute(staging_root))?.as_path()) {
        return Err(IvyError::new("target_conflict", "Preparation must be one directly owned build directory."));
    }
    let metadata = internal(fs::symlink_metadata(root))?;
    let staging = internal(realpath(staging_root))?; let actual = internal(realpath(root))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() || actual.parent() != Some(staging.as_path()) || actual.file_name() != root.file_name() {
        return Err(IvyError::new("target_conflict", "Preparation directory cannot traverse a link."));
    }
    Ok(actual)
}
pub async fn save_preparation(staging_root: impl Into<PathBuf>, record: IvyJson) -> IvyResult<()> {
    let staging_root = staging_root.into();
    internal(tokio::task::spawn_blocking(move || {
        validate_host("PreparationRecord", &record)?;
        let root = staging_root.join(format!("build-{}", record.required("preparationId")?.as_str()?));
        owned_root(&staging_root, &root)?;
        write_json(&root.join("preparation.json"), &record)
    }).await)?
}
/// Read exactly the requested build record; recovery must not scan all history.
pub async fn preparation_record(staging_root: impl Into<PathBuf>, preparation_id: &str) -> IvyResult<Option<IvyJson>> {
    let staging_root = staging_root.into(); let id = preparation_id.to_owned();
    internal(tokio::task::spawn_blocking(move || read_record(&staging_root, &staging_root.join(format!("build-{id}")))).await)?
}
pub(crate) fn read_record(staging_root: &Path, root: &Path) -> IvyResult<Option<IvyJson>> {
    owned_root(staging_root, root)?;
    let record = match read_json(&root.join("preparation.json"), 32768) {
        Ok(value) => value,
        Err(FileError::Missing) => return Ok(None),
        Err(error) => return Err(error.error()),
    };
    validate_host("PreparationRecord", &record)?;
    let candidate = record.get("candidate").filter(|v| !v.is_null());
    if staging_root.join(format!("build-{}", record.required("preparationId")?.as_str()?)) != root
        || candidate.is_some_and(|v| v.get("buildId") != record.get("buildId") || v.get("componentId") != record.get("componentId")) {
        return Err(IvyError::new("build_mismatch", "Preparation record does not match its release identity."));
    }
    Ok(Some(record))
}

/// Inventory only directly owned preparation names. Invalid or linked entries
/// remain visible with their error code, without walking outside staging.
pub async fn preparation_inventory(host: IvyJson) -> IvyResult<IvyJson> {
    let staging = crate::config::path_value(host.required("stagingRoot")?)?;
    let inventory_root = staging.clone();
    let roots = internal(tokio::task::spawn_blocking(move || {
        let directory = match fs::read_dir(&inventory_root) {
            Ok(directory) => directory,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(_) => return Err(IvyError::internal()),
        };
        let mut roots = Vec::new();
        for entry in directory {
            let entry = internal(entry)?;
            if entry.file_name().to_str().is_some_and(build_name) { roots.push(entry.path()); }
        }
        if roots.len() > 10_000 {
            return Err(IvyError::new("limit_exceeded", "Preparation directory count exceeds its bounded inventory."));
        }
        // All admitted names are fixed-length ASCII UUIDs, so this is the same
        // ordering as the source's JavaScript string sort.
        roots.sort();
        Ok(roots)
    }).await)??;
    let mut entries = Vec::with_capacity(roots.len());
    for root in roots {
        let id = root.file_name().and_then(|name| name.to_str()).ok_or_else(IvyError::internal)?[6..].to_owned();
        let mut entry = IvyJson::object([
            ("preparationId", id.into()), ("path", crate::layout::path_json(&root)),
            ("phase", "unrecognized".into()), ("buildId", IvyJson::Null),
            ("candidateId", IvyJson::Null), ("bytes", IvyJson::Number(0.0)), ("code", IvyJson::Null),
        ]);
        let record_root = root.clone(); let record_staging = staging.clone();
        let record = internal(tokio::task::spawn_blocking(move || read_record(&record_staging, &record_root)).await)?;
        let measured = async {
            if let Some(record) = record? {
                entry.insert("phase", record.required("phase")?.clone());
                entry.insert("buildId", record.required("buildId")?.clone());
                entry.insert("candidateId", record.get("candidate").and_then(|candidate| candidate.get("candidateId")).cloned().unwrap_or(IvyJson::Null));
                entry.insert("code", record.required("errorCode")?.clone());
            }
            let boundary_root = root.clone(); let boundary_staging = staging.clone();
            let boundary = internal(tokio::task::spawn_blocking(move || owned_root(&boundary_staging, &boundary_root)).await)??;
            let bytes = crate::storage_space::tree_bytes(root, Some(boundary)).await?;
            entry.insert("bytes", IvyJson::Number(bytes));
            Ok::<(), IvyError>(())
        }.await;
        if let Err(error) = measured { entry.insert("code", error.code.into()); }
        entries.push(entry);
    }
    let result = IvyJson::object([
        ("schemaVersion", IvyJson::Number(1.0)), ("hostId", host.required("hostId")?.clone()),
        ("observedAt", chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true).into()),
        ("availableBytes", IvyJson::Number(crate::storage_space::available_storage_bytes(staging).await?)),
        ("entries", IvyJson::Array(entries)),
    ]);
    validate_host("PreparationInventory", &result)?;
    Ok(result)
}
