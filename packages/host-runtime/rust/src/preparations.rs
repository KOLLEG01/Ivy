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
