use super::*;
use crate::{
    artifact,
    journal::HostJournal,
    layout::{self, StorageContext},
    native_path, node_path, source,
};
use ivy_contracts::hash::{digest, hash_json};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

fn internal<T, E>(value: Result<T, E>) -> IvyResult<T> {
    value.map_err(|_| IvyError::internal())
}
fn require(value: bool, code: &str, message: &str) -> IvyResult<()> {
    if value {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn text(value: &J) -> IvyResult<JsString> {
    value.as_js_string().cloned().ok_or_else(IvyError::internal)
}
fn field(value: &J, key: &str) -> IvyResult<JsString> {
    text(value.required(key)?)
}
fn array(value: &J) -> IvyResult<&[J]> {
    value.as_array().ok_or_else(IvyError::internal)
}
fn deployment<'a>(config: &'a J, storage: &StorageContext) -> IvyResult<&'a J> {
    let selected = config.get("deployment").filter(|v| !v.is_null());
    let hosts = selected.and_then(|v| v.get("hosts")).and_then(J::as_array);
    require(
        hosts.is_some_and(|hosts| {
            !hosts.is_empty()
                && hosts
                    .iter()
                    .any(|host| host.get("hostId") == config.get("hostId"))
        }),
        "invalid_arguments",
        "Runtime reset requires an explicit deployment identity and host list containing this host.",
    )?;
    let hosts = hosts.unwrap();
    let mut ids = BTreeSet::new();
    require(
        hosts.iter().all(|host| {
            host.get("hostId")
                .and_then(J::as_js_string)
                .is_some_and(|id| ids.insert(id))
        }),
        "invalid_arguments",
        "Runtime reset deployment hosts must be unique.",
    )?;
    for host in hosts {
        require(
            host.get("hostId") == config.get("hostId")
                || (host
                    .get("ssh")
                    .and_then(J::as_js_string)
                    .is_some_and(|text| !text.0.is_empty())
                    && host
                        .get("cliPath")
                        .and_then(J::as_js_string)
                        .is_some_and(|text| !text.0.is_empty())
                    && node_path::is_absolute(storage.paths.style, &field(host, "cliPath")?)),
            "invalid_arguments",
            "Every remote reset host requires an SSH identity and absolute CLI path.",
        )?;
    }
    selected.ok_or_else(IvyError::internal)
}
async fn future(value: PathBuf) -> IvyResult<PathBuf> {
    internal(tokio::task::spawn_blocking(move || crate::paths::future_native_path(&value)).await)?
}
pub async fn tree_hash(path: JsString, storage: Arc<StorageContext>) -> IvyResult<J> {
    native_tree_hash(native_path::to_native(&path), storage).await
}
async fn native_tree_hash(path: PathBuf, storage: Arc<StorageContext>) -> IvyResult<J> {
    internal(
        tokio::task::spawn_blocking(move || {
            fn walk(
                path: &Path,
                local: JsString,
                storage: &StorageContext,
                rows: &mut Vec<J>,
            ) -> IvyResult<()> {
                let info = internal(fs::symlink_metadata(path))?;
                require(
                    !info.file_type().is_symlink(),
                    "target_conflict",
                    "Preserved reset paths cannot traverse links.",
                )?;
                if info.is_file() {
                    rows.push(J::object([
                        ("path", J::String(local)),
                        ("hash", crate::streaming_hash::hash_file(path)?.into()),
                    ]));
                    return Ok(());
                }
                require(
                    info.is_dir(),
                    "target_conflict",
                    "Preserved reset paths must be regular files or directories.",
                )?;
                let mut entries =
                    internal(internal(fs::read_dir(path))?.collect::<Result<Vec<_>, _>>())?;
                entries.sort_by(|a, b| {
                    (storage.compare)(
                        &native_path::filesystem_name(&a.file_name()),
                        &native_path::filesystem_name(&b.file_name()),
                    )
                });
                for entry in entries {
                    require(
                        !internal(entry.file_type())?.is_symlink(),
                        "target_conflict",
                        "Preserved reset paths cannot contain links.",
                    )?;
                    let native_name = entry.file_name();
                    // Checksum rows require a representable public path.
                    #[cfg(not(windows))]
                    if native_name.to_str().is_none() {
                        return Err(IvyError::internal());
                    }
                    let name = native_path::filesystem_name(&native_name);
                    let mut child = local.0.clone();
                    if !child.is_empty() {
                        child.push(47);
                    }
                    child.extend(&name.0);
                    walk(&entry.path(), JsString(child), storage, rows)?;
                }
                Ok(())
            }
            match fs::symlink_metadata(&path) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(J::Null),
                Err(_) => return Err(IvyError::internal()),
                Ok(_) => (),
            }
            let mut rows = Vec::new();
            walk(&path, JsString::default(), &storage, &mut rows)?;
            Ok(hash_json(&J::Array(rows))?.into())
        })
        .await,
    )?
}

fn separate(a: &Path, b: &Path) -> bool {
    !native_path::within(a, b) && !native_path::within(b, a)
}
fn resolve(base: &Path, path: &Path) -> PathBuf {
    let path = base.join(path);
    crate::paths::absolute(&path).unwrap_or_else(|_| native_path::lexical(&path))
}

/// Resolve fixed component declarations and preserved-tree hashes without
/// deleting data or running reset phases. The journal/manifest selection stays
/// identical to the installed-or-source fallback of the existing planner.
pub async fn local_runtime_reset_plan(
    config: J,
    config_path: JsString,
    distribution_root: PathBuf,
    storage: Arc<StorageContext>,
) -> IvyResult<J> {
    let selected = deployment(&config, &storage)?.clone();
    let journal = HostJournal::open(config.clone()).await?;
    let result = async {
        let cwd = native_path::to_native(&storage.paths.cwd);
        let mut deleting = Vec::new();
        let mut preserving = Vec::new();
        let mut markers = Vec::new();
        let mut delete_paths = Vec::new();
        let mut preserve_paths = Vec::new();
        let mut marker_paths = Vec::new();
        for instance in array(config.required("instances")?)? {
            let component = field(instance, "componentId")?;
            let manifest = match journal
                .installed(&field(instance, "instanceId")?.to_utf8()?)
                .await?
            {
                Some(installed) => {
                    journal
                        .manifest(&field(&installed, "candidateId")?.to_utf8()?)
                        .await?
                }
                None => {
                    let mut plan =
                        source::source_build_plan(distribution_root.clone(), &component.to_utf8()?)
                            .await?;
                    plan.insert(
                        "buildId",
                        digest(format!("uninstalled:{}", component.to_utf8()?)).into(),
                    );
                    artifact::release_manifest(&plan)?
                }
            };
            let layout = manifest
                .get("runtimeReset")
                .filter(|v| !v.is_null())
                .ok_or_else(|| {
                    IvyError::new_js(
                        "unsupported_storage".into(),
                        JsString(
                            [
                                JsString::from("Component ").0,
                                component.0.clone(),
                                JsString::from(" has no fixed runtime reset layout.").0,
                            ]
                            .concat(),
                        ),
                    )
                })?;
            for kind in ["delete", "preserve"] {
                for entry in array(layout.required(kind)?)? {
                    let area = field(entry, "area")?;
                    let member = field(entry, "path")?;
                    let root = resolve(
                        &cwd,
                        &native_path::to_native(&if area == JsString::from("host") {
                            field(&config, "runtimeRoot")?
                        } else {
                            field(&layout::service_paths(&config, instance)?, &area.to_utf8()?)?
                        }),
                    );
                    let target = resolve(&root, &native_path::to_native(&member));
                    require(
                        if target == root {
                            member == JsString::from(".")
                                && ["work", "logs"].iter().any(|v| area == JsString::from(*v))
                        } else {
                            native_path::within(&root, &target)
                        },
                        "target_conflict",
                        "Runtime reset target leaves its declared component area.",
                    )?;
                    let actual_root = future(root.clone()).await?;
                    let actual = future(target.clone()).await?;
                    require(
                        if actual == actual_root {
                            target == root
                        } else {
                            native_path::within(&actual_root, &actual)
                        },
                        "target_conflict",
                        "Runtime reset target traverses a filesystem link.",
                    )?;
                    let mut value = J::object([
                        ("instanceId", instance.required("instanceId")?.clone()),
                        ("componentId", instance.required("componentId")?.clone()),
                        ("area", J::String(area)),
                        ("path", J::String(member)),
                        ("absolutePath", J::String(native_path::from_native(&target))),
                    ]);
                    if kind == "delete" {
                        delete_paths.push(target);
                        deleting.push(value);
                    } else {
                        value.insert(
                            "checksum",
                            native_tree_hash(target.clone(), storage.clone()).await?,
                        );
                        preserve_paths.push(target);
                        preserving.push(value);
                    }
                }
            }
            if let Some(marker) = layout
                .get("completionMarker")
                .filter(|v| v.as_js_string().is_some_and(|text| !text.0.is_empty()))
            {
                let root = resolve(
                    &cwd,
                    &native_path::to_native(&field(
                        &layout::service_paths(&config, instance)?,
                        "data",
                    )?),
                );
                let target = resolve(&root, &native_path::to_native(&text(marker)?));
                require(
                    target != root && native_path::within(&root, &target),
                    "target_conflict",
                    "Runtime reset completion marker leaves its instance data.",
                )?;
                require(
                    native_path::within(&future(root).await?, &future(target.clone()).await?),
                    "target_conflict",
                    "Runtime reset completion marker traverses a filesystem link.",
                )?;
                markers.push(J::object([
                    ("instanceId", instance.required("instanceId")?.clone()),
                    ("componentId", instance.required("componentId")?.clone()),
                    ("path", marker.clone()),
                    ("absolutePath", J::String(native_path::from_native(&target))),
                ]));
                marker_paths.push(target);
            }
        }
        let config_file = resolve(&cwd, &native_path::to_native(&config_path));
        let mut protected = vec![
            resolve(
                &cwd,
                &native_path::to_native(&field(&config, "artifactRoot")?),
            ),
            resolve(
                &cwd,
                &native_path::to_native(&field(&config, "stagingRoot")?),
            ),
            config_file.clone(),
        ];
        for instance in array(config.required("instances")?)?
            .iter()
            .filter(|v| v.get("componentId") == Some(&"agent-manager".into()))
        {
            for key in ["projectRoot", "internalProjectRoot"] {
                if let Some(path) = instance
                    .required("settings")?
                    .get(key)
                    .and_then(J::as_js_string)
                {
                    protected.push(resolve(&cwd, &native_path::to_native(path)));
                }
            }
        }
        require(
            delete_paths
                .iter()
                .all(|path| protected.iter().all(|root| separate(path, root))),
            "target_conflict",
            "Runtime reset target overlaps configuration, software, source or project storage.",
        )?;
        require(
            delete_paths
                .iter()
                .all(|path| preserve_paths.iter().all(|saved| separate(path, saved))),
            "target_conflict",
            "A preserved component setting overlaps a deletion target.",
        )?;
        let distinct = |values: &[PathBuf]| {
            let mut seen = BTreeSet::new();
            // Preserve the public duplicate-identity folding policy.
            values
                .iter()
                .all(|path| seen.insert(node_path::lowercase(&native_path::from_native(path))))
        };
        require(
            distinct(&delete_paths),
            "target_conflict",
            "Runtime reset deletion targets overlap between component declarations.",
        )?;
        require(
            distinct(&marker_paths),
            "target_conflict",
            "Runtime reset completion markers overlap between component declarations.",
        )?;
        let instances = array(config.required("instances")?)?
            .iter()
            .map(|instance| {
                Ok(J::object([
                    ("instanceId", instance.required("instanceId")?.clone()),
                    ("componentId", instance.required("componentId")?.clone()),
                    ("enabled", instance.required("enabled")?.clone()),
                ]))
            })
            .collect::<IvyResult<Vec<_>>>()?;
        Ok(J::object([
            ("schemaVersion", J::Number(1.0)),
            ("deploymentIdentity", selected.required("identity")?.clone()),
            ("hostId", config.required("hostId")?.clone()),
            (
                "configPath",
                J::String(native_path::from_native(&config_file)),
            ),
            ("instances", J::Array(instances)),
            (
                "scopeRootIds",
                J::Array(
                    configured_scope_root_ids(&config)?
                        .into_iter()
                        .map(J::String)
                        .collect(),
                ),
            ),
            ("delete", J::Array(deleting)),
            ("preserve", J::Array(preserving)),
            ("completionMarkers", J::Array(markers)),
        ]))
    }
    .await;
    journal.close().await?;
    result
}
