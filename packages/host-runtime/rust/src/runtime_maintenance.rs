//! Read-only maintenance markers shared by bootstrap and service startup.
use crate::{IvyError, IvyJson, IvyResult, config, native_path};
use std::path::{Path, PathBuf};

pub fn runtime_reset_marker(config: &IvyJson) -> IvyResult<PathBuf> {
    let root = match config.get("ivyRoot").filter(|value| !value.is_null()) {
        Some(value) => config::path_value(value)?,
        None => {
            let runtime = config::path_value(config.required("runtimeRoot")?)?;
            runtime
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .map(Path::to_path_buf)
                .unwrap_or_else(|| {
                    if runtime.has_root() {
                        runtime
                    } else {
                        PathBuf::from(".")
                    }
                })
        }
    };
    Ok(native_path::lexical(
        &root.join("reset").join("maintenance.json"),
    ))
}
pub fn hive_reset_bootstrap_marker(data_root: &Path) -> PathBuf {
    native_path::lexical(&data_root.join("runtime-reset-bootstrap.json"))
}
pub async fn runtime_reset_active(config: &IvyJson) -> IvyResult<bool> {
    let marker = runtime_reset_marker(config)?;
    tokio::task::spawn_blocking(move || match std::fs::symlink_metadata(&marker) {
        Ok(metadata) => Ok(metadata.is_file()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err(IvyError::internal()),
    })
    .await
    .map_err(|_| IvyError::internal())?
}
