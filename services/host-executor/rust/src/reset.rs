//! Local clear after the caller admits UI bundles/contracts, the preview and
//! reset identities. Full coordinator/publication remains a separate boundary.
use crate::reset_storage::{ResetBootstrap, hive_reset_storage};
use ivy_contracts::{IvyError, IvyJson as J, IvyResult, JsString, core_schema::validate_host};
use ivy_host_runtime::{
    config, filesystem, journal::HostJournal, layout::{self, StorageContext},
    linux_resources::installation_identity, node_path, paths, runtime_maintenance,
    runtime_reset::{ResetHost, preserved_reset_tree_hash, reset_record_path, reset_timestamp,
        restored_bootstrap_target}, target,
};
use ivy_sdk::clock::Clock;
use std::{path::PathBuf, sync::Arc};

fn internal<T, E>(value: Result<T, E>) -> IvyResult<T> {
    value.map_err(|_| IvyError::internal())
}
fn field(value: &J, key: &str) -> IvyResult<JsString> {
    value.required(key)?.as_js_string().cloned().ok_or_else(IvyError::internal)
}
fn array(value: &J) -> IvyResult<&[J]> {
    value.as_array().ok_or_else(IvyError::internal)
}
fn native(value: &JsString) -> IvyResult<PathBuf> {
    internal(node_path::node_filesystem_path(value))
}
fn require(ok: bool, code: &str, message: &str) -> IvyResult<()> {
    if ok { Ok(()) } else { Err(IvyError::new(code, message)) }
}

/// Concrete local clear owner. Hive Store/SQL stays bound at this service layer;
/// no HostRuntime -> Hive/CLI dependency or replacement storage adapter exists.
#[derive(Clone)]
pub struct ResetClear {
    host: ResetHost,
    storage: Arc<StorageContext>,
    clock: Arc<dyn Clock>,
}
impl ResetClear {
    pub fn new(
        distribution: PathBuf,
        job_launcher: PathBuf,
        storage: Arc<StorageContext>,
        clock: Arc<dyn Clock>,
    ) -> IvyResult<Self> {
        Ok(Self { host: ResetHost::new(distribution, job_launcher, clock.clone())?, storage, clock })
    }

    /// Caller supplies the actual checked bootstrap contracts after UI/plan/argv
    /// admission. This is not the runtime-reset-host clear CLI entrypoint.
    /// Administrator admission and all accepted effects survive a lost observer.
    pub async fn clear(
        &self,
        config: J,
        reset_id: JsString,
        epoch: JsString,
        bootstrap_contracts: Vec<J>,
    ) -> IvyResult<J> {
        let this = self.clone();
        internal(tokio::spawn(async move {
            this.host.require_administrator().await?;
            this.clear_admitted(config, reset_id, epoch, bootstrap_contracts).await
        }).await)?
    }

    async fn clear_admitted(
        &self,
        config: J,
        reset_id: JsString,
        epoch: JsString,
        bootstrap_contracts: Vec<J>,
    ) -> IvyResult<J> {
        let record_path = reset_record_path(&config)?;
        let mut record = config::json_file(record_path.clone(), config::CONFIGURATION_BYTES).await?;
        let held = tokio::fs::metadata(runtime_maintenance::runtime_reset_marker(&config)?).await.is_ok();
        require(
            record.get("resetId") == Some(&J::String(reset_id))
                && record.get("runtimeEpoch") == Some(&J::String(epoch))
                && ["prepared", "cleared"].iter().any(|s| record.get("phase") == Some(&J::from(*s)))
                && held,
            "mutation_conflict", "Runtime reset host is not prepared for this identity.",
        )?;
        if record.get("phase") == Some(&"cleared".into()) { return Ok(record); }
        let instances = array(config.required("instances")?)?;
        let hive = instances.iter().find(|v| v.get("componentId") == Some(&"hive".into()));
        let binding = hive_reset_storage();
        if let Some(instance) = hive.filter(|_| record.get("instructionsCaptured") != Some(&J::Bool(true))) {
            let root = field(&layout::service_paths(&config, instance)?, "data")?;
            record.insert("instructions", binding.saved_instructions(native(&root)?.join("hive.sqlite")).await?);
            record.insert("instructionsCaptured", true.into());
            record.insert("updatedAt", reset_timestamp(self.clock.as_ref())?);
            // Capture is durable before the first destructive effect, including
            // a later failure or a lost response from this clear attempt.
            config::atomic_json(record_path.clone(), record.clone()).await?;
        }
        for entry in array(record.required("plan")?.required("delete")?)? {
            let absolute = field(entry, "absolutePath")?;
            let area = field(entry, "area")?;
            let root = if area == JsString::from("host") {
                field(&config, "runtimeRoot")?
            } else {
                let instance = instances.iter().find(|v| v.get("instanceId") == entry.get("instanceId"))
                    .ok_or_else(IvyError::internal)?;
                field(&layout::service_paths(&config, instance)?, &area.to_utf8()?)?
            };
            let (actual, root) = internal(tokio::task::spawn_blocking(move || {
                Ok::<_, IvyError>((paths::future_path(&absolute)?, paths::future_path(&root)?))
            }).await)??;
            let member = field(entry, "path")?;
            require(
                if actual == root {
                    member == JsString::from(".") && ["work", "logs"].iter().any(|v| area == JsString::from(*v))
                } else { self.storage.paths.inside(&root, &actual) },
                "target_conflict", "Runtime reset target changed after preview.",
            )?;
            let absolute = native(&field(entry, "absolutePath")?)?;
            filesystem::remove_force(absolute.clone()).await?;
            if member == JsString::from(".") { internal(tokio::fs::create_dir_all(absolute).await)?; }
        }
        for marker in array(record.required("plan")?.required("completionMarkers")?)? {
            config::atomic_json(native(&field(marker, "absolutePath")?)?, J::object([
                ("schemaVersion", J::Number(1.0)),
                ("resetId", record.required("resetId")?.clone()),
                ("runtimeEpoch", record.required("runtimeEpoch")?.clone()),
                ("completedAt", reset_timestamp(self.clock.as_ref())?),
            ])).await?;
        }
        if let Some(instance) = hive {
            let clock = self.clock.clone();
            binding.rebootstrap(ResetBootstrap {
                path: native(&field(&layout::service_paths(&config, instance)?, "data")?)?.join("hive.sqlite"),
                epoch: field(&record, "runtimeEpoch")?,
                instructions: record.required("instructions")?.clone(),
                scope_roots: array(record.required("plan")?.required("scopeRootIds")?)?.iter()
                    .map(|v| v.as_js_string().cloned().ok_or_else(IvyError::internal)).collect::<IvyResult<_>>()?,
                service_contracts: bootstrap_contracts,
                now: Arc::new(move || reset_timestamp(clock.as_ref())?.as_js_string().cloned().ok_or_else(IvyError::internal)),
            }).await?;
        }
        let bootstrap = if cfg!(target_os = "linux") {
            let root = native(&field(&config, "runtimeRoot")?)?;
            let plan = config::json_file(root.join("bootstrap").join(format!("{}.json", installation_identity(&config)?)), config::CONFIGURATION_BYTES).await?;
            validate_host("BootstrapPlan", &plan)?;
            require(plan.get("os") == Some(&"linux".into()) && plan.get("hostId") == config.get("hostId")
                && plan.get("runtimeRoot") == config.get("runtimeRoot"),
                "target_conflict", "Retained Linux bootstrap belongs to another host runtime.")?;
            Some(plan)
        } else { None };
        let journal = HostJournal::open_with_clock(config.clone(), self.clock.clone()).await?;
        let restored = async {
            for saved in array(record.required("saved")?)? {
                let requested = reset_timestamp(self.clock.as_ref())?;
                let id = field(saved, "instanceId")?;
                let candidate = saved.required("candidate")?;
                let candidate_id = field(candidate, "candidateId")?;
                let enabled = saved.required("enabled")?.as_bool().ok_or_else(IvyError::internal)?;
                journal.save_candidate(candidate.clone(), saved.required("manifest")?.clone()).await?;
                journal.set_installed(J::object([
                    ("instanceId", J::String(id.clone())), ("candidateId", J::String(candidate_id.clone())),
                    ("buildId", candidate.required("buildId")?.clone()), ("enabled", enabled.into()),
                    ("installedAt", requested.clone()),
                ])).await?;
                let retained = bootstrap.as_ref().map(|plan| restored_bootstrap_target(
                    plan, &id, &candidate_id, enabled,
                    requested.as_js_string().cloned().ok_or_else(IvyError::internal)?,
                )).transpose()?.flatten();
                let target = match retained {
                    Some(value) => value,
                    None => target::materialize_target(journal.clone(), id, candidate_id.to_utf8()?, enabled,
                        None, self.storage.account_home.clone()).await?.target,
                };
                journal.restore_target(target).await?;
            }
            Ok::<_, IvyError>(())
        }.await;
        journal.close().await?;
        restored?;
        for saved in array(record.required("plan")?.required("preserve")?)? {
            require(preserved_reset_tree_hash(field(saved, "absolutePath")?, self.storage.clone()).await?
                == *saved.required("checksum")?, "configuration_changed", "Preserved reset configuration changed.")?;
        }
        record.insert("phase", "cleared".into());
        record.insert("updatedAt", reset_timestamp(self.clock.as_ref())?);
        config::atomic_json(record_path, record.clone()).await?;
        Ok(record)
    }
}

#[cfg(test)]
#[path = "../../../../tests/rust/host-reset-clear-cases.rs"]
mod tests;
