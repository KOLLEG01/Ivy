//! Fixed source snapshot metadata and runtime-discovered component build plans.
use crate::{IvyError, IvyJson, IvyResult, config::{read_json, path_value, FileError, CONFIGURATION_BYTES}, paths::inside};
use ivy_contracts::core_schema::validate_host;
use std::path::PathBuf;

fn internal<T, E>(result: Result<T, E>) -> IvyResult<T> { result.map_err(|_| IvyError::internal()) }

/// Metadata validation deliberately does not walk or rehash the fixed snapshot.
pub async fn verify_snapshot_metadata(snapshot: IvyJson, staging_root: impl Into<PathBuf>) -> IvyResult<IvyJson> {
    let staging_root = staging_root.into();
    internal(tokio::task::spawn_blocking(move || {
        validate_host("SourceSnapshot", &snapshot)?;
        if !internal(inside(&staging_root, &path_value(snapshot.required("sourceRoot")?)?))?
            || !internal(inside(&staging_root, &path_value(snapshot.required("manifestPath")?)?))? {
            return Err(IvyError::new("target_conflict", "Source snapshot is outside this host staging root."));
        }
        let manifest = read_json(&path_value(snapshot.required("manifestPath")?)?, 8 * 1024 * 1024).map_err(FileError::error)?;
        validate_host("SnapshotManifest", &manifest)?;
        let recorded = manifest.required("snapshot")?;
        if recorded.get("snapshotId") != snapshot.get("snapshotId") || recorded.get("sourceRoot") != snapshot.get("sourceRoot") {
            return Err(IvyError::new("artifact_changed", "Snapshot metadata changed."));
        }
        Ok(manifest)
    }).await)?
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
            1 => Ok(plans.remove(0)),
            _ => Err(IvyError::new("target_conflict", "Source component has more than one deploy plan.")),
        }
    }).await)?
}
