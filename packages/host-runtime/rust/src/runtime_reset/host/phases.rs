//! Held bootstrap and release, including the original durable re-hold on error.
use super::*;
use crate::{
    filesystem, journal::HostJournal, linux_resources, node_runtime::NodeRuntime,
    runtime_maintenance,
};
use ivy_contracts::hash::hash_json;
use std::collections::BTreeSet;

fn property<'a>(value: &'a J, key: &str) -> IvyResult<&'a J> {
    value.get(key).ok_or_else(IvyError::internal)
}
fn enabled(record: &J) -> IvyResult<BTreeSet<JsString>> {
    let mut ids = BTreeSet::new();
    for value in property(record, "saved")?
        .as_array()
        .ok_or_else(IvyError::internal)?
    {
        if value.is_null() {
            return Err(IvyError::internal());
        }
        if value.get("enabled") == Some(&J::Bool(true)) {
            if let Some(id) = value.get("instanceId").and_then(J::as_js_string) {
                ids.insert(id.clone());
            }
        }
    }
    Ok(ids)
}
fn selected(instance: &J, enabled: &BTreeSet<JsString>) -> bool {
    instance
        .get("instanceId")
        .and_then(J::as_js_string)
        .is_some_and(|id| enabled.contains(id))
}
fn hive(config: &J, enabled: &BTreeSet<JsString>) -> IvyResult<Option<J>> {
    Ok(instances(config)?
        .iter()
        .find(|v| {
            v.get("componentId") == Some(&"hive".into())
                && v.get("engine") == Some(&"docker".into())
                && selected(v, enabled)
        })
        .cloned())
}
async fn exists(path: PathBuf) -> IvyResult<bool> {
    match tokio::fs::symlink_metadata(node_path::filesystem_path(&node_path::from_native(&path)))
        .await
    {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err(IvyError::internal()),
    }
}
async fn record(config: &J) -> IvyResult<J> {
    let record = config::json_file(reset_record_path(config)?, config::CONFIGURATION_BYTES).await?;
    if record.is_null() {
        return Err(IvyError::internal());
    }
    Ok(record)
}
async fn running_target(journal: &HostJournal, instance: &J, message: &str) -> IvyResult<J> {
    // Node's SQLite text binding replaces lone UTF-16 surrogates. Convert only
    // the lookup key; keep the exact configured/retained JSON identity intact.
    let target = journal
        .target(&String::from_utf16_lossy(&field(instance, "instanceId")?.0))
        .await?;
    require(
        target
            .as_ref()
            .is_some_and(|v| v.get("desired") == Some(&"running".into())),
        "storage_invalid",
        message,
    )?;
    target.ok_or_else(IvyError::internal)
}
async fn runtime(target: &J, node: Option<&NodeRuntime>) -> IvyResult<J> {
    Ok(config::instance_config(
        node_path::node_filesystem_path(
            property(target, "configPath")?
                .as_js_string()
                .ok_or_else(IvyError::internal)?,
        )
        .map_err(|_| IvyError::internal())?,
        node,
    )
    .await?
    .json()
    .clone())
}

impl ResetHost {
    /// Only intentional JavaScript workloads need the caller's Node capability.
    pub async fn bootstrap(
        &self,
        config: J,
        reset_id: JsString,
        runtime_epoch: JsString,
        node: impl Into<Option<NodeRuntime>>,
    ) -> IvyResult<J> {
        let node = node.into();
        let inner = self.0.clone();
        internal(
            tokio::spawn(async move {
                inner.require_administrator().await?;
                inner.bootstrap(config, reset_id, runtime_epoch, node).await
            })
            .await,
        )?
    }
    pub async fn release(
        &self,
        config: J,
        reset_id: JsString,
        runtime_epoch: JsString,
        node: impl Into<Option<NodeRuntime>>,
    ) -> IvyResult<J> {
        let node = node.into();
        let inner = self.0.clone();
        internal(
            tokio::spawn(async move {
                inner.require_administrator().await?;
                inner.release(config, reset_id, runtime_epoch, node).await
            })
            .await,
        )?
    }
}
impl Inner {
    pub(super) async fn bootstrap(
        self: Arc<Self>,
        config: J,
        reset_id: JsString,
        runtime_epoch: JsString,
        node: Option<NodeRuntime>,
    ) -> IvyResult<J> {
        let record = record(&config).await?;
        // Admit the retained identity before reading its maintenance marker.
        require(
            record.get("resetId") == Some(&J::String(reset_id.clone()))
                && record.get("runtimeEpoch") == Some(&J::String(runtime_epoch.clone()))
                && record.get("phase") == Some(&"complete".into())
                && exists(runtime_maintenance::runtime_reset_marker(&config)?).await?,
            "mutation_conflict",
            "Runtime reset host is not held for bootstrap.",
        )?;
        let enabled = enabled(&record)?;
        let Some(instance) = hive(&config, &enabled)? else {
            return Ok(record);
        };
        let journal = HostJournal::open(config.clone()).await?;
        let result = async {
            let target = running_target(
                &journal,
                &instance,
                "Enabled Hive reset target was not restored.",
            )
            .await?;
            let id = field(&target, "candidateId")?.to_utf8()?;
            let candidate = journal.candidate(&id).await?;
            let manifest = journal.manifest(&id).await?;
            let runtime = runtime(&target, node.as_ref()).await?;
            self.docker
                .stop_container(
                    config.clone(),
                    field(&instance, "instanceId")?,
                    number(property(property(&manifest, "shutdown")?, "timeoutMs")?)?,
                    None,
                )
                .await?;
            config::atomic_json(
                runtime_maintenance::hive_reset_bootstrap_marker(
                    &node_path::node_filesystem_path(
                        property(&runtime, "dataRoot")?
                            .as_js_string()
                            .ok_or_else(IvyError::internal)?,
                    )
                    .map_err(|_| IvyError::internal())?,
                ),
                J::object([
                    ("schemaVersion", J::Number(1.0)),
                    ("resetId", J::String(reset_id)),
                    ("runtimeEpoch", J::String(runtime_epoch)),
                    ("phase", "bootstrap".into()),
                    (
                        "publisherCredentialDigest",
                        property(&record, "publisherCredentialDigest")?.clone(),
                    ),
                ]),
            )
            .await?;
            let managed = self
                .docker
                .start_container(
                    config, instance, target, runtime, candidate, manifest, true, None,
                )
                .await?;
            managed.verify().await?;
            managed.dispose();
            Ok(record)
        }
        .await;
        journal.close().await?;
        result
    }
    pub(super) async fn release(
        self: Arc<Self>,
        config: J,
        reset_id: JsString,
        runtime_epoch: JsString,
        node: Option<NodeRuntime>,
    ) -> IvyResult<J> {
        let record = record(&config).await?;
        require(
            record.get("resetId") == Some(&J::String(reset_id.clone()))
                && record.get("runtimeEpoch") == Some(&J::String(runtime_epoch.clone()))
                && ["complete", "released"]
                    .iter()
                    .any(|v| record.get("phase") == Some(&J::from(*v))),
            "mutation_conflict",
            "Runtime reset host is not jointly complete for this identity.",
        )?;
        if record.get("phase") == Some(&"released".into()) {
            return Ok(record);
        }
        let enabled = enabled(&record)?;
        let journal = HostJournal::open(config.clone()).await?;
        let mut hive_bootstrap_path = None;
        let result = async {
            if let Some(instance) = hive(&config, &enabled)? {
                let target = running_target(
                    &journal,
                    &instance,
                    "Enabled Hive reset target was not restored.",
                )
                .await?;
                let manifest = journal
                    .manifest(&field(&target, "candidateId")?.to_utf8()?)
                    .await?;
                let runtime = runtime(&target, node.as_ref()).await?;
                let path = runtime_maintenance::hive_reset_bootstrap_marker(
                    &node_path::node_filesystem_path(
                        property(&runtime, "dataRoot")?
                            .as_js_string()
                            .ok_or_else(IvyError::internal)?,
                    )
                    .map_err(|_| IvyError::internal())?,
                );
                hive_bootstrap_path = Some(path.clone());
                self.docker
                    .stop_container(
                        config.clone(),
                        field(&instance, "instanceId")?,
                        number(property(property(&manifest, "shutdown")?, "timeoutMs")?)?,
                        None,
                    )
                    .await?;
                config::atomic_json(
                    path,
                    J::object([
                        ("schemaVersion", J::Number(1.0)),
                        ("resetId", J::String(reset_id.clone())),
                        ("runtimeEpoch", J::String(runtime_epoch.clone())),
                        ("phase", "released".into()),
                    ]),
                )
                .await?;
            }
            filesystem::remove_file_force(runtime_maintenance::runtime_reset_marker(&config)?)
                .await?;
            for instance in instances(&config)?
                .iter()
                .filter(|v| v.get("engine") == Some(&"docker".into()) && selected(v, &enabled))
            {
                let target = running_target(
                    &journal,
                    instance,
                    "Enabled Docker reset target was not restored.",
                )
                .await?;
                let id = field(&target, "candidateId")?.to_utf8()?;
                let candidate = journal.candidate(&id).await?;
                let manifest = journal.manifest(&id).await?;
                let runtime = runtime(&target, node.as_ref()).await?;
                let managed = self
                    .docker
                    .start_container(
                        config.clone(),
                        instance.clone(),
                        target,
                        runtime,
                        candidate,
                        manifest,
                        true,
                        None,
                    )
                    .await?;
                managed.verify().await?;
                managed.dispose();
            }
            for instance in instances(&config)?
                .iter()
                .filter(|v| v.get("engine") == Some(&"process".into()) && selected(v, &enabled))
            {
                if cfg!(target_os = "linux") {
                    self.systemd
                        .start_unit(
                            config.clone(),
                            systemd_unit_name(&config, &field(instance, "instanceId")?)?.into(),
                        )
                        .await?;
                } else if ["host-executor", "service-manager"]
                    .iter()
                    .any(|v| instance.get("componentId") == Some(&J::from(*v)))
                {
                    let name = format!(
                        "{}-{}",
                        linux_resources::installation_identity(&config)?,
                        &hash_json(property(instance, "instanceId")?)?[7..19]
                    );
                    let result = exec_file::exec_file(
                        exec_file::FileCommand {
                            executable: "schtasks.exe".into(),
                            args: vec!["/run".into(), "/tn".into(), name.into()],
                            cwd: None,
                            timeout_ms: 30000,
                            maximum_buffer_bytes: 65536,
                        },
                        self.distribution.clone(),
                        self.options(),
                    )
                    .await
                    .map_err(|_| IvyError::internal())?;
                    if result.failure.is_some() {
                        return Err(IvyError::internal());
                    }
                }
            }
            let mut next = record.clone();
            next.insert("phase", "released".into());
            next.insert("deploymentComplete", false.into());
            next.insert("updatedAt", local::now(self.clock.as_ref())?);
            config::atomic_json(reset_record_path(&config)?, next.clone()).await?;
            if let Some(path) = &hive_bootstrap_path {
                filesystem::remove_file_force(path).await?;
            }
            Ok(next)
        }
        .await;
        match result {
            Ok(next) => {
                journal.close().await?;
                Ok(next)
            }
            Err(error) => {
                // Close authority before trying to recover the durable hold.
                journal.close().await?;
                let recovery = async {
                    config::atomic_json(
                        runtime_maintenance::runtime_reset_marker(&config)?,
                        J::object([
                            ("schemaVersion", J::Number(1.0)),
                            (
                                "deploymentIdentity",
                                property(&record, "deploymentIdentity")?.clone(),
                            ),
                            ("resetId", property(&record, "resetId")?.clone()),
                            ("runtimeEpoch", property(&record, "runtimeEpoch")?.clone()),
                            ("hostId", property(&record, "hostId")?.clone()),
                            ("createdAt", local::now(self.clock.as_ref())?),
                        ]),
                    )
                    .await?;
                    if let Some(path) = hive_bootstrap_path {
                        config::atomic_json(
                            path,
                            J::object([
                                ("schemaVersion", J::Number(1.0)),
                                ("resetId", property(&record, "resetId")?.clone()),
                                ("runtimeEpoch", property(&record, "runtimeEpoch")?.clone()),
                                ("phase", "bootstrap".into()),
                                (
                                    "publisherCredentialDigest",
                                    property(&record, "publisherCredentialDigest")?.clone(),
                                ),
                            ]),
                        )
                        .await?;
                    }
                    Ok::<_, IvyError>(())
                }
                .await;
                // This finally stage runs even if recreating a marker failed.
                let _ = self.clone().stop(config.clone()).await;
                let mut held = record;
                held.insert("phase", "complete".into());
                held.insert("deploymentComplete", false.into());
                held.insert("updatedAt", local::now(self.clock.as_ref())?);
                config::atomic_json(reset_record_path(&config)?, held).await?;
                recovery?;
                Err(error)
            }
        }
    }
}
