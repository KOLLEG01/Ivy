use super::*;
use crate::native_path;

pub(super) struct Entry {
    pub phase: J,
    pub at: String,
    pub request: J,
    kind: &'static str,
    pub directory: PathBuf,
    record: J,
    pub obsolete: bool,
}
impl Entry {
    pub fn completed(&self) -> bool {
        ["succeeded", "rolled_back"]
            .iter()
            .any(|p| self.phase == (*p).into())
    }
}
fn fallback(value: Option<&J>, default: &str) -> J {
    value
        .filter(|v| !v.is_null())
        .cloned()
        .unwrap_or_else(|| default.into())
}
pub(super) fn protect_plan(ids: &mut BTreeSet<String>, plan: &J) -> IvyResult<()> {
    validate_host("BootstrapPlan", plan)?;
    ids.insert(plan.required("candidateId")?.as_str()?);
    for process in array(plan.required("processes")?)? {
        if let Some(id) = process.get("candidateId") {
            ids.insert(id.as_str()?);
        }
    }
    Ok(())
}
pub(super) async fn load(
    config: &J,
    kind: &'static str,
    progress: &Progress,
) -> IvyResult<Vec<Entry>> {
    let root = path(config, "runtimeRoot")?.join(kind);
    let mut result = Vec::new();
    for entry in entries(root.clone()).await? {
        let file_type = internal(entry.file_type())?;
        let filename = entry.file_name();
        let name = filename.to_str();
        if !file_type.is_dir() || file_type.is_symlink() || !name.is_some_and(hash_name) {
            progress.skipped("maintenance", "unrecognized_history_entry", true);
            continue;
        }
        let name = name.unwrap();
        let directory = root.join(name);
        let record = config::json_file_if_present(
            directory.join("record.json"),
            config::CONFIGURATION_BYTES,
        )
        .await?
        .unwrap_or(J::Null);
        let request = match record.get("request").filter(|v| !v.is_null()) {
            Some(request) => request.clone(),
            None => config::json_file_if_present(
                directory.join("request.json"),
                config::CONFIGURATION_BYTES,
            )
            .await?
            .unwrap_or(J::Null),
        };
        let completed = ["succeeded", "rolled_back"]
            .iter()
            .any(|p| record.get("phase") == Some(&(*p).into()));
        let request_object = request.as_object().is_some();
        if !request_object && completed {
            progress.skipped("maintenance", "unrecognized_completed_record", true);
            continue;
        }
        require(
            request_object,
            "storage_invalid",
            "Unrecognized bootstrap maintenance prevents candidate collection.",
        )?;
        if let Err(error) = validate_host("BootstrapMaintenanceRequest", &request) {
            if completed {
                progress.skipped("maintenance", "unrecognized_completed_record", true);
                continue;
            }
            return Err(error);
        }
        require(
            &hash_json(request.required("operationId")?)?[7..] == name,
            "storage_invalid",
            "Maintenance history does not identify its owned directory.",
        )?;
        result.push(Entry {
            phase: fallback(record.get("phase"), "unknown"),
            at: record
                .get("updatedAt")
                .filter(|value| !value.is_null())
                .map(|value| value.as_str().map_err(|_| IvyError::internal()))
                .transpose()?
                .unwrap_or_default(),
            request,
            kind,
            directory,
            record,
            obsolete: false,
        });
    }
    Ok(result)
}
fn retain(ids: &mut BTreeSet<String>, keep: &mut BTreeSet<String>, plan: &J) -> IvyResult<()> {
    keep.insert(hash_json(plan)?);
    protect_plan(ids, plan)
}
/// Only selected OS definitions and unfinished maintenance own installation artifacts.
pub(super) async fn collect_plans(
    config: &J,
    journal: &HostJournal,
    ids: &mut BTreeSet<String>,
    now: f64,
    history: &mut [Entry],
    progress: &Progress,
) -> IvyResult<BTreeSet<PathBuf>> {
    let mut protected = BTreeSet::new();
    let plans = crate::bootstrap::retained_bootstrap_plans(config.clone()).await?;
    if plans.is_empty() { return Ok(protected); }
    let runtime = path(config, "runtimeRoot")?;
    let selected = config::json_file(runtime.join("bootstrap").join(format!("{}.json", crate::linux_resources::installation_identity(config)?)), config::CONFIGURATION_BYTES).await?;
    validate_host("BootstrapPlan", &selected)?;
    let selected_hash = hash_json(&selected)?;
    require(plans.iter().map(hash_json).collect::<IvyResult<Vec<_>>>()?.contains(&selected_hash), "storage_invalid", "Selected bootstrap plan must belong to this installation.")?;
    let mut keep = BTreeSet::new();
    retain(ids, &mut keep, &selected)?;
    let mut selected_record = false;
    let observations = journal.current_owner_observations().await?;
    let mut installed = std::collections::BTreeMap::new();
    for instance in array(config.required("instances")?)? {
        let id = instance.required("instanceId")?.as_str()?;
        installed.insert(id.clone(), journal.installed(&id).await?);
    }
    for entry in history {
        let current = !selected_record && entry.phase == "succeeded".into() && hash_json(entry.request.required("next")?)? == selected_hash;
        let superseded = ["needs_attention", "outcome_unknown", "failed"].iter().any(|p| entry.phase == (*p).into())
            && array(config.required("instances")?)?.iter().all(|instance| {
                let Some(id) = instance.get("instanceId").and_then(J::as_js_string) else { return false; };
                let prior = id.to_utf8().ok().and_then(|id| installed.get(&id)).and_then(Option::as_ref);
                let observed = observations.get_key(id);
                prior.zip(observed).is_some_and(|(prior, observed)| {
                    prior.get("installedAt").and_then(J::as_js_string).is_some_and(|at| at > &JsString::from(entry.at.as_str()))
                        && observed.get("candidateId") == prior.get("candidateId")
                        && ["ready", "stopped", "idle"].iter().any(|p| observed.get("state") == Some(&(*p).into()))
                        && observed.get("observedAt").and_then(J::as_js_string).is_some_and(|at| now - ivy_sdk::clock::parse_date(at) < 15_000.0)
                })
            });
        entry.obsolete = !current && (entry.completed() || superseded);
        if entry.obsolete { continue; }
        protected.insert(entry.directory.clone());
        if current { selected_record = true; continue; }
        retain(ids, &mut keep, entry.request.required("next")?)?;
        let snapshot = entry.request.required("previous")?;
        let owners = array(snapshot.required("owners")?)?;
        for owner in owners { ids.insert(owner.required("candidateId")?.as_str()?); }
        for plan in &plans {
            let processes = array(plan.required("processes")?)?;
            if plan.get("configHash") == snapshot.get("configurationHash") && processes.len() == owners.len()
                && owners.iter().all(|owner| processes.iter().any(|process| {
                    process.get("instanceId") == owner.get("instanceId") && process.get("name") == owner.get("name")
                        && process.get("candidateId").filter(|v| !v.is_null()).or_else(|| plan.get("candidateId")) == owner.get("candidateId")
                })) { retain(ids, &mut keep, plan)?; }
        }
    }
    for plan in plans {
        if keep.contains(&hash_json(&plan)?) { continue; }
        let file = runtime.join("bootstrap-plans").join(format!("{}.json", &hash_json(&plan)?[7..]));
        let info = metadata(file.clone()).await?.ok_or_else(|| IvyError::new("storage_invalid", "A retained bootstrap plan disappeared during collection."))?;
        require(info.is_file() && !info.file_type().is_symlink(), "storage_invalid", "Bootstrap history must remain a regular file.")?;
        blocking(move || internal(fs::remove_file(&file))).await?;
        progress.removed("bootstrapPlans");
    }
    Ok(protected)
}
pub(super) async fn collect_details(
    config: &J,
    journal: &HostJournal,
    history: &[Entry],
    protected: &BTreeSet<PathBuf>,
    progress: &Progress,
) -> IvyResult<()> {
    for entry in history {
        if !entry.obsolete || protected.contains(&entry.directory) {
            progress.skipped("maintenance", "retained_history", false);
            continue;
        }
        let result: IvyResult<()> = async {
            let value = &entry.record;
            if entry.kind == "bootstrap-maintenance" { validate_host("BootstrapMaintenanceRecord", value)?; }
            require(value.get("operationId") == entry.request.get("operationId") && value.get("requestHash") == Some(&hash_json(&entry.request)?.into()), "storage_invalid", "Completed bootstrap record is invalid.")?;
            let current = config::json_file(entry.directory.join("record.json"), config::CONFIGURATION_BYTES).await?;
            require(hash_json(&current)? == hash_json(value)?, "storage_invalid", "Completed maintenance history changed before collection.")?;
            let info = metadata(entry.directory.clone()).await?.ok_or_else(IvyError::internal)?;
            let actual = realpath(entry.directory.clone()).await?;
            require(info.is_dir() && !info.file_type().is_symlink() && actual.parent() == Some(realpath(path(config, "runtimeRoot")?.join(entry.kind)).await?.as_path()), "target_conflict", "Maintenance history leaves its owned root.")?;
            let mut allowed = BTreeSet::from([std::ffi::OsString::from("record.json")]);
            if entry.kind == "bootstrap-automatic" { allowed.insert("request.json".into()); }
            else {
                for definition in array(value.required("definitions")?)? {
                    let id = native_path::to_native(&field(definition, "instanceId")?).into_os_string();
                    for suffix in ["prior", "desired"] { let mut name = id.clone(); name.push(format!(".{suffix}.json")); allowed.insert(name); }
                }
            }
            for file in entries(actual.clone()).await? {
                let kind = internal(file.file_type())?;
                require(allowed.contains(&file.file_name()) && kind.is_file() && !kind.is_symlink(), "target_conflict", "Unrecognized maintenance evidence must be reviewed before collection.")?;
            }
            // Persist a replay fence before deleting the original evidence.
            journal.retire_identity(value.required("operationId")?.as_str()?, value.required("requestHash")?.as_str()?, None).await?;
            blocking(move || internal(fs::remove_dir_all(actual))).await?;
            progress.removed("maintenance");
            Ok(())
        }.await;
        if let Err(error) = result { progress.skipped("maintenance", &error.code, true); }
    }
    Ok(())
}
