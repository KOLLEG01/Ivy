use super::*;

/// One executing-account/path generation and the shared locale comparator.
pub struct StorageContext {
    pub paths: node_path::Context,
    pub account_home: JsString,
    pub compare: std::sync::Arc<dyn Fn(&JsString, &JsString) -> std::cmp::Ordering + Send + Sync>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StorageArea {
    pub key: JsString,
    pub path: JsString,
}
impl StorageArea {
    pub fn json(&self) -> IvyJson {
        IvyJson::object([
            ("key", IvyJson::String(self.key.clone())),
            ("path", IvyJson::String(self.path.clone())),
        ])
    }
}
pub(crate) fn concat(parts: &[JsString]) -> JsString {
    JsString(parts.iter().flat_map(|p| p.0.iter().copied()).collect())
}
fn contains(context: &StorageContext, root: &JsString, path: &JsString) -> bool {
    let local = context.paths.relative(root, path);
    // hostStorageAreas deliberately uses startsWith('..'), unlike inside().
    !local.0.starts_with(&[46, 46]) && !node_path::is_absolute(context.paths.style, &local)
}
pub fn host_storage_areas(
    config: &IvyJson,
    context: &StorageContext,
) -> IvyResult<Vec<StorageArea>> {
    let mut areas = vec![StorageArea {
        key: "state".into(),
        path: text(config.required("runtimeRoot")?)?,
    }];
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
        let id = text(instance.required("instanceId")?)?;
        let mut roots = instance
            .get("storageRoots")
            .map(|value| value.as_object().ok_or_else(IvyError::internal))
            .transpose()?
            .unwrap_or(&[])
            .iter()
            .collect::<Vec<_>>();
        roots.sort_by(|(a, _), (b, _)| (context.compare)(a, b));
        for (name, value) in roots {
            let path = text(value)?;
            if !areas.iter().any(|a| contains(context, &a.path, &path)) {
                areas.push(StorageArea {
                    key: concat(&[
                        "services/".into(),
                        id.clone(),
                        "/storage/".into(),
                        name.clone(),
                    ]),
                    path,
                });
            }
        }
        for (kind, value) in service_paths_for_style(config, instance, context.paths.style)?
            .as_object()
            .ok_or_else(IvyError::internal)?
        {
            let path = text(value)?;
            if !areas.iter().any(|a| contains(context, &a.path, &path)) {
                areas.push(StorageArea {
                    key: concat(&["services/".into(), id.clone(), "/".into(), kind.clone()]),
                    path,
                });
            }
        }
        if instance.get("componentId") == Some(&"agent-manager".into()) {
            let path = instance
                .required("settings")?
                .get("internalProjectRoot")
                .and_then(IvyJson::as_js_string)
                .cloned()
                .unwrap_or_else(|| {
                    node_path::join(
                        context.paths.style,
                        &[
                            context.account_home.clone(),
                            ".ivy".into(),
                            "codex-projects".into(),
                        ],
                    )
                });
            if !areas.iter().any(|a| contains(context, &a.path, &path)) {
                areas.push(StorageArea {
                    key: concat(&["projects/".into(), id]),
                    path,
                });
            }
        }
    }
    Ok(areas)
}
pub fn storage_path(
    config: &IvyJson,
    path: &JsString,
    context: &StorageContext,
) -> IvyResult<JsString> {
    let area = host_storage_areas(config, context)?
        .into_iter()
        .find(|area| contains(context, &area.path, path))
        .ok_or_else(|| IvyError::new("target_conflict", "Path is not in captured host storage."))?;
    let local = context.paths.relative(&area.path, path);
    let local = JsString(
        local
            .0
            .into_iter()
            .map(|c| if c == 92 { 47 } else { c })
            .collect(),
    );
    Ok(concat(&[area.key, "/".into(), local]))
}
pub fn storage_file(
    config: &IvyJson,
    key: &JsString,
    areas: Option<&[StorageArea]>,
    context: &StorageContext,
) -> IvyResult<JsString> {
    let owned;
    let areas = match areas {
        Some(areas) => areas,
        None => {
            owned = host_storage_areas(config, context)?;
            &owned
        }
    };
    let area = areas
        .iter()
        .find(|area| {
            key.0
                .starts_with(&concat(&[area.key.clone(), "/".into()]).0)
        })
        .ok_or_else(|| IvyError::new("target_conflict", "Unknown host storage area."))?;
    let local = JsString(key.0[area.key.0.len() + 1..].to_vec());
    if local.0.is_empty()
        || local
            .0
            .split(|c| *c == 47 || *c == 92)
            .any(|p| p == [46, 46])
        || node_path::is_absolute(context.paths.style, &local)
    {
        return Err(IvyError::new(
            "target_conflict",
            "Storage member leaves its area.",
        ));
    }
    Ok(node_path::join(
        context.paths.style,
        &[area.path.clone(), local],
    ))
}
