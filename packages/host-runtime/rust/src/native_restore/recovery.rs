use super::{relocate_database, require};
use crate::{
    backup_files::{self, BackupFile, relative_file_with_context},
    codex_home::uses_external_app_server_json,
    config,
    layout::storage_path,
    native_backup_paths::NativeBackupPolicy,
    node_path::{self, Context},
    paths, streaming_hash,
};
use ivy_contracts::{
    IvyError, IvyJson as J, IvyResult, JsString, core_schema::validate_host, hash::hash_json,
};
use ivy_store::sqlite::{self, OpenOptions};
use std::{collections::BTreeMap, fs, path::Path, time::Duration};

fn internal<T, E>(value: Result<T, E>) -> IvyResult<T> {
    value.map_err(|_| IvyError::internal())
}
fn text(value: &J) -> IvyResult<&JsString> {
    value.as_js_string().ok_or_else(IvyError::internal)
}
fn field<'a>(value: &'a J, key: &str) -> IvyResult<&'a JsString> {
    text(value.required(key)?)
}
fn concat(parts: &[&JsString]) -> JsString {
    JsString(
        parts
            .iter()
            .flat_map(|part| part.0.iter().copied())
            .collect(),
    )
}
fn joined(paths: &Context, parts: &[JsString]) -> JsString {
    node_path::join(paths.style, parts)
}
fn record(value: &J) -> IvyResult<BackupFile> {
    Ok(BackupFile {
        path: field(value, "path")?.clone(),
        hash: value.required("hash")?.as_str()?,
        bytes: value
            .required("bytes")?
            .as_f64()
            .ok_or_else(IvyError::internal)? as u64,
        mode: value
            .required("mode")?
            .as_f64()
            .ok_or_else(IvyError::internal)? as u32,
    })
}

/// Called inside the fresh working database transaction, on its blocking worker.
/// Logical paths remain UTF-16; only filesystem arguments use Node's UTF-8 boundary.
pub fn mapped_file(
    raw: &JsString,
    scope: &str,
    old_home: &JsString,
    new_home: &JsString,
    prefix: &JsString,
    files: &BTreeMap<JsString, J>,
    context: &Context,
) -> IvyResult<Option<JsString>> {
    let absolute = node_path::is_absolute(context.style, raw);
    if scope == "optional-owned-file" && (!absolute || !context.inside(old_home, raw)) {
        return Ok(None);
    }
    require(
        absolute
            && context.inside(old_home, raw)
            && !raw.0.split(|c| *c == 47 || *c == 92).any(|p| p == [46, 46]),
        "backup_invalid",
        "Native rollout pointer must be absolute and inside its captured native home.",
    )?;
    let local = JsString(
        context
            .relative(old_home, raw)
            .0
            .into_iter()
            .map(|c| {
                if c == 92 && context.style == node_path::Style::Windows {
                    47
                } else {
                    c
                }
            })
            .collect(),
    );
    require(
        scope == "optional-owned-file"
            || local.0.starts_with(&JsString::from("sessions/").0)
            || local.0.starts_with(&JsString::from("archived_sessions/").0),
        "backup_invalid",
        "Native rollout pointer leaves its owned history namespace.",
    )?;
    let target = relative_file_with_context(new_home, &local, context)?;
    let physical = context.filesystem_path(&target);
    let key = concat(&[prefix, &"/".into(), &local]);
    if let Some(expected) = files.get(&key) {
        let metadata = internal(fs::symlink_metadata(&physical))?;
        // Keep the original short-circuit and observation order.
        let valid = metadata.is_file()
            && !metadata.file_type().is_symlink()
            && context.inside(
                &node_path::from_native(&internal(paths::realpath(
                    &context.filesystem_path(new_home),
                ))?),
                &node_path::from_native(&internal(paths::realpath(&physical))?),
            )
            && Some(metadata.len() as f64) == expected.get("bytes").and_then(J::as_f64)
            && expected.get("hash") == Some(&streaming_hash::hash_file(&physical)?.into());
        require(
            valid,
            "backup_changed",
            "Native pointer target differs from its captured regular file.",
        )?;
    } else {
        let absent = if scope == "required-rollout" {
            false
        } else {
            match fs::symlink_metadata(&physical) {
                Ok(_) => false,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
                Err(_) => return Err(IvyError::internal()),
            }
        };
        require(
            absent,
            "backup_invalid",
            "A native pointer has no captured target file.",
        )?;
    }
    Ok(Some(target))
}

fn state_database(name: &JsString) -> Option<(u32, &[u16])> {
    let rest = name.0.strip_prefix(JsString::from("state_").0.as_slice())?;
    let digits = rest.iter().take_while(|c| (48u16..=57).contains(*c)).count();
    if digits == 0 {
        return None;
    }
    let tail = &rest[digits..];
    if ![".sqlite", ".sqlite-wal", ".sqlite-shm"]
        .iter()
        .any(|s| tail == JsString::from(*s).0)
    {
        return None;
    }
    // Retain the spelling: state_05 is not state_5 in the original regexp.
    let version = if &rest[..digits] == [53] { 5 } else { 0 };
    Some((version, tail))
}
fn exact_version(value: &J) -> Option<String> {
    let value = value.as_js_string()?;
    if value.0.len() > 64 {
        return None;
    }
    let parts = value.0.split(|c| *c == 46).collect::<Vec<_>>();
    (parts.len() == 3
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.iter().all(|c| (48..=57).contains(c))))
    .then(|| String::from_utf16_lossy(&value.0))
}

/// Relocate only an activation-held restored working copy. Source and backup
/// roots are configuration inputs; this function never opens either database.
pub async fn relocate_native_storage(
    original: &J,
    config: &J,
    manifest: &J,
    distribution_root: &Path,
    policy: &NativeBackupPolicy,
) -> IvyResult<J> {
    let restored = config.get("restoredFrom");
    require(
        restored.is_some_and(|v| !v.is_null() && v.get("backupId") == manifest.get("backupId")),
        "target_conflict",
        "Native recovery requires its selected restored host.",
    )?;
    policy.exclusions(original)?;
    policy.exclusions(config)?;
    let storage = policy.storage();
    let paths = &storage.paths;
    let manifest_files = manifest
        .required("files")?
        .as_array()
        .ok_or_else(IvyError::internal)?;
    let mut files = BTreeMap::new();
    for file in manifest_files {
        files.insert(field(file, "path")?.clone(), file.clone());
    }
    let mut report = restored.expect("checked restored identity").clone();
    report.insert("schemaVersion", J::Number(1.0));
    report.insert("completedAt", "".into());
    let agents = config
        .required("instances")?
        .as_array()
        .ok_or_else(IvyError::internal)?
        .iter()
        .filter(|i| i.get("componentId") == Some(&"agent-manager".into()))
        .collect::<Vec<_>>();
    let mut instances = Vec::new();
    for instance in agents {
        let prior = original
            .required("instances")?
            .as_array()
            .ok_or_else(IvyError::internal)?
            .iter()
            .find(|v| v.get("instanceId") == instance.get("instanceId"));
        require(
            prior.is_some_and(|v| v.get("componentId") == Some(&"agent-manager".into())),
            "target_conflict",
            "Native recovery instance differs from its captured identity.",
        )?;
        let prior = prior.expect("checked original identity");
        let old_settings = prior.required("settings")?;
        let settings = instance.required("settings")?;
        if uses_external_app_server_json(old_settings)? {
            continue;
        }
        let Some(old_home) = policy.owned_home(original, prior)? else {
            continue;
        };
        let new_home = policy.owned_home(config, instance)?;
        require(
            new_home.is_some(),
            "target_conflict",
            "Owned native restore must retain a separate instance-owned home.",
        )?;
        let new_home = new_home.expect("checked owned home");
        let prefix = storage_path(config, &new_home, storage)?;
        require(
            prefix == storage_path(original, &old_home, storage)?
                && settings.get("nativeVersion") == old_settings.get("nativeVersion")
                && settings.get("nativeExecutableHash") == old_settings.get("nativeExecutableHash"),
            "target_conflict",
            "Native restore requires the captured relative home and exact native build.",
        )?;
        let prefix_slash = concat(&[&prefix, &"/".into()]);
        let database_files = manifest_files
            .iter()
            .filter_map(|file| {
                let path = file.get("path")?.as_js_string()?;
                let tail = path.0.strip_prefix(prefix_slash.0.as_slice())?;
                state_database(&JsString(tail.to_vec())).map(|(version, _)| (file, version))
            })
            .collect::<Vec<_>>();
        require(
            database_files
                .iter()
                .all(|(file, version)| file.get("bytes") == Some(&J::Number(0.0)) || *version == 5),
            "unsupported_storage",
            "Native home contains an unsupported state database.",
        )?;
        let database_key = concat(&[&prefix_slash, &"state_5.sqlite".into()]);
        if !files.contains_key(&database_key) {
            require(
                database_files
                    .iter()
                    .all(|(file, _)| file.get("bytes") == Some(&J::Number(0.0))),
                "backup_invalid",
                "Native state companions have no captured database.",
            )?;
            continue;
        }
        let version = old_settings
            .get("nativeVersion")
            .and_then(exact_version)
            .ok_or_else(|| {
                IvyError::new(
                    "unsupported_storage",
                    "Native recovery needs an exact supported storage layout.",
                )
            })?;
        let platform = if cfg!(windows) {
            "win32"
        } else {
            std::env::consts::OS
        };
        let arch = match std::env::consts::ARCH {
            "x86_64" => "x64",
            "aarch64" => "arm64",
            "x86" => "ia32",
            other => other,
        };
        let root = node_path::from_native(distribution_root);
        let directory = joined(
            paths,
            &[
                root,
                "dist/specs/native".into(),
                format!("codex-{version}").into(),
            ],
        );
        let platform_layout = joined(
            paths,
            &[
                directory.clone(),
                format!("storage.{platform}-{arch}.json").into(),
            ],
        );
        let layout_path = if backup_files::exists(&paths.filesystem_path(&platform_layout)).await? {
            platform_layout
        } else {
            joined(paths, &[directory, "storage.json".into()])
        };
        require(
            backup_files::exists(&paths.filesystem_path(&layout_path)).await?,
            "unsupported_storage",
            "The selected native build has no retained storage layout.",
        )?;
        let layout = config::json_file(
            paths.filesystem_path(&layout_path),
            config::CONFIGURATION_BYTES,
        )
        .await?;
        validate_host("NativeStorageLayout", &layout)?;
        require(
            layout.get("nativeVersion") == Some(&version.clone().into()),
            "unsupported_storage",
            "Native storage layout does not match its configured version.",
        )?;
        let mut original_files = Vec::new();
        for suffix in ["", "-wal", "-shm"] {
            let filename: JsString = format!("state_5.sqlite{suffix}").into();
            let key = concat(&[&prefix_slash, &filename]);
            let Some(captured) = files.get(&key) else {
                continue;
            };
            let recovery_path = concat(&[
                &"recovery-original/native/".into(),
                field(instance, "instanceId")?,
                &"/".into(),
                &filename,
            ]);
            let destination =
                relative_file_with_context(field(config, "runtimeRoot")?, &recovery_path, paths)?;
            let from = joined(paths, &[new_home.clone(), filename]);
            backup_files::copy_verified(
                node_path::to_native(&from),
                node_path::to_native(&destination),
                Some(record(captured)?),
                true,
            )
            .await?;
            let mut saved = captured.clone();
            saved.insert("path", J::String(recovery_path));
            original_files.push(saved);
        }
        let database = joined(paths, &[new_home.clone(), "state_5.sqlite".into()]);
        let physical = paths.filesystem_path(&database);
        let old_home = old_home.clone();
        let new_home = new_home.clone();
        let prefix = prefix.clone();
        let captured = files.clone();
        let context = Context {
            style: paths.style,
            cwd: paths.cwd.clone(),
        };
        let sql_layout = layout.clone();
        let (relocated, working_hash) = internal(
            tokio::task::spawn_blocking(move || {
                let db = internal(sqlite::open(
                    &physical,
                    OpenOptions {
                        busy_timeout: Duration::ZERO,
                        read_only: false,
                    },
                ))?;
                let result = relocate_database(db, &sql_layout, |raw, scope| {
                    mapped_file(
                        raw, scope, &old_home, &new_home, &prefix, &captured, &context,
                    )
                })?;
                let file = internal(
                    fs::OpenOptions::new()
                        .read(true)
                        .write(true)
                        .open(&physical),
                )?;
                backup_files::sync_file(file)?;
                Ok::<_, IvyError>((result, streaming_hash::hash_file(&physical)?))
            })
            .await,
        )??;
        instances.push(J::object([
            ("instanceId", instance.required("instanceId")?.clone()),
            ("nativeVersion", version.into()),
            ("layoutHash", hash_json(&layout)?.into()),
            ("database", "state_5.sqlite".into()),
            ("originalFiles", J::Array(original_files)),
            ("workingHash", working_hash.into()),
            ("logicalDataHash", relocated.logical_data_hash.into()),
            ("changes", J::Array(relocated.changes)),
        ]));
    }
    report.insert("instances", J::Array(instances));
    report.insert(
        "completedAt",
        chrono::Utc::now()
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
            .into(),
    );
    validate_host("NativeRecoveryReport", &report)?;
    let path = joined(
        paths,
        &[
            field(config, "runtimeRoot")?.clone(),
            "native-recovery.json".into(),
        ],
    );
    config::atomic_json(paths.filesystem_path(&path), report.clone()).await?;
    Ok(report)
}
