//! Release manifest projection and transferred archive identity shared by receivers.
use crate::{IvyError,IvyJson,IvyResult};
use ivy_contracts::core_schema::validate_host;
use std::{fs,path::{Path,PathBuf}};
use crate::streaming_hash::hash_file;
pub use crate::streaming_hash::file_hash;
fn internal<T,E>(value:Result<T,E>)->IvyResult<T>{value.map_err(|_|IvyError::internal())}
fn require(value:bool,code:&str,message:&str)->IvyResult<()>{if value{Ok(())}else{Err(IvyError::new(code,message))}}
/// Check bytes received from another host; local immutable candidate activation
/// uses the identity stamp and selected entry, without rehashing its entire tree.
pub async fn verify_transferred_archive(source: impl Into<PathBuf>, expected_hash: &str) -> IvyResult<()> {
    let source = source.into(); let expected = expected_hash.to_owned();
    internal(tokio::task::spawn_blocking(move || {
        require(expected.len() == 71 && expected.starts_with("sha256:") && expected[7..].bytes().all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)),
            "invalid_arguments", "Transferred archive needs an explicit SHA-256 checksum.")?;
        let metadata = internal(fs::symlink_metadata(&source))?;
        require(metadata.is_file() && !metadata.file_type().is_symlink(), "artifact_changed", "Transferred archive must be a regular file.")?;
        require(hash_file(&source)? == expected, "artifact_changed", "Transferred candidate archive changed.")
    }).await)?
}

/// Historical preparation/provenance fields have no launch meaning.
pub fn release_manifest(value: &IvyJson) -> IvyResult<IvyJson> {
    let manifest = IvyJson::object([
        "schemaVersion", "componentId", "kind", "version", "buildId", "connectsToHive", "requirements",
        "app", "entrypoint", "readiness", "shutdown", "restart", "storage", "runtimeReset",
    ].into_iter().filter_map(|key| value.get(key).map(|value| (key, value.clone()))));
    validate_host("ReleaseManifest", &manifest)?;
    Ok(manifest)
}
pub async fn read_release_manifest(source: impl AsRef<Path>) -> IvyResult<IvyJson> {
    release_manifest(&crate::config_file::json_file(source.as_ref().to_owned(), crate::config_file::CONFIGURATION_BYTES).await?)
}
