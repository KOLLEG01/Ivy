//! Read-only maintenance markers shared by bootstrap and service startup.
use crate::{
    IvyError, IvyJson, IvyResult,
    node_path::{self, Style},
};
use ivy_contracts::JsString;
use std::path::{Path, PathBuf};

pub fn runtime_reset_marker(config: &IvyJson) -> IvyResult<PathBuf> {
    let root = config
        .get("ivyRoot")
        .filter(|v| !v.is_null())
        .map(|v| v.as_js_string().cloned().ok_or_else(IvyError::internal))
        .unwrap_or_else(|| {
            Ok(node_path::dirname(
                Style::native(),
                config
                    .required("runtimeRoot")?
                    .as_js_string()
                    .ok_or_else(IvyError::internal)?,
            ))
        })?;
    Ok(node_path::to_native(&node_path::join(
        Style::native(),
        &[root, "reset".into(), "maintenance.json".into()],
    )))
}
pub fn hive_reset_bootstrap_marker(data_root: &Path) -> PathBuf {
    node_path::to_native(&node_path::join(
        Style::native(),
        &[
            node_path::from_native(data_root),
            JsString::from("runtime-reset-bootstrap.json"),
        ],
    ))
}
pub async fn runtime_reset_active(config: &IvyJson) -> IvyResult<bool> {
    let marker = runtime_reset_marker(config)?;
    tokio::task::spawn_blocking(move || {
        match std::fs::symlink_metadata(node_path::filesystem_path(&node_path::from_native(
            &marker,
        ))) {
            Ok(metadata) => Ok(metadata.is_file()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(_) => Err(IvyError::internal()),
        }
    })
    .await
    .map_err(|_| IvyError::internal())?
}
