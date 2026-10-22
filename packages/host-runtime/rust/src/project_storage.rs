//! Retained internal project roots, discovered only after offline ownership.
use crate::{
    IvyError, IvyJson, IvyResult, backup_files, config,
    layout::{self, StorageArea, StorageContext},
    node_path,
};
use ivy_contracts::{JsString, hash::hash_json, json};

fn text(value: &IvyJson) -> IvyResult<JsString> {
    value.as_js_string().cloned().ok_or_else(IvyError::internal)
}
fn concat(parts: &[JsString]) -> JsString {
    JsString(parts.iter().flat_map(|p| p.0.iter().copied()).collect())
}
pub async fn project_storage_areas(
    config: &IvyJson,
    context: &StorageContext,
) -> IvyResult<Vec<StorageArea>> {
    let mut areas = layout::host_storage_areas(config, context)?;
    let mut instances = config
        .required("instances")?
        .as_array()
        .ok_or_else(IvyError::internal)?
        .iter()
        .collect::<Vec<_>>();
    instances.sort_by(|a, b| {
        (context.compare)(
            a.get("instanceId")
                .and_then(IvyJson::as_js_string)
                .unwrap_or(&JsString::default()),
            b.get("instanceId")
                .and_then(IvyJson::as_js_string)
                .unwrap_or(&JsString::default()),
        )
    });
    for instance in instances {
        if instance.get("componentId") != Some(&"agent-manager".into()) {
            continue;
        }
        let data = text(layout::service_paths(config, instance)?.required("data")?)?;
        let path = node_path::join(
            context.paths.style,
            &[data, "project-backup-paths.json".into()],
        );
        if !backup_files::exists(&node_path::filesystem_path(&path)).await? {
            continue;
        }
        let paths = config::json_file(
            node_path::filesystem_path(&path),
            config::CONFIGURATION_BYTES,
        )
        .await?;
        let paths = paths
            .as_array()
            .filter(|a| {
                a.len() <= 256
                    && a.iter().all(|v| {
                        v.as_js_string()
                            .is_some_and(|s| node_path::is_absolute(context.paths.style, s))
                    })
            })
            .ok_or_else(|| {
                IvyError::new(
                    "backup_invalid",
                    "Invalid cached internal project inventory.",
                )
            })?;
        for path in paths {
            let cwd = text(path)?;
            if areas.iter().any(|a| context.paths.inside(&a.path, &cwd)) {
                continue;
            }
            areas.push(StorageArea {
                key: concat(&[
                    "retained-projects/".into(),
                    text(instance.required("instanceId")?)?,
                    "/".into(),
                    hash_json(path)?[7..].into(),
                ]),
                path: cwd,
            });
        }
    }
    Ok(areas)
}
pub fn restored_storage_areas(
    config: &IvyJson,
    captured: &[StorageArea],
    context: &StorageContext,
) -> IvyResult<Vec<StorageArea>> {
    let mut areas = layout::host_storage_areas(config, context)?;
    for area in captured
        .iter()
        .filter(|a| a.key.0.starts_with(&JsString::from("retained-projects/").0))
    {
        let parts = area.key.0.split(|c| *c == 47).collect::<Vec<_>>();
        let instance = parts.get(1).and_then(|id| {
            config.get("instances")?.as_array()?.iter().find(|v| {
                v.get("componentId") == Some(&"agent-manager".into())
                    && v.get("instanceId") == Some(&IvyJson::String(JsString(id.to_vec())))
            })
        });
        let hash = parts
            .get(2)
            .filter(|s| s.len() == 64 && s.iter().all(|c| matches!(*c,48..=57|97..=102)))
            .ok_or_else(|| {
                IvyError::new(
                    "backup_invalid",
                    "Invalid retained internal project identity.",
                )
            })?;
        let instance = instance.ok_or_else(|| {
            IvyError::new(
                "backup_invalid",
                "Invalid retained internal project identity.",
            )
        })?;
        let root = match instance.required("settings")?.get("internalProjectRoot") {
            Some(v) => json::try_js_string(v)?,
            None => "undefined".into(),
        };
        areas.push(StorageArea {
            key: area.key.clone(),
            path: node_path::join(
                context.paths.style,
                &[root, concat(&["restored-".into(), JsString(hash.to_vec())])],
            ),
        });
    }
    Ok(areas)
}
