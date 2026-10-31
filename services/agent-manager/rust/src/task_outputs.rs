//! Remove only the deterministic, disposable output directory of one ticket.
use crate::project_policy::ProjectPolicy;
use ivy_contracts::{IvyError, IvyJson as Json, IvyResult, JsString, hash::hash_json};
use ivy_host_runtime::{filesystem, node_path, paths};
use std::{fs, io};

fn checked_directory(policy: &ProjectPolicy, path: &JsString) -> IvyResult<bool> {
    let native = node_path::filesystem_path(path);
    let metadata = match fs::symlink_metadata(&native) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(failed(error)),
    };
    if !metadata.is_dir()
        || metadata.file_type().is_symlink()
        || !policy.same_path(
            &node_path::from_native(&paths::realpath(&native).map_err(failed)?),
            path,
        )
    {
        return Err(IvyError::new(
            "target_conflict",
            "Task output cleanup refuses redirected directories.",
        ));
    }
    Ok(true)
}
fn failed(error: io::Error) -> IvyError {
    IvyError::new(
        "task_output_cleanup_failed",
        format!("Task output cleanup could not finish: {error}"),
    )
}

pub fn cleanup(
    policy: &ProjectPolicy,
    task_key: &JsString,
    operation_id: &JsString,
) -> IvyResult<Json> {
    ProjectPolicy::workspace_key(task_key)?;
    let root = paths::future_path(&policy.internal_project_root)?;
    let outputs = policy.paths.resolve(&[root, "outputs".into()]);
    let path = policy.paths.resolve(&[outputs.clone(), task_key.clone()]);
    for boundary in &policy.protected_roots {
        if policy.paths.inside(&paths::future_path(boundary)?, &path) {
            return Err(IvyError::new(
                "target_conflict",
                "Task output cleanup cannot enter protected storage.",
            ));
        }
    }
    let identity = hash_json(&Json::String(operation_id.clone()))?;
    let retiring = policy.paths.resolve(&[
        outputs.clone(),
        format!(".retiring-{}-{}", task_key.to_utf8()?, &identity[7..]).into(),
    ]);
    let mut removed = false;
    if checked_directory(policy, &outputs)? {
        // An admitted deletion continues against its renamed directory even if
        // the caller stops waiting. Restoring a ticket can create fresh outputs
        // without a still-running removal touching that new directory.
        if checked_directory(policy, &retiring)? {
            removed = true;
        } else if checked_directory(policy, &path)? {
            match fs::rename(
                node_path::filesystem_path(&path),
                node_path::filesystem_path(&retiring),
            ) {
                Ok(()) => removed = true,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(failed(error)),
            }
        }
        filesystem::remove_force_sync(&node_path::filesystem_path(&retiring)).map_err(failed)?;
    }
    Ok(Json::object([
        ("path", Json::String(path)),
        ("removed", removed.into()),
    ]))
}
