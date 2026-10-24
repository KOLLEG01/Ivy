use super::*;
use crate::{backup_files::BackupFile, node_runtime::NodeRuntime, storage_space};
use std::{path::Path, time::Instant};

/// The original workload runtime is part of the immutable backup identity.
pub fn require_restore_platform<'a>(manifest: &J, node: impl Into<Option<&'a NodeRuntime>>) -> IvyResult<()> {
    let platform = manifest.required("platform")?;
    let node = node.into();
    let node_matches = platform.get("node") == Some(&"unavailable".into())
        || node.is_some_and(|node| platform.get("node") == Some(&format!("v{}", node.version).into()));
    let os = if cfg!(windows) { "win32" } else { "linux" };
    let arch = match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        value => value,
    };
    require(
        platform.get("os") == Some(&os.into())
            && platform.get("arch") == Some(&arch.into())
            && node_matches,
        "unsupported_runtime",
        "Restore requires its original OS/architecture/Node toolchain.",
    )
}
fn captured(manifest: &J) -> IvyResult<Vec<StorageArea>> {
    manifest
        .get("storageAreas")
        .filter(|v| !v.is_null())
        .map(array)
        .transpose()?
        .unwrap_or(&[])
        .iter()
        .map(|area| {
            Ok(StorageArea {
                key: field(area, "key")?,
                path: field(area, "path")?,
            })
        })
        .collect()
}
fn number(value: &J) -> IvyResult<f64> {
    if let J::Number(value) = value {
        Ok(*value)
    } else {
        Err(IvyError::internal())
    }
}
fn total_bytes(manifest: &J) -> IvyResult<f64> {
    array(manifest.required("files")?)?
        .iter()
        .try_fold(0.0, |sum, file| Ok(sum + number(file.required("bytes")?)?))
}
async fn directory(path: &Path) -> IvyResult<()> {
    let mut builder = tokio::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    builder.mode(0o700);
    internal(builder.create(path).await)
}

/// Copy an already verified backup into its already admitted fresh target.
/// Save immutable recovery authority before any working journal/native rewrite.
/// The outer action retains the selected manifest, runtime and offline admission.
pub async fn copy_restore_payload(
    backup_root: JsString,
    manifest: J,
    config: J,
    bindings: Arc<RestoreBindings>,
    job_launcher: PathBuf,
) -> IvyResult<f64> {
    internal(
        tokio::spawn(async move {
            let areas = project_storage::restored_storage_areas(
                &config,
                &captured(&manifest)?,
                &bindings.storage,
            )?;
            let total = total_bytes(&manifest)?;
            let roots = areas
                .iter()
                .map(|area| native(&area.path))
                .collect::<IvyResult<Vec<_>>>()?;
            let artifacts = native(&field(&config, "artifactRoot")?)?;
            let staging = native(&field(&config, "stagingRoot")?)?;
            let parent = |path: &Path| {
                path.parent()
                    .filter(|path| !path.as_os_str().is_empty())
                    .unwrap_or_else(|| {
                        if path.has_root() {
                            path
                        } else {
                            Path::new(".")
                        }
                    })
                    .to_owned()
            };
            for root in roots.iter().chain([&artifacts, &staging]) {
                let mut ancestor = parent(root);
                while !backup_files::exists(&ancestor).await? {
                    ancestor = parent(&ancestor);
                }
                storage_space::require_storage_space(ancestor, total + 64.0 * 1024.0 * 1024.0)
                    .await?;
            }
            let runtime_path = field(&config, "runtimeRoot")?;
            let runtime = native(&runtime_path)?;
            for root in [&runtime, &artifacts, &staging] {
                backup_files::private_directory(root.clone(), job_launcher.clone()).await?;
            }
            for (_, root) in areas
                .iter()
                .zip(&roots)
                .filter(|(area, _)| area.key != JsString::from("state"))
            {
                directory(root).await?;
            }
            let backup = native(&backup_root)?;
            for file in array(manifest.required("files")?)? {
                let path = field(file, "path")?;
                let destination = if !starts(&path, "state/") {
                    if !areas.iter().any(|area| {
                        area.key != JsString::from("state")
                            && path
                                .0
                                .starts_with(&concat(&[area.key.clone(), "/".into()]).0)
                    }) {
                        continue;
                    }
                    native(&layout::storage_file(
                        &config,
                        &path,
                        Some(&areas),
                        &bindings.storage,
                    )?)?
                } else {
                    let mut local = JsString(path.0[6..].to_vec());
                    if [
                        "restore.json",
                        "host.json",
                        "native-recovery.json",
                        "project-recovery.json",
                    ]
                    .iter()
                    .any(|name| local == JsString::from(*name))
                        || [
                            "restore-decisions/",
                            "recovery-original/",
                            "previous-recovery/",
                        ]
                        .iter()
                        .any(|prefix| starts(&local, prefix))
                    {
                        local = concat(&["previous-recovery/".into(), local]);
                    }
                    backup_files::relative_file_with_context(
                        &runtime_path,
                        &local,
                        &bindings.storage.paths,
                    )?;
                    runtime.join(native(&local)?)
                };
                let expected = BackupFile {
                    path: path.clone(),
                    hash: field(file, "hash")?.to_utf8()?,
                    bytes: number(file.required("bytes")?)? as u64,
                    mode: number(file.required("mode")?)? as u32,
                };
                backup_files::relative_file_with_context(
                    &backup_root,
                    &path,
                    &bindings.storage.paths,
                )?;
                backup_files::copy_verified(
                    backup.join(native(&path)?),
                    destination,
                    Some(expected),
                    true,
                )
                .await?;
            }
            let recovery = runtime.join("recovery-original");
            directory(&recovery).await?;
            for (source, destination) in [
                (
                    runtime.join("deployments.sqlite"),
                    recovery.join("deployments.sqlite"),
                ),
                (
                    backup.join("configuration.json"),
                    recovery.join("configuration.json"),
                ),
            ] {
                backup_files::copy_verified(source, destination, None, false).await?;
            }
            Ok(total)
        })
        .await,
    )?
}

/// Finish only after the existing maintenance/project and actual native owner
/// stages succeed. The native report is mandatory; no fallback is synthesized.
pub async fn publish_restored_host(
    config: J,
    manifest: J,
    native_recovery: J,
    maintenance_original_bytes: u64,
    created_at: JsString,
    started: Instant,
    bindings: Arc<RestoreBindings>,
) -> IvyResult<J> {
    internal(
        tokio::spawn(async move {
            reconcile_restored_journal(config.clone(), created_at).await?;
            for instance in array(config.required("instances")?)? {
                let data = native(&field(&layout::service_paths(&config, instance)?, "data")?)?;
                for name in ["health.json", "control.json"] {
                    let path = data.join(name);
                    if backup_files::exists(&path).await? {
                        internal(tokio::fs::remove_file(path).await)?;
                    }
                }
            }
            let mut roots = layout::host_storage_areas(&config, &bindings.storage)?
                .into_iter()
                .map(|area| native(&area.path))
                .collect::<IvyResult<Vec<_>>>()?;
            roots.extend([
                native(&field(&config, "artifactRoot")?)?,
                native(&field(&config, "stagingRoot")?)?,
            ]);
            for root in roots {
                backup_files::sync_directories(root).await?;
            }
            let config_path = layout::host_configuration_path(&config)?;
            config::atomic_json(config_path.clone(), config.clone()).await?;
            let mut bytes = total_bytes(&manifest)? + maintenance_original_bytes as f64;
            for instance in array(native_recovery.required("instances")?)? {
                for file in array(instance.required("originalFiles")?)? {
                    bytes += number(file.required("bytes")?)?;
                }
            }
            let identity = config.required("restoredFrom")?;
            let result = J::object([
                ("schemaVersion", J::Number(1.0)),
                ("restoreId", identity.required("restoreId")?.clone()),
                ("backupId", manifest.required("backupId")?.clone()),
                ("manifestHash", identity.required("manifestHash")?.clone()),
                (
                    "configPath",
                    J::String(node_path::from_native(&config_path)),
                ),
                ("state", "disabled".into()),
                ("restoredBytes", J::Number(bytes)),
                (
                    "elapsedMs",
                    J::Number((started.elapsed().as_secs_f64() * 1000.0).round()),
                ),
            ]);
            validate_host("RestoreResult", &result)?;
            Ok(result)
        })
        .await,
    )?
}
