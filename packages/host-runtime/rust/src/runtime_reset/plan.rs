use super::*;
use crate::{
    artifact,
    journal::HostJournal,
    layout::{self, StorageContext},
    node_path, source,
};
use ivy_contracts::hash::{digest, hash_json};
use std::{fs, path::PathBuf, sync::Arc};

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
fn native(value: &JsString) -> IvyResult<PathBuf> {
    internal(node_path::node_filesystem_path(value))
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
                || (host.get("ssh").is_some_and(truthy)
                    && host.get("cliPath").is_some_and(truthy)
                    && node_path::is_absolute(storage.paths.style, &field(host, "cliPath")?)),
            "invalid_arguments",
            "Every remote reset host requires an SSH identity and absolute CLI path.",
        )?;
    }
    selected.ok_or_else(IvyError::internal)
}
async fn future(value: JsString) -> IvyResult<JsString> {
    internal(tokio::task::spawn_blocking(move || crate::paths::future_path(&value)).await)?
}
async fn tree_hash(path: JsString, storage: Arc<StorageContext>) -> IvyResult<J> {
    internal(
        tokio::task::spawn_blocking(move || {
            fn walk(
                path: &JsString,
                local: JsString,
                storage: &StorageContext,
                rows: &mut Vec<J>,
            ) -> IvyResult<()> {
                let info = internal(fs::symlink_metadata(native(path)?))?;
                require(
                    !info.file_type().is_symlink(),
                    "target_conflict",
                    "Preserved reset paths cannot traverse links.",
                )?;
                if info.is_file() {
                    rows.push(J::object([
                        ("path", J::String(local)),
                        (
                            "hash",
                            crate::streaming_hash::hash_file(&native(path)?)?.into(),
                        ),
                    ]));
                    return Ok(());
                }
                require(
                    info.is_dir(),
                    "target_conflict",
                    "Preserved reset paths must be regular files or directories.",
                )?;
                let mut entries = internal(
                    internal(fs::read_dir(native(path)?))?.collect::<Result<Vec<_>, _>>(),
                )?;
                entries.sort_by(|a, b| {
                    (storage.compare)(
                        &node_path::node_utf8(&a.file_name()),
                        &node_path::node_utf8(&b.file_name()),
                    )
                });
                for entry in entries {
                    require(
                        !internal(entry.file_type())?.is_symlink(),
                        "target_conflict",
                        "Preserved reset paths cannot contain links.",
                    )?;
                    let name = node_path::node_utf8(&entry.file_name());
                    let mut child = local.0.clone();
                    if !child.is_empty() {
                        child.push(47);
                    }
                    child.extend(&name.0);
                    walk(
                        &node_path::join(storage.paths.style, &[path.clone(), name]),
                        JsString(child),
                        storage,
                        rows,
                    )?;
                }
                Ok(())
            }
            match fs::symlink_metadata(native(&path)?) {
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
fn inside(storage: &StorageContext, a: &JsString, b: &JsString) -> bool {
    storage.paths.inside(a, b)
}
fn separate(storage: &StorageContext, a: &JsString, b: &JsString) -> bool {
    a != b && !inside(storage, a, b) && !inside(storage, b, a)
}
fn resolve(storage: &StorageContext, parts: &[JsString]) -> JsString {
    storage.paths.resolve(parts)
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
        let mut deleting = Vec::new();
        let mut preserving = Vec::new();
        let mut markers = Vec::new();
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
                        &storage,
                        &[if area == JsString::from("host") {
                            field(&config, "runtimeRoot")?
                        } else {
                            field(&layout::service_paths(&config, instance)?, &area.to_utf8()?)?
                        }],
                    );
                    let target = resolve(&storage, &[root.clone(), member.clone()]);
                    require(
                        if target == root {
                            member == JsString::from(".")
                                && ["work", "logs"].iter().any(|v| area == JsString::from(*v))
                        } else {
                            inside(&storage, &root, &target)
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
                            inside(&storage, &actual_root, &actual)
                        },
                        "target_conflict",
                        "Runtime reset target traverses a filesystem link.",
                    )?;
                    let mut value = J::object([
                        ("instanceId", instance.required("instanceId")?.clone()),
                        ("componentId", instance.required("componentId")?.clone()),
                        ("area", J::String(area)),
                        ("path", J::String(member)),
                        ("absolutePath", J::String(target.clone())),
                    ]);
                    if kind == "delete" {
                        deleting.push(value);
                    } else {
                        value.insert("checksum", tree_hash(target, storage.clone()).await?);
                        preserving.push(value);
                    }
                }
            }
            if let Some(marker) = layout.get("completionMarker").filter(|v| truthy(v)) {
                let root = resolve(
                    &storage,
                    &[field(&layout::service_paths(&config, instance)?, "data")?],
                );
                let target = resolve(&storage, &[root.clone(), text(marker)?]);
                require(
                    target != root && inside(&storage, &root, &target),
                    "target_conflict",
                    "Runtime reset completion marker leaves its instance data.",
                )?;
                require(
                    inside(
                        &storage,
                        &future(root).await?,
                        &future(target.clone()).await?,
                    ),
                    "target_conflict",
                    "Runtime reset completion marker traverses a filesystem link.",
                )?;
                markers.push(J::object([
                    ("instanceId", instance.required("instanceId")?.clone()),
                    ("componentId", instance.required("componentId")?.clone()),
                    ("path", marker.clone()),
                    ("absolutePath", J::String(target)),
                ]));
            }
        }
        let config_file = resolve(&storage, &[config_path]);
        let mut protected = vec![
            field(&config, "artifactRoot")?,
            field(&config, "stagingRoot")?,
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
                    protected.push(resolve(&storage, &[path.clone()]));
                }
            }
        }
        let paths = |values: &[J]| -> IvyResult<Vec<JsString>> {
            values.iter().map(|v| field(v, "absolutePath")).collect()
        };
        let delete_paths = paths(&deleting)?;
        let preserve_paths = paths(&preserving)?;
        require(
            delete_paths
                .iter()
                .all(|path| protected.iter().all(|root| separate(&storage, path, root))),
            "target_conflict",
            "Runtime reset target overlaps configuration, software, source or project storage.",
        )?;
        require(
            delete_paths.iter().all(|path| {
                preserve_paths
                    .iter()
                    .all(|saved| separate(&storage, path, saved))
            }),
            "target_conflict",
            "A preserved component setting overlaps a deletion target.",
        )?;
        let distinct = |values: Vec<JsString>| {
            let mut seen = BTreeSet::new();
            values
                .into_iter()
                .all(|path| seen.insert(node_path::lowercase(&path)))
        };
        require(
            distinct(delete_paths),
            "target_conflict",
            "Runtime reset deletion targets overlap between component declarations.",
        )?;
        require(
            distinct(paths(&markers)?),
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
            ("configPath", J::String(config_file)),
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
