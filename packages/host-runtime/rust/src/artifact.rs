//! Runtime release-manifest boundary shared by retained and new candidates.
use crate::{IvyJson, IvyResult};
use ivy_contracts::core_schema::validate_host;
use std::path::Path;

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
    release_manifest(&crate::config::json_file(source.as_ref().to_owned(), crate::config::CONFIGURATION_BYTES).await?)
}
