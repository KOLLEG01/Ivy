//! Native backup ownership and references, using the executing host context.
use crate::{
    agent_backends::{AgentBackend, configured_backends},
    codex_home::{
        HomeContext, expected_server_home_json, instance_owned_codex_home_data_json,
        uses_external_app_server_json,
    },
    layout::{StorageContext, host_storage_areas, service_paths_for_style, storage_path},
    node_path,
};
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString};
use std::collections::BTreeMap;

const SHARED_HOME_CONFLICT: &str = "Shared Codex homes must not overlap owned host, service, project or software storage. Use explicit owned-stdio for an instance-owned home.";

/// One immutable executing-account/path/environment generation. The supplied
/// storage context includes the required production locale comparator.
pub struct NativeBackupPolicy {
    storage: StorageContext,
    environment: BTreeMap<JsString, JsString>,
    process_cwd: JsString,
}
impl NativeBackupPolicy {
    pub fn new(
        storage: StorageContext,
        environment: BTreeMap<JsString, JsString>,
        process_cwd: JsString,
    ) -> Self {
        Self {
            storage,
            environment,
            process_cwd,
        }
    }
    fn homes(&self) -> HomeContext<'_> {
        HomeContext {
            paths: &self.storage.paths,
            environment: &self.environment,
            user_home: None,
            cwd: &self.process_cwd,
            account_home: &self.storage.account_home,
        }
    }
    pub(crate) fn storage(&self) -> &StorageContext {
        &self.storage
    }
    pub(crate) fn owned_home(
        &self,
        config: &IvyJson,
        instance: &IvyJson,
        backend: &AgentBackend<'_>,
    ) -> IvyResult<Option<JsString>> {
        let paths = if backend.data_root.is_none() {
            Some(service_paths_for_style(
                config,
                instance,
                self.storage.paths.style,
            )?)
        } else {
            None
        };
        instance_owned_codex_home_data_json(
            backend.settings,
            backend
                .data_root
                .or_else(|| paths.as_ref().and_then(|paths| paths.get("data"))),
            &self.homes(),
        )
    }
    /// Exclude only the per-session helper cache of an explicitly owned home.
    pub fn exclusions(&self, config: &IvyJson) -> IvyResult<Vec<IvyJson>> {
        let mut rows = Vec::new();
        for instance in agents(config)? {
            let settings = instance.get("settings").ok_or_else(IvyError::internal)?;
            for backend in configured_backends(settings, None)? {
                let settings = backend.settings;
                if uses_external_app_server_json(settings)? {
                    continue;
                }
                let Some(home) = self.owned_home(config, instance, &backend)? else {
                    continue;
                };
                let path = node_path::join(
                    self.storage.paths.style,
                    &[home, "tmp".into(), "arg0".into()],
                );
                let path = storage_path(config, &path, &self.storage)?;
                let prefix: Vec<u16> = "state/".encode_utf16().collect();
                let path = if path.0.starts_with(&prefix) {
                    JsString(path.0[prefix.len()..].to_vec())
                } else {
                    path
                };
                let mut row = IvyJson::object([
                    ("instanceId", instance.required("instanceId")?.clone()),
                    ("path", IvyJson::String(path)),
                    ("kind", "codex-arg0-helper-cache".into()),
                ]);
                if let Some(identity) = &backend.identity {
                    row.insert("backend", identity.json());
                }
                rows.push(row);
            }
        }
        self.sort(&mut rows, "path")?;
        Ok(rows)
    }
    /// Shared homes are references only; this method performs no filesystem I/O.
    pub fn shared_homes(&self, config: &IvyJson) -> IvyResult<Vec<IvyJson>> {
        let mut rows = Vec::new();
        for instance in agents(config)? {
            let settings = instance.get("settings").ok_or_else(IvyError::internal)?;
            for backend in configured_backends(settings, None)? {
                let settings = backend.settings;
                if !uses_external_app_server_json(settings)? {
                    if self.owned_home(config, instance, &backend)?.is_some() {
                        continue;
                    }
                }
                let selected = expected_server_home_json(settings, &self.homes())?;
                let Some(home) = selected
                    .as_js_string()
                    .filter(|home| node_path::is_absolute(self.storage.paths.style, home))
                else {
                    return Err(IvyError::new("target_conflict", SHARED_HOME_CONFLICT));
                };
                // Preserve the original short-circuit: shared-home roots are built
                // only after the external/owned branch and absolute-string check.
                let areas = host_storage_areas(config, &self.storage)?;
                for root in areas.iter().map(|a| Some(&a.path)).chain([
                    config.get("artifactRoot").and_then(IvyJson::as_js_string),
                    config.get("stagingRoot").and_then(IvyJson::as_js_string),
                ]) {
                    let root = root.ok_or_else(IvyError::internal)?;
                    if self.storage.paths.inside(root, home)
                        || self.storage.paths.inside(home, root)
                    {
                        return Err(IvyError::new("target_conflict", SHARED_HOME_CONFLICT));
                    }
                }
                let mut row = IvyJson::object([
                    ("instanceId", instance.required("instanceId")?.clone()),
                    ("path", selected),
                    ("kind", "external-user-state".into()),
                ]);
                if let Some(identity) = &backend.identity {
                    row.insert("backend", identity.json());
                }
                rows.push(row);
            }
        }
        self.sort(&mut rows, "instanceId")?;
        Ok(rows)
    }
    fn sort(&self, rows: &mut [IvyJson], key: &str) -> IvyResult<()> {
        // HostConfig's IDs and generated paths are strings. Sorting remains
        // stable when the qualified locale comparator considers two equal.
        if rows.len() < 2 {
            return Ok(());
        }
        for row in rows.iter() {
            text(row.get(key))?;
        }
        rows.sort_by(|a, b| {
            (self.storage.compare)(
                a.get(key)
                    .and_then(IvyJson::as_js_string)
                    .expect("validated sort key"),
                b.get(key)
                    .and_then(IvyJson::as_js_string)
                    .expect("validated sort key"),
            )
        });
        Ok(())
    }
}
impl crate::native_backup_policy::NativeBackupPaths for NativeBackupPolicy {
    fn exclusions(&self, config: &IvyJson) -> IvyResult<Vec<IvyJson>> {
        NativeBackupPolicy::exclusions(self, config)
    }
    fn shared_homes(&self, config: &IvyJson) -> IvyResult<Vec<IvyJson>> {
        NativeBackupPolicy::shared_homes(self, config)
    }
}
fn text(value: Option<&IvyJson>) -> IvyResult<&JsString> {
    value
        .and_then(IvyJson::as_js_string)
        .ok_or_else(IvyError::internal)
}
fn agents(config: &IvyJson) -> IvyResult<Vec<&IvyJson>> {
    let instances = config
        .get("instances")
        .and_then(IvyJson::as_array)
        .ok_or_else(IvyError::internal)?;
    let mut agents = Vec::new();
    // Array.filter completes before flatMap starts in the original function.
    for instance in instances {
        if instance.is_null() {
            return Err(IvyError::internal());
        }
        if instance.get("componentId") == Some(&"agent-manager".into()) {
            agents.push(instance);
        }
    }
    Ok(agents)
}
