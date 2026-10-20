//! Offline backup admission and Host-owned recovery stages. Native relocation
//! and final restore publication are assembled separately by the Host owner.
use crate::{
    IvyError, IvyJson as J, IvyResult, backup_files, codex_home, config,
    layout::{self, StorageArea, StorageContext},
    node_path::{self, Style},
    private_backup::NativeBackupPaths,
    project_storage, streaming_hash,
};
use ivy_contracts::{
    JsString,
    core_schema::{validate_host, validate_shared},
    hash::hash_json,
    json,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    sync::Arc,
};
mod target;
pub use target::target_configuration;
mod recovery;
pub use recovery::{
    hold_restored_bootstrap_maintenance, reconcile_restored_journal, relocate_projects,
};

/// Use one executing-account/path generation and the actual native ownership
/// policy. A production caller must supply the qualified locale comparator.
pub struct RestoreBindings {
    pub storage: StorageContext,
    pub environment: BTreeMap<JsString, JsString>,
    pub native_paths: Arc<dyn NativeBackupPaths>,
}
fn internal<T, E>(v: Result<T, E>) -> IvyResult<T> {
    v.map_err(|_| IvyError::internal())
}
fn require(ok: bool, code: &str, message: &str) -> IvyResult<()> {
    if ok {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn text(v: &J) -> IvyResult<JsString> {
    v.as_js_string().cloned().ok_or_else(IvyError::internal)
}
fn field(v: &J, k: &str) -> IvyResult<JsString> {
    text(v.required(k)?)
}
fn array(v: &J) -> IvyResult<&[J]> {
    v.as_array().ok_or_else(IvyError::internal)
}
fn truthy(v: &J) -> bool {
    match v {
        J::Null => false,
        J::Bool(v) => *v,
        J::Number(v) => *v != 0.0 && !v.is_nan(),
        J::String(v) => !v.0.is_empty(),
        _ => true,
    }
}
fn concat(v: &[JsString]) -> JsString {
    JsString(v.iter().flat_map(|v| v.0.iter().copied()).collect())
}
fn starts(v: &JsString, prefix: &str) -> bool {
    v.0.starts_with(&JsString::from(prefix).0)
}
fn join(v: &[JsString]) -> JsString {
    node_path::join(Style::native(), v)
}
fn native(v: &JsString) -> IvyResult<PathBuf> {
    internal(node_path::node_filesystem_path(v))
}
async fn exists(v: &JsString) -> IvyResult<bool> {
    backup_files::exists(&native(v)?).await
}
async fn json_file(v: &JsString, limit: usize) -> IvyResult<J> {
    config::json_file(native(v)?, limit).await
}
fn areas(manifest: &J, original: &J) -> IvyResult<Vec<StorageArea>> {
    if let Some(values) = manifest.get("storageAreas").filter(|v| !v.is_null()) {
        return array(values)?
            .iter()
            .map(|v| {
                Ok(StorageArea {
                    key: field(v, "key")?,
                    path: field(v, "path")?,
                })
            })
            .collect();
    }
    Ok(vec![StorageArea {
        key: "state".into(),
        path: field(original, "runtimeRoot")?,
    }])
}
fn home_context(b: &RestoreBindings) -> codex_home::HomeContext<'_> {
    codex_home::HomeContext {
        paths: &b.storage.paths,
        environment: &b.environment,
        user_home: None,
        cwd: &b.storage.paths.cwd,
        account_home: &b.storage.account_home,
    }
}
fn owned_home(config: &J, instance: &J, b: &RestoreBindings) -> IvyResult<bool> {
    Ok(codex_home::instance_owned_codex_home_json(
        instance.required("settings")?,
        &field(&layout::service_paths(config, instance)?, "data")?,
        &home_context(b),
    )?
    .is_some())
}
fn file_identity(files: &[J]) -> IvyResult<J> {
    let mut result = files
        .iter()
        .map(|f| {
            Ok(J::object([
                ("path", f.required("path")?.clone()),
                ("hash", f.required("hash")?.clone()),
                ("bytes", f.required("bytes")?.clone()),
            ]))
        })
        .collect::<IvyResult<Vec<_>>>()?;
    result.sort_by(|a, b| {
        a.get("path")
            .and_then(J::as_js_string)
            .cmp(&b.get("path").and_then(J::as_js_string))
    });
    Ok(J::Array(result))
}

/// Verify the selected immutable backup and all declared bytes before restore
/// target admission. No target directory or configuration is created here.
pub async fn verify_backup(
    directory: JsString,
    expected_hash: JsString,
    b: &RestoreBindings,
) -> IvyResult<J> {
    validate_shared("Hash", &J::String(expected_hash.clone()))?;
    let path = join(&[directory.clone(), "backup.json".into()]);
    let info = internal(tokio::fs::symlink_metadata(native(&path)?).await)?;
    require(
        info.is_file()
            && !info.file_type().is_symlink()
            && info.len() <= 12 * 1024 * 1024
            && !exists(&join(&[directory.clone(), "backup-failure.json".into()])).await?,
        "backup_invalid",
        "Backup publication is incomplete or invalid.",
    )?;
    require(
        JsString::from(streaming_hash::file_hash(native(&path)?).await?) == expected_hash,
        "backup_changed",
        "Backup manifest does not match the selected immutable identity.",
    )?;
    let manifest = json_file(&path, 12 * 1024 * 1024).await?;
    validate_host("BackupManifest", &manifest)?;
    let files = array(manifest.required("files")?)?;
    let names = files
        .iter()
        .map(|f| {
            field(f, "path").map(|v| {
                if cfg!(windows) {
                    node_path::lowercase(&v)
                } else {
                    v
                }
            })
        })
        .collect::<IvyResult<BTreeSet<_>>>()?;
    require(
        names.len() == files.len() && !files.is_empty(),
        "backup_invalid",
        "Backup file names must be distinct.",
    )?;
    for file in files {
        backup_files::relative_file(&directory, &field(file, "path")?)?;
    }
    let actual = backup_files::inventory_files(
        native(&directory)?,
        Some(Arc::new(|path| Ok(*path == JsString::from("backup.json")))),
    )
    .await?;
    require(
        hash_json(&file_identity(
            &actual.iter().map(|f| f.json()).collect::<Vec<_>>(),
        )?)? == hash_json(&file_identity(files)?)?,
        "backup_changed",
        "Backup payload is missing, extra or changed.",
    )?;
    let configuration = json_file(
        &join(&[directory.clone(), "configuration.json".into()]),
        config::CONFIGURATION_BYTES,
    )
    .await?;
    validate_host("HostConfig", &configuration)?;
    require(
        manifest.get("configurationHash") == Some(&hash_json(&configuration)?.into())
            && manifest.get("hostId") == configuration.get("hostId"),
        "backup_invalid",
        "Backup configuration identity differs from its manifest.",
    )?;
    let shared = manifest
        .get("sharedNativeHomes")
        .map(array)
        .transpose()?
        .unwrap_or(&[]);
    let mut shared_instances = Vec::new();
    for instance in array(configuration.required("instances")?)? {
        if instance.get("componentId") == Some(&"agent-manager".into())
            && (codex_home::uses_external_app_server_json(instance.required("settings")?)?
                || !owned_home(&configuration, instance, b)?)
        {
            shared_instances.push(instance);
        }
    }
    let mut valid = shared.len() == shared_instances.len();
    let mut seen = BTreeSet::new();
    for home in shared {
        valid &= seen.insert(field(home, "instanceId")?);
        let instance = shared_instances
            .iter()
            .find(|v| v.get("instanceId") == home.get("instanceId"));
        let path = field(home, "path")?;
        if instance.is_none()
            || !node_path::is_absolute(b.storage.paths.style, &path)
            || b.storage
                .paths
                .inside(&field(&configuration, "runtimeRoot")?, &path)
        {
            valid = false;
            continue;
        }
        let settings = instance.unwrap().required("settings")?;
        let connection = settings.get("appServer");
        let explicit = connection
            .filter(|v| v.get("mode") == Some(&"external-proxy".into()))
            .and_then(|v| v.get("expectedCodexHome"))
            .filter(|v| !v.is_null())
            .or_else(|| settings.get("codexHome").filter(|v| !v.is_null()))
            .or_else(|| settings.get("nativeHome").filter(|v| !v.is_null()));
        if let Some(explicit) = explicit {
            valid &= explicit.as_js_string().is_some_and(|v| {
                b.storage.paths.resolve(&[v.clone()]) == b.storage.paths.resolve(&[path])
            });
        }
    }
    require(
        valid,
        "backup_invalid",
        "Shared Codex home references do not match the captured instance ownership.",
    )?;
    let captured = areas(&manifest, &configuration)?;
    let base = J::Array(
        captured
            .iter()
            .filter(|a| !starts(&a.key, "retained-projects/"))
            .map(StorageArea::json)
            .collect(),
    );
    require(
        hash_json(&base)?
            == hash_json(&J::Array(
                layout::host_storage_areas(&configuration, &b.storage)?
                    .iter()
                    .map(StorageArea::json)
                    .collect(),
            ))?,
        "backup_invalid",
        "Captured service and project storage areas differ from the configuration.",
    )?;
    for (index, area) in captured
        .iter()
        .enumerate()
        .filter(|(_, a)| starts(&a.key, "retained-projects/"))
    {
        let parts = area.key.0.split(|c| *c == 47).collect::<Vec<_>>();
        let expected = hash_json(&J::String(area.path.clone()))?;
        require(
            parts.len() == 3
                && array(configuration.required("instances")?)?
                    .iter()
                    .any(|v| {
                        v.get("instanceId") == Some(&J::String(JsString(parts[1].to_vec())))
                            && v.get("componentId") == Some(&"agent-manager".into())
                    })
                && node_path::is_absolute(b.storage.paths.style, &area.path)
                && parts[2] == &JsString::from(&expected[7..]).0
                && captured.iter().enumerate().all(|(other, boundary)| {
                    other == index
                        || !b.storage.paths.inside(&boundary.path, &area.path)
                            && !b.storage.paths.inside(&area.path, &boundary.path)
                }),
            "backup_invalid",
            "Retained project storage identity is invalid or overlapping.",
        )?;
    }
    if let Some(ephemeral) = manifest.get("ephemeralNativePaths") {
        require(
            hash_json(ephemeral)?
                == hash_json(&J::Array(b.native_paths.exclusions(&configuration)?))?,
            "backup_invalid",
            "Native cache exclusions differ from the captured configuration.",
        )?;
        let mut valid = true;
        for entry in array(ephemeral)? {
            let path = field(entry, "path")?;
            let path = if starts(&path, "services/") {
                path
            } else {
                concat(&["state/".into(), path])
            };
            let prefix = concat(&[path.clone(), "/".into()]);
            for file in files {
                let file = field(file, "path")?;
                valid &= file != path && !file.0.starts_with(&prefix.0);
            }
        }
        require(
            valid,
            "backup_invalid",
            "Backup contains a declared ephemeral native cache.",
        )?;
    }
    Ok(manifest)
}
