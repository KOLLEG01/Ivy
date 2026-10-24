use super::*;
use crate::{native_path, paths};
use std::path::Path;

async fn future(path: PathBuf) -> IvyResult<PathBuf> {
    internal(tokio::task::spawn_blocking(move || paths::future_native_path(&path)).await)?
}
fn identities(config: &J, b: &RestoreBindings) -> IvyResult<J> {
    let mut values = array(config.required("instances")?)?
        .iter()
        .map(|v| {
            Ok(J::object([
                ("instanceId", v.required("instanceId")?.clone()),
                ("componentId", v.required("componentId")?.clone()),
                ("serviceNodeId", v.required("serviceNodeId")?.clone()),
            ]))
        })
        .collect::<IvyResult<Vec<_>>>()?;
    values.sort_by(|a, bv| {
        (b.storage.compare)(
            a.get("instanceId").and_then(J::as_js_string).unwrap(),
            bv.get("instanceId").and_then(J::as_js_string).unwrap(),
        )
    });
    Ok(J::Array(values))
}
fn separate(a: &Path, v: &Path) -> bool {
    !native_path::within(a, v) && !native_path::within(v, a)
}
async fn futures(values: Vec<JsString>) -> IvyResult<Vec<PathBuf>> {
    let mut result = Vec::with_capacity(values.len());
    for value in values {
        result.push(future(native_path::to_native(&value)).await?);
    }
    Ok(result)
}
/// Admit the original host identity into fresh, disjoint, disabled target roots.
/// This performs prerequisite/path checks only; publication belongs to restore.
pub async fn target_configuration(
    template_path: JsString,
    backup_root: JsString,
    manifest: &J,
    manifest_hash: JsString,
    restore_id: JsString,
    b: &RestoreBindings,
) -> IvyResult<J> {
    let original = json_file(
        &join(&[backup_root.clone(), "configuration.json".into()]),
        config::CONFIGURATION_BYTES,
    )
    .await?;
    let raw = json_file(&template_path, config::CONFIGURATION_BYTES).await?;
    validate_host("HostConfigInput", &raw)?;
    let input = layout::resolve_host_configuration(
        &raw,
        &b.storage.paths.resolve(&[template_path.clone()]),
        &b.environment,
        b.storage.paths.style,
        &b.storage.account_home,
    )?;
    validate_host("HostConfig", &input)?;
    require(
        !input
            .get("restoredFrom")
            .is_some_and(|value| !value.is_null())
            && input.get("hostId") == manifest.get("hostId")
            && array(input.required("instances")?)?
                .iter()
                .all(|v| v.get("enabled") != Some(&J::Bool(true))),
        "target_conflict",
        "Restore needs the original host identity, a fresh configuration and every instance disabled.",
    )?;
    require(
        hash_json(&identities(&input, b)?)? == hash_json(&identities(&original, b)?)?
            && input.get("publicBaseUrl") != original.get("publicBaseUrl"),
        "target_conflict",
        "Isolated restore retains instance identities and needs a distinct explicit Hive endpoint.",
    )?;
    require(
        hash_json(input.required("executables")?)? == hash_json(original.required("executables")?)?,
        "target_conflict",
        "This restore requires its recorded host executable mappings.",
    )?;
    for prerequisite in array(manifest.required("externalFiles")?)? {
        let path = field(prerequisite, "path")?;
        require(
            node_path::is_absolute(b.storage.paths.style, &path)
                && Some(&J::from(streaming_hash::file_hash(native(&path)?).await?))
                    == prerequisite.get("hash"),
            "runtime_changed",
            "A recorded runtime/native executable prerequisite is missing or changed.",
        )?;
    }
    let roots = ["runtimeRoot", "artifactRoot", "stagingRoot"]
        .iter()
        .map(|k| field(&input, k))
        .collect::<IvyResult<Vec<_>>>()?;
    let source_areas = areas(manifest, &original)?;
    let target_areas = project_storage::restored_storage_areas(&input, &source_areas, &b.storage)?;
    let target_config = layout::host_configuration_path(&input)?;
    let actual_roots = futures(roots.clone()).await?;
    let actual_areas = futures(target_areas.iter().map(|a| a.path.clone()).collect()).await?;
    let mut protected = source_areas
        .iter()
        .map(|a| a.path.clone())
        .collect::<Vec<_>>();
    protected.extend([
        field(&original, "artifactRoot")?,
        field(&original, "stagingRoot")?,
        backup_root,
    ]);
    protected.extend(
        b.native_paths
            .shared_homes(&input)?
            .iter()
            .map(|v| field(v, "path"))
            .collect::<IvyResult<Vec<_>>>()?,
    );
    let protected = futures(protected).await?;
    let actual_config = future(target_config.clone()).await?;
    require(
        actual_config != future(layout::host_configuration_path(&original)?).await?
            && protected
                .iter()
                .all(|p| !native_path::within(p, &actual_config))
            && actual_roots
                .iter()
                .skip(1)
                .chain(actual_areas.iter().skip(1))
                .all(|p| !native_path::within(p, &actual_config))
            && (target_config
                == native_path::to_native(&b.storage.paths.resolve(&[template_path]))
                || !backup_files::exists(&target_config).await?),
        "target_conflict",
        "Restore configuration must be a fresh target, never the original user configuration.",
    )?;
    require(
        hash_json(&J::Array(
            source_areas
                .iter()
                .map(|a| J::String(a.key.clone()))
                .collect(),
        ))? == hash_json(&J::Array(
            target_areas
                .iter()
                .map(|a| J::String(a.key.clone()))
                .collect(),
        ))?,
        "target_conflict",
        "Restore must explicitly preserve the captured service/project layout.",
    )?;
    for area in target_areas
        .iter()
        .filter(|a| a.key != JsString::from("state"))
    {
        let path = native_path::to_native(&area.path);
        let actual = future(path.clone()).await?;
        require(
            path.is_absolute()
                && !backup_files::exists(&path).await?
                && protected
                    .iter()
                    .chain(actual_roots.iter())
                    .all(|root| separate(root, &actual)),
            "target_conflict",
            "Restored service and project areas must be fresh, disjoint and outside original data.",
        )?;
    }
    let base = actual_areas
        .iter()
        .enumerate()
        .filter(|(i, _)| !starts(&target_areas[*i].key, "retained-projects/"))
        .map(|(_, p)| p)
        .collect::<Vec<_>>();
    require(
        base.iter().enumerate().all(|(i, a)| {
            base.iter()
                .enumerate()
                .all(|(j, other)| i == j || !native_path::within(a, other))
        }),
        "target_conflict",
        "Restore storage areas overlap.",
    )?;
    let shared = b.native_paths.shared_homes(&input)?;
    require(
        hash_json(&J::Array(shared))?
            == hash_json(
                manifest
                    .get("sharedNativeHomes")
                    .unwrap_or(&J::Array(vec![])),
            )?,
        "target_conflict",
        "Shared Codex homes are external prerequisites and must retain their captured assignment; restore never copies or relocates their data.",
    )?;
    for root in &roots {
        let path = native_path::to_native(root);
        require(
            path.is_absolute()
                && !backup_files::exists(&path).await?
                && backup_files::exists(path.parent().unwrap_or(&path)).await?,
            "target_conflict",
            "Restore target roots must be fresh absolute directories with existing parents.",
        )?;
        let actual = future(path).await?;
        require(
            protected.iter().all(|p| separate(p, &actual))
                && actual_roots
                    .iter()
                    .all(|other| *other == actual || separate(other, &actual)),
            "target_conflict",
            "Restore roots overlap each other, the backup or the source installation.",
        )?;
    }
    require(
        roots
            .iter()
            .map(|r| node_path::lowercase(&b.storage.paths.resolve(&[r.clone()])))
            .collect::<BTreeSet<_>>()
            .len()
            == roots.len(),
        "target_conflict",
        "Restore roots must be distinct.",
    )?;
    for instance in array(input.required("instances")?)? {
        let data = field(&layout::service_paths(&input, instance)?, "data")?;
        if instance.get("componentId") == Some(&"agent-manager".into())
            && !manifest
                .get("sharedNativeHomes")
                .map(array)
                .transpose()?
                .unwrap_or(&[])
                .iter()
                .any(|h| h.get("instanceId") == instance.get("instanceId"))
        {
            require(
                owned_home(&input, instance, b)?,
                "target_conflict",
                "Restored owned native home must be explicitly bound inside its new instance data.",
            )?;
        }
        if ["host-executor", "service-manager"]
            .iter()
            .any(|v| instance.get("componentId") == Some(&(*v).into()))
        {
            require(
                instance.required("settings")?.get("hostConfigPath")
                    == Some(&J::String(native_path::from_native(&target_config))),
                "target_conflict",
                "Restored management components must explicitly reference the new host configuration.",
            )?;
        }
        if instance.get("componentId") == Some(&"hive".into()) {
            let settings = instance.required("settings")?;
            validate_host("HiveSettings", settings)?;
            let prior = array(original.required("instances")?)?
                .iter()
                .find(|v| v.get("instanceId") == instance.get("instanceId"))
                .ok_or_else(IvyError::internal)?;
            if instance.get("engine") == Some(&"process".into())
                && prior.get("engine") == Some(&"process".into())
            {
                require(
                    settings.get("listenPort") != prior.required("settings")?.get("listenPort"),
                    "target_conflict",
                    "Isolated Hive process must use a different listener port from its source installation.",
                )?;
            }
            if instance.get("engine") == Some(&"docker".into()) {
                let valid = if let Some(docker) = instance
                    .get("docker")
                    .filter(|value| value.as_object().is_some())
                {
                    array(docker.required("ports")?)?.iter().all(|p| {
                        !prior
                            .get("docker")
                            .and_then(|d| d.get("ports"))
                            .and_then(J::as_array)
                            .is_some_and(|ports| {
                                ports.iter().any(|old| old.get("port") == p.get("port"))
                            })
                    })
                } else {
                    false
                };
                require(
                    valid,
                    "target_conflict",
                    "Isolated Hive container must use different host listener ports.",
                )?;
            }
            let directory = field(settings.required("backup")?, "directory")?;
            require(
                b.storage
                    .paths
                    .inside(&field(&input, "runtimeRoot")?, &directory)
                    && !b.storage.paths.inside(&data, &directory),
                "target_conflict",
                "Restored Hive backup directory must be separate owned runtime storage.",
            )?;
        }
    }
    let mut result = input;
    result.insert(
        "restoredFrom",
        J::object([
            ("backupId", manifest.required("backupId")?.clone()),
            ("manifestHash", J::String(manifest_hash)),
            ("restoreId", J::String(restore_id)),
        ]),
    );
    Ok(result)
}
