use super::*;
use ivy_store::rusqlite::{Connection, OptionalExtension, params};

/// Operate only on the new working copy after recovery-original has been saved.
/// The transaction never replays deployment effects or edits their requests.
pub async fn reconcile_restored_journal(config: J, at: JsString) -> IvyResult<()> {
    internal(tokio::task::spawn_blocking(move || {
        let runtime=field(&config,"runtimeRoot")?;
        let db=internal(Connection::open(native(&join(&[runtime.clone(),"deployments.sqlite".into()]))?))?;
        let result: IvyResult<()>= (|| {
            internal(db.execute_batch("PRAGMA foreign_keys=ON; PRAGMA synchronous=FULL; BEGIN IMMEDIATE"))?;
            let version:i64=internal(db.query_row("PRAGMA user_version",[],|r|r.get(0)))?;
            let host:Option<String>=internal(db.query_row("SELECT value FROM meta WHERE key=?",["hostId"],|r|r.get(0)).optional())?;
            require(version==1&&host.map(J::from).as_ref()==config.get("hostId"),"unsupported_storage","Restored host journal format/identity is unsupported.")?;
            internal(db.execute_batch("DELETE FROM candidates; DELETE FROM installed;"))?;
            let retirements=internal(db.query_row("SELECT 1 FROM sqlite_schema WHERE type='table' AND name='candidate_retirements'",[],|r|r.get::<_,i64>(0)).optional())?.is_some();
            if retirements {internal(db.execute_batch("DELETE FROM candidate_retirements;"))?;}
            internal(db.execute_batch("DELETE FROM meta WHERE key='storage-retention' OR key LIKE 'bootstrap-rollback:%';"))?;
            let rows={let mut q=internal(db.prepare("SELECT entry_json FROM operations ORDER BY deployment_id"))?;let rows=internal(q.query_map([],|r|r.get::<_,String>(0)))?;internal(rows.collect::<Result<Vec<_>,_>>())?};
            for row in rows {
                let mut entry=json::parse(&row,usize::MAX)?;validate_host("JournalEntry",&entry)?;
                let old=field(&entry,"configurationPath")?;
                entry.insert("configurationPath",J::String(join(&[runtime.clone(),"accepted-configurations".into(),node_path::basename(Style::native(),&old)])));
                let mut activation=entry.required("activation")?.clone();
                if let Some(previous)=activation.get("previousConfigurationPath").filter(|v|truthy(v)) {activation.insert("previousConfigurationPath",J::String(join(&[runtime.clone(),"accepted-configurations".into(),node_path::basename(Style::native(),&text(previous)?)])));}
                entry.insert("activation",activation);
                let deployment=field(entry.required("record")?,"deploymentId")?.to_utf8()?;
                if ["succeeded","rolled_back","failed","needs_attention"].iter().any(|p|entry.required("record").ok().and_then(|r|r.get("phase"))==Some(&(*p).into())) {
                    internal(db.execute("UPDATE operations SET entry_json=? WHERE deployment_id=?",params![json::canonical(&entry,usize::MAX)?,deployment]))?;continue;
                }
                let mut record=entry.required("record")?.clone();
                for(k,v)in[("phase","needs_attention".into()),("updatedAt",J::String(at.clone())),("observedBuild",J::Null),("errorCode","restore_reconciliation_required".into()),("readiness",J::object([("state","unknown".into()),("message","Restored deployment may have effects after its snapshot; original evidence is retained and no action is replayed.".into())]))]{record.insert(k,v);}
                entry.insert("record",record);validate_host("JournalEntry",&entry)?;
                internal(db.execute("UPDATE operations SET phase=?,entry_json=? WHERE deployment_id=?",params!["needs_attention",json::canonical(&entry,usize::MAX)?,deployment]))?;
            }
            internal(db.execute_batch("DELETE FROM observations; DELETE FROM runtime_targets;"))?;
            internal(db.execute("INSERT INTO meta VALUES (?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value",params!["installation",crate::accepted_configuration::installation_record(&config)?]))?;
            internal(db.execute_batch("COMMIT"))?;Ok(())
        })();
        let rollback=if !db.is_autocommit(){internal(db.execute_batch("ROLLBACK"))}else{Ok(())};
        let close=internal(db.close().map_err(|(_,e)|e));rollback?;close?;result
    }).await)?
}

/// Hold every restored non-successful manual bootstrap record and retain its
/// original bytes before rewriting the working record. No OS action is invoked.
pub async fn hold_restored_bootstrap_maintenance(config: J, at: JsString) -> IvyResult<u64> {
    internal(
        tokio::spawn(async move {
            let runtime = field(&config, "runtimeRoot")?;
            let parent = join(&[runtime.clone(), "bootstrap-maintenance".into()]);
            if !exists(&parent).await? {
                return Ok(0);
            }
            let scan = native(&parent)?;
            let entries = internal(
                tokio::task::spawn_blocking(move || {
                    let mut entries = internal(
                        internal(std::fs::read_dir(scan))?.collect::<Result<Vec<_>, _>>(),
                    )?;
                    entries.sort_by_key(|e| e.file_name());
                    Ok::<_, IvyError>(entries)
                })
                .await,
            )??;
            let mut bytes = 0;
            for entry in entries {
                let name = node_path::node_utf8(&entry.file_name());
                let kind = internal(entry.file_type())?;
                require(
                    kind.is_dir()
                        && !kind.is_symlink()
                        && name.0.len() == 64
                        && name.0.iter().all(|c| matches!(*c,48..=57|97..=102)),
                    "storage_invalid",
                    "Invalid restored bootstrap maintenance directory.",
                )?;
                let path = join(&[parent.clone(), name.clone(), "record.json".into()]);
                if !exists(&path).await? {
                    continue;
                }
                let mut record = json_file(&path, config::CONFIGURATION_BYTES).await?;
                validate_host("BootstrapMaintenanceRecord", &record)?;
                require(
                    JsString::from(&hash_json(record.required("operationId")?)?[7..]) == name
                        && record.get("requestHash")
                            == Some(&hash_json(record.required("request")?)?.into()),
                    "storage_invalid",
                    "Restored bootstrap identity is invalid.",
                )?;
                if record.get("phase") == Some(&"succeeded".into()) {
                    continue;
                }
                let original = join(&[
                    runtime.clone(),
                    "recovery-original/bootstrap-maintenance".into(),
                    name,
                    "record.json".into(),
                ]);
                bytes +=
                    backup_files::copy_verified(native(&path)?, native(&original)?, None, false)
                        .await?
                        .bytes;
                record.insert("phase", "needs_attention".into());
                record.insert("errorCode", "restore_reconciliation_required".into());
                record.insert("updatedAt", J::String(at.clone()));
                config::atomic_json(native(&path)?, record).await?;
            }
            Ok(bytes)
        })
        .await,
    )?
}

/// Relocate only the disposable project backup index. Hive assignments and
/// saved project plans retain their original paths and authority.
pub async fn relocate_projects(
    original: J,
    config: J,
    captured: Vec<StorageArea>,
    storage: Arc<StorageContext>,
) -> IvyResult<()> {
    internal(
        tokio::spawn(async move {
            for instance in array(config.required("instances")?)?
                .iter()
                .filter(|v| v.get("componentId") == Some(&"agent-manager".into()))
            {
                let path = join(&[
                    field(&layout::service_paths(&config, instance)?, "data")?,
                    "project-backup-paths.json".into(),
                ]);
                if !exists(&path).await? {
                    continue;
                }
                let prior = array(original.required("instances")?)?
                    .iter()
                    .find(|v| v.get("instanceId") == instance.get("instanceId"))
                    .ok_or_else(IvyError::internal)?;
                let value = |instance: &J| -> IvyResult<JsString> {
                    match instance.required("settings")?.get("internalProjectRoot") {
                        Some(v) => text(v),
                        None => Err(IvyError::new("backup_invalid", "Retained projects require an internal project root.")),
                    }
                };
                let before = value(prior)?;
                let after = value(instance)?;
                let paths = json_file(&path, config::CONFIGURATION_BYTES).await?;
                let areas = project_storage::restored_storage_areas(&config, &captured, &storage)?;
                let prefix = concat(&[
                    "retained-projects/".into(),
                    field(instance, "instanceId")?,
                    "/".into(),
                ]);
                let mut result = Vec::new();
                for cwd in array(&paths)? {
                    let cwd = text(cwd)?;
                    let retained = captured.iter().find(|a| {
                        a.key.0.starts_with(&prefix.0) && storage.paths.inside(&a.path, &cwd)
                    });
                    result.push(J::String(if let Some(retained) = retained {
                        join(&[
                            areas
                                .iter()
                                .find(|a| a.key == retained.key)
                                .ok_or_else(IvyError::internal)?
                                .path
                                .clone(),
                            storage.paths.relative(&retained.path, &cwd),
                        ])
                    } else if storage.paths.inside(&before, &cwd) {
                        join(&[after.clone(), storage.paths.relative(&before, &cwd)])
                    } else {
                        cwd
                    }));
                }
                config::atomic_json(native(&path)?, J::Array(result)).await?;
            }
            Ok(())
        })
        .await,
    )?
}
