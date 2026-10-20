use super::*;

pub(super) struct Entry {
    pub phase: J,
    pub at: J,
    pub request: J,
    kind: &'static str,
    pub directory: PathBuf,
    record: J,
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
        if let Some(id) = process.get("candidateId").filter(|v| truthy(v)) {
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
    let root = joined(&path(config, "runtimeRoot")?, kind);
    let mut result = Vec::new();
    for entry in entries(root.clone()).await? {
        let file_type = internal(entry.file_type())?;
        let name = node_path::node_utf8(&entry.file_name());
        let name = name.to_utf8().ok();
        if !file_type.is_dir()
            || file_type.is_symlink()
            || !name.as_ref().is_some_and(|v| hash_name(v))
        {
            progress.skipped("maintenance", "unrecognized_history_entry", true);
            continue;
        }
        let name = name.unwrap();
        let directory = joined(&root, &name);
        let record = config::json_file_if_present(
            joined(&directory, "record.json"),
            config::CONFIGURATION_BYTES,
        )
        .await?
        .unwrap_or(J::Null);
        let request = match record.get("request").filter(|v| !v.is_null()) {
            Some(request) => request.clone(),
            None => config::json_file_if_present(
                joined(&directory, "request.json"),
                config::CONFIGURATION_BYTES,
            )
            .await?
            .unwrap_or(J::Null),
        };
        let completed = ["succeeded", "rolled_back"]
            .iter()
            .any(|p| record.get("phase") == Some(&(*p).into()));
        if !truthy(&request) && truthy(&record) && completed {
            progress.skipped("maintenance", "unrecognized_completed_record", true);
            continue;
        }
        require(
            truthy(&request),
            "storage_invalid",
            "Unrecognized bootstrap maintenance prevents candidate collection.",
        )?;
        if let Err(error) = validate_host("BootstrapMaintenanceRequest", &request) {
            if truthy(&record) && completed {
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
            at: fallback(record.get("updatedAt"), ""),
            request,
            kind,
            directory,
            record,
        });
    }
    Ok(result)
}
fn retain(ids: &mut BTreeSet<String>, keep: &mut BTreeSet<String>, plan: &J) -> IvyResult<()> {
    keep.insert(hash_json(plan)?);
    protect_plan(ids, plan)
}
fn covers(plan: &J, id: &str) -> bool {
    plan.get("candidateId") == Some(&id.into())
        || plan
            .get("processes")
            .and_then(J::as_array)
            .is_some_and(|p| p.iter().any(|p| p.get("candidateId") == Some(&id.into())))
}
pub(super) async fn collect_plans(
    config: &J,
    journal: &HostJournal,
    ids: &mut BTreeSet<String>,
    cutoff: f64,
    history: &[Entry],
    progress: &Progress,
) -> IvyResult<BTreeSet<PathBuf>> {
    let mut protected = BTreeSet::new();
    let plans = crate::bootstrap::retained_bootstrap_plans(config.clone()).await?;
    let mut keep = BTreeSet::new();
    if plans.is_empty() {
        return Ok(protected);
    }
    let runtime = path(config, "runtimeRoot")?;
    let selected = config::json_file(
        joined(
            &joined(&runtime, "bootstrap"),
            &format!(
                "{}.json",
                crate::linux_resources::installation_identity(config)?
            ),
        ),
        config::CONFIGURATION_BYTES,
    )
    .await?;
    validate_host("BootstrapPlan", &selected)?;
    let selected_hash = hash_json(&selected)?;
    let mut belongs = false;
    for plan in &plans {
        if hash_json(plan)? == selected_hash {
            belongs = true;
        }
    }
    require(
        belongs,
        "storage_invalid",
        "Selected bootstrap plan must belong to this installation.",
    )?;
    retain(ids, &mut keep, &selected)?;
    let mut previous_selected = false;
    for entry in history {
        let unresolved = !entry.completed();
        let next = entry.request.required("next")?;
        let selected_processes = array(selected.required("processes")?)?;
        let next_processes = array(next.required("processes")?)?;
        let previous = !previous_selected
            && entry.phase == "succeeded".into()
            && next_processes.len() == selected_processes.len()
            && selected_processes.iter().all(|process| {
                next_processes.iter().any(|owner| {
                    owner.get("instanceId") == process.get("instanceId")
                        && owner
                            .get("candidateId")
                            .filter(|v| !v.is_null())
                            .or_else(|| next.get("candidateId"))
                            == process
                                .get("candidateId")
                                .filter(|v| !v.is_null())
                                .or_else(|| selected.get("candidateId"))
                })
            });
        if !unresolved && !previous {
            continue;
        }
        protected.insert(entry.directory.clone());
        if previous {
            previous_selected = true;
        }
        if unresolved {
            retain(ids, &mut keep, next)?;
        }
        let snapshot = entry.request.required("previous")?;
        let owners = array(snapshot.required("owners")?)?;
        for owner in owners {
            ids.insert(owner.required("candidateId")?.as_str()?);
        }
        for plan in &plans {
            let processes = array(plan.required("processes")?)?;
            if plan.get("configHash") == snapshot.get("configurationHash")
                && processes.len() == owners.len()
                && owners.iter().all(|owner| {
                    processes.iter().any(|process| {
                        process.get("instanceId") == owner.get("instanceId")
                            && process.get("name") == owner.get("name")
                            && process
                                .get("candidateId")
                                .filter(|v| !v.is_null())
                                .or_else(|| plan.get("candidateId"))
                                == owner.get("candidateId")
                    })
                })
            {
                retain(ids, &mut keep, plan)?;
            }
        }
    }
    let mut inventory = Vec::new();
    for plan in plans {
        let file = joined(
            &joined(&runtime, "bootstrap-plans"),
            &format!("{}.json", &hash_json(&plan)?[7..]),
        );
        let info = metadata(file.clone()).await?;
        inventory.push((plan, file, info));
    }
    inventory.sort_by(|a, b| {
        let at =
            a.2.as_ref()
                .and_then(|m| m.modified().ok())
                .map(milliseconds)
                .unwrap_or(0.0);
        let bt =
            b.2.as_ref()
                .and_then(|m| m.modified().ok())
                .map(milliseconds)
                .unwrap_or(0.0);
        bt.total_cmp(&at)
    });
    // Set iteration order matters when one historical OS plan covers several
    // selections. Keep the original journal insertion order at this boundary.
    for id in journal.retention_candidate_order().await? {
        let mut retained = false;
        for (plan, _, _) in &inventory {
            if keep.contains(&hash_json(plan)?) && covers(plan, &id) {
                retained = true;
                break;
            }
        }
        if retained {
            continue;
        }
        if let Some((plan, _, _)) = inventory.iter().find(|(plan, _, _)| covers(plan, &id)) {
            retain(ids, &mut keep, plan)?;
        }
    }
    for (plan, file, info) in inventory {
        if keep.contains(&hash_json(&plan)?) {
            continue;
        }
        let Some(info) = info else {
            retain(ids, &mut keep, &plan)?;
            continue;
        };
        if milliseconds(internal(info.modified())?) > cutoff {
            retain(ids, &mut keep, &plan)?;
            continue;
        }
        require(
            info.is_file() && !info.file_type().is_symlink(),
            "storage_invalid",
            "Bootstrap history must remain a regular file.",
        )?;
        blocking(move || internal(fs::remove_file(fs_path(&file)))).await?;
        progress.removed("bootstrapPlans");
    }
    Ok(protected)
}
pub(super) async fn collect_details(
    config: &J,
    history: &[Entry],
    protected: &BTreeSet<PathBuf>,
    progress: &Progress,
) -> IvyResult<()> {
    let mut active = BTreeSet::new();
    for entry in history.iter().filter(|e| !e.completed()) {
        let id = field(&entry.request, "operationId")?;
        active.insert(id.clone());
        let mut rollback = id;
        rollback.0.extend("-rollback".encode_utf16());
        active.insert(rollback);
    }
    let mut completed = 0;
    for entry in history {
        if entry.kind != "bootstrap-maintenance" {
            continue;
        }
        if !entry.completed() {
            progress.skipped("maintenance", "unfinished_operation", true);
            continue;
        }
        completed += 1;
        if completed <= 20
            || protected.contains(&entry.directory)
            || active.contains(&field(&entry.request, "operationId")?)
        {
            progress.skipped("maintenance", "retained_history", false);
            continue;
        }
        let result: IvyResult<()> = async {
            let value = &entry.record;
            validate_host("BootstrapMaintenanceRecord", value)?;
            require(
                value.get("operationId") == entry.request.get("operationId")
                    && value.get("requestHash") == Some(&hash_json(&entry.request)?.into()),
                "storage_invalid",
                "Completed bootstrap record is invalid.",
            )?;
            let current = config::json_file(
                joined(&entry.directory, "record.json"),
                config::CONFIGURATION_BYTES,
            )
            .await?;
            require(
                hash_json(&current)? == hash_json(value)?,
                "storage_invalid",
                "Completed maintenance history changed before collection.",
            )?;
            let info = metadata(entry.directory.clone())
                .await?
                .ok_or_else(IvyError::internal)?;
            let actual = realpath(entry.directory.clone()).await?;
            require(
                info.is_dir()
                    && !info.file_type().is_symlink()
                    && actual.parent()
                        == Some(
                            realpath(joined(&path(config, "runtimeRoot")?, entry.kind))
                                .await?
                                .as_path(),
                        ),
                "target_conflict",
                "Maintenance history leaves its owned root.",
            )?;
            for definition in array(value.required("definitions")?)? {
                let id = field(definition, "instanceId")?;
                for suffix in ["prior", "desired"] {
                    let mut name = id.clone();
                    name.0.extend(format!(".{suffix}.json").encode_utf16());
                    let file = node_path::to_native(&node_path::join(
                        node_path::Style::native(),
                        &[node_path::from_native(&entry.directory), name],
                    ));
                    let Some(info) = metadata(file.clone()).await? else {
                        continue;
                    };
                    require(
                        info.is_file()
                            && !info.file_type().is_symlink()
                            && realpath(file.clone()).await?.parent() == Some(actual.as_path()),
                        "target_conflict",
                        "Maintenance detail must be an owned regular file.",
                    )?;
                    blocking(move || internal(fs::remove_file(fs_path(&file)))).await?;
                    progress.removed("maintenance");
                }
            }
            Ok(())
        }
        .await;
        if let Err(error) = result {
            progress.skipped("maintenance", &error.code, true);
        }
    }
    Ok(())
}
