//! Exact stopped-owner bootstrap maintenance. A dedicated journal transaction
//! fences lifecycle acceptance while retained OS definitions are replaced.
use crate::{
    IvyError, IvyJson, IvyResult,
    artifact::{self, FileCompare},
    backup_files, bootstrap,
    bootstrap_os::BootstrapOs,
    config,
    executor_lock::ExecutorLock,
    host_config,
    journal::HostJournal,
    layout, node_path,
    node_runtime::NodeRuntime,
    private_backup::{self, OfflineOwners},
};
use ivy_contracts::{JsString, core_schema::validate_host, hash::hash_json};
use ivy_sdk::clock::{Clock, SystemClock};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::Arc,
};

fn require(value: bool, code: &str, message: &str) -> IvyResult<()> {
    if value {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn field<'a>(value: &'a IvyJson, key: &str) -> &'a IvyJson {
    static NULL: IvyJson = IvyJson::Null;
    value.get(key).unwrap_or(&NULL)
}
fn truthy(value: &IvyJson) -> bool {
    match value {
        IvyJson::Null => false,
        IvyJson::Bool(value) => *value,
        IvyJson::Number(value) => *value != 0.0 && !value.is_nan(),
        IvyJson::String(value) => !value.0.is_empty(),
        _ => true,
    }
}
fn array(value: &IvyJson) -> IvyResult<&[IvyJson]> {
    value.as_array().ok_or_else(IvyError::internal)
}
fn text(value: &IvyJson, key: &str) -> IvyResult<String> {
    value.required(key)?.as_str()
}
fn js(value: &IvyJson, key: &str) -> IvyResult<JsString> {
    value
        .required(key)?
        .as_js_string()
        .cloned()
        .ok_or_else(IvyError::internal)
}
fn stamp(clock: &dyn Clock) -> IvyResult<IvyJson> {
    Ok(chrono::DateTime::from_timestamp_millis(clock.utc_millis())
        .ok_or_else(IvyError::internal)?
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
        .into())
}
fn absolute(path: &Path) -> IvyResult<PathBuf> {
    Ok(node_path::to_native(
        &node_path::Context::current()
            .map_err(|_| IvyError::internal())?
            .resolve(&[node_path::from_native(path)]),
    ))
}
pub fn operation_root(config: &IvyJson, operation_id: &JsString) -> IvyResult<PathBuf> {
    require(
        !operation_id.0.is_empty() && operation_id.0.len() <= 128,
        "invalid_arguments",
        "A bounded stable bootstrap operation ID is required.",
    )?;
    Ok(config::path_value(config.required("runtimeRoot")?)?
        .join("bootstrap-maintenance")
        .join(&hash_json(&IvyJson::String(operation_id.clone()))?[7..]))
}
async fn retained(config: &IvyJson, operation_id: &JsString) -> IvyResult<Option<IvyJson>> {
    let path = operation_root(config, operation_id)?.join("record.json");
    if !backup_files::exists(&path).await? {
        return Ok(None);
    }
    let record = config::json_file(path, config::CONFIGURATION_BYTES).await?;
    validate_host("BootstrapMaintenanceRecord", &record)?;
    require(
        record.get("operationId") == Some(&IvyJson::String(operation_id.clone()))
            && record.required("request")?.get("operationId")
                == Some(&IvyJson::String(operation_id.clone()))
            && record.get("requestHash") == Some(&hash_json(record.required("request")?)?.into()),
        "storage_invalid",
        "Bootstrap operation identity is inconsistent.",
    )?;
    Ok(Some(record))
}
pub async fn bootstrap_status(config: &IvyJson, operation_id: &JsString) -> IvyResult<IvyJson> {
    retained(config, operation_id).await?.ok_or_else(|| {
        IvyError::new(
            "not_found",
            "Bootstrap maintenance operation was not found.",
        )
    })
}
async fn current_configuration(path: &Path) -> IvyResult<IvyJson> {
    let input = config::json_file(path.to_owned(), config::CONFIGURATION_BYTES).await?;
    let context = host_config::HostContext::current()?;
    layout::resolve_host_configuration(
        &input,
        &node_path::from_native(path),
        &context.environment,
        context.paths.style,
        &context.account_home,
    )
}
pub(crate) async fn candidate_plans(
    config: &IvyJson,
    path: &Path,
    journal: &HostJournal,
    next: Option<&IvyJson>,
    compare: &FileCompare,
) -> IvyResult<Vec<IvyJson>> {
    let rows = journal.retained_candidates("host-executor", 1001).await?;
    require(
        !rows.is_empty() && rows.len() <= 1000,
        "limit_exceeded",
        "Expected a bounded set of retained host-executor candidates.",
    )?;
    let mut plans = Vec::new();
    for candidate in rows {
        validate_host("Candidate", &candidate)?;
        match journal.manifest(&text(&candidate, "candidateId")?).await {
            Ok(_) => (),
            Err(error) if error.code == "invalid_arguments" => continue,
            Err(error) => return Err(error),
        }
        let components =
            bootstrap::bootstrap_components(config, journal, &candidate, &[], compare).await?;
        plans.push(bootstrap::configuration_bootstrap_plan(
            config,
            &node_path::from_native(path),
            &candidate,
            None,
            &components,
        )?);
    }
    let mut history = bootstrap::retained_bootstrap_plans(config.clone()).await?;
    if let Some(next) = next {
        history.push(next.clone());
    }
    for prior in history {
        let candidate = match journal.candidate(&text(&prior, "candidateId")?).await {
            Ok(candidate) => candidate,
            Err(error) if error.code == "not_found" => continue,
            Err(error) => return Err(error),
        };
        if candidate.get("componentId") != Some(&"host-executor".into()) {
            continue;
        }
        require(
            prior.get("artifactRoot") == candidate.get("artifactRoot")
                && prior.get("configPath") == Some(&IvyJson::String(node_path::from_native(path)))
                && prior.get("nodeExecutable") == config.required("executables")?.get("node"),
            "target_conflict",
            "Retained bootstrap plan differs from this candidate or host paths.",
        )?;
        plans.push(prior);
    }
    let mut hashes = BTreeSet::new();
    let mut unique = Vec::new();
    for plan in plans {
        if hashes.insert(hash_json(&plan)?) {
            unique.push(plan);
        }
    }
    Ok(unique)
}
fn public_owner(owner: &IvyJson) -> IvyJson {
    IvyJson::object(
        [
            "instanceId",
            "componentId",
            "name",
            "candidateId",
            "definitionHash",
            "enabled",
            "running",
        ]
        .into_iter()
        .filter_map(|key| owner.get(key).map(|value| (key, value.clone()))),
    )
}
struct Inner {
    node: NodeRuntime,
    compare: FileCompare,
    os: Arc<dyn BootstrapOs>,
    job_launcher: PathBuf,
    clock: Arc<dyn Clock>,
}
#[derive(Clone)]
pub struct BootstrapMaintenance(Arc<Inner>);
impl BootstrapMaintenance {
    pub fn new(
        node: NodeRuntime,
        compare: FileCompare,
        os: Arc<dyn BootstrapOs>,
        job_launcher: PathBuf,
    ) -> Self {
        Self::with_clock(node, compare, os, job_launcher, Arc::new(SystemClock))
    }
    pub fn with_clock(
        node: NodeRuntime,
        compare: FileCompare,
        os: Arc<dyn BootstrapOs>,
        job_launcher: PathBuf,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self(Arc::new(Inner {
            node,
            compare,
            os,
            job_launcher,
            clock,
        }))
    }
    pub fn os(&self) -> &Arc<dyn BootstrapOs> {
        &self.0.os
    }
    pub fn clock(&self) -> &Arc<dyn Clock> {
        &self.0.clock
    }
    pub async fn inspect(&self, config_path: PathBuf) -> IvyResult<IvyJson> {
        let inner = self.0.clone();
        tokio::spawn(async move { inner.inspect(config_path).await })
            .await
            .map_err(|_| IvyError::internal())?
    }
    /// The admitted operation owns its transaction, exact evidence and locks
    /// through cleanup even when an observer drops its response future.
    pub async fn update(&self, config_path: PathBuf, request: IvyJson) -> IvyResult<IvyJson> {
        let inner = self.0.clone();
        tokio::spawn(async move { inner.update(config_path, request).await })
            .await
            .map_err(|_| IvyError::internal())?
    }
}
impl Inner {
    async fn inspect_owned(
        &self,
        plans: &[IvyJson],
        journal: &HostJournal,
        verify_artifacts: bool,
    ) -> IvyResult<Vec<IvyJson>> {
        let owners = self.os.inspect(plans.to_vec()).await?;
        let instances = array(
            plans
                .first()
                .ok_or_else(IvyError::internal)?
                .required("processes")?,
        )?;
        let mut valid = owners.len() == instances.len();
        if valid {
            if owners.iter().any(IvyJson::is_null) {
                return Err(IvyError::internal());
            }
            let ids = owners
                .iter()
                .map(|owner| field(owner, "instanceId").as_js_string().cloned())
                .collect::<BTreeSet<_>>();
            valid = ids.len() == instances.len();
        }
        if valid {
            for owner in &owners {
                if !instances.iter().any(|instance| {
                    instance.get("instanceId") == owner.get("instanceId")
                        && instance.get("name") == owner.get("name")
                }) {
                    valid = false;
                    break;
                }
                let definition = owner.required("definition")?;
                if definition.is_null() {
                    return Err(IvyError::internal());
                }
                if definition.get("hash") != owner.get("definitionHash") {
                    valid = false;
                    break;
                }
            }
        }
        require(
            valid,
            "target_conflict",
            "Observed bootstrap owners differ from the configured instance set.",
        )?;
        if verify_artifacts {
            let mut seen = BTreeSet::new();
            for owner in &owners {
                let id = text(owner, "candidateId")?;
                if seen.insert(id.clone()) {
                    artifact::verify_candidate(
                        journal.candidate(&id).await?,
                        config::path_value(journal.config().await?.required("artifactRoot")?)?,
                        &self.node,
                    )
                    .await?;
                }
            }
        }
        Ok(owners)
    }
    async fn offline(&self, journal: &HostJournal, owners: &[IvyJson]) -> IvyResult<()> {
        require(
            journal.unfinished().await?.is_empty(),
            "bootstrap_busy",
            "Finish or reconcile host activations before bootstrap maintenance.",
        )?;
        require(
            owners.iter().all(|owner| {
                !truthy(field(owner, "running"))
                    && (!cfg!(windows) || !truthy(field(owner, "enabled")))
            }),
            "bootstrap_busy",
            "Stop owned OS processes and disable Windows scheduler entries before bootstrap maintenance.",
        )
    }
    async fn inspect(&self, config_path: PathBuf) -> IvyResult<IvyJson> {
        let path = absolute(&config_path)?;
        let config = host_config::host_config(path.clone()).await?;
        let journal = HostJournal::open_with_clock(config.clone(), self.clock.clone()).await?;
        let result = async {
            let plans = candidate_plans(&config, &path, &journal, None, &self.compare).await?;
            let owners = self.inspect_owned(&plans, &journal, false).await?;
            require(
                hash_json(&current_configuration(&path).await?)? == hash_json(&config)?,
                "configuration_changed",
                "Host configuration changed during bootstrap inspection.",
            )?;
            let snapshot = IvyJson::object([
                ("schemaVersion", IvyJson::Number(1.0)),
                ("hostId", config.required("hostId")?.clone()),
                (
                    "installationId",
                    plans
                        .first()
                        .ok_or_else(IvyError::internal)?
                        .required("installationId")?
                        .clone(),
                ),
                ("configPath", IvyJson::String(node_path::from_native(&path))),
                ("configurationHash", hash_json(&config)?.into()),
                ("observedAt", stamp(self.clock.as_ref())?),
                (
                    "owners",
                    IvyJson::Array(owners.iter().map(public_owner).collect()),
                ),
            ]);
            validate_host("BootstrapSnapshot", &snapshot)?;
            Ok(snapshot)
        }
        .await;
        journal.close().await?;
        result
    }
    async fn save(&self, root: &Path, record: &mut IvyJson) -> IvyResult<()> {
        record.insert("updatedAt", stamp(self.clock.as_ref())?);
        validate_host("BootstrapMaintenanceRecord", record)?;
        config::atomic_json(root.join("record.json"), record.clone()).await
    }
    async fn update(&self, config_path: PathBuf, request: IvyJson) -> IvyResult<IvyJson> {
        validate_host("BootstrapMaintenanceRequest", &request)?;
        let path = absolute(&config_path)?;
        let config = host_config::host_config(path.clone()).await?;
        let operation = js(&request, "operationId")?;
        let root = operation_root(&config, &operation)?;
        let request_hash = hash_json(&request)?;
        let lock = ExecutorLock::acquire(
            config::path_value(config.required("runtimeRoot")?)?.join("bootstrap-maintenance-lock"),
            false,
        )
        .await?;
        let mut record = None;
        let mut journal = None;
        let mut owners = None;
        let result = self
            .apply(
                &config,
                &path,
                &request,
                &root,
                &request_hash,
                &mut record,
                &mut journal,
                &mut owners,
            )
            .await;
        let result = match result {
            Err(error) => {
                if let Some(record) = record.as_mut().filter(|record| {
                    record.get("requestHash") == Some(&request_hash.clone().into())
                        && record.get("phase") != Some(&"succeeded".into())
                }) {
                    record.insert("phase", "needs_attention".into());
                    record.insert("errorCode", IvyJson::String(error.code_js()));
                    self.save(&root, record).await.map(|_| record.clone())
                } else {
                    Err(error)
                }
            }
            value => value,
        };
        // Release every admitted resource even when an evidence write failed.
        let mut cleanup = None;
        if let Some(journal) = journal {
            if let Err(error) = journal.rollback_maintenance().await {
                cleanup = Some(error);
            }
            if let Err(error) = journal.close().await {
                cleanup.get_or_insert(error);
            }
        }
        if let Some(mut owners) = owners {
            if let Err(error) = owners.close().await {
                cleanup.get_or_insert(error);
            }
        }
        if let Err(error) = lock.close().await {
            cleanup.get_or_insert(error);
        }
        if let Some(error) = cleanup {
            Err(error)
        } else {
            result
        }
    }
    async fn apply(
        &self,
        config: &IvyJson,
        path: &Path,
        request: &IvyJson,
        root: &Path,
        request_hash: &str,
        record_slot: &mut Option<IvyJson>,
        journal_slot: &mut Option<HostJournal>,
        owner_slot: &mut Option<OfflineOwners>,
    ) -> IvyResult<IvyJson> {
        *record_slot = retained(config, &js(request, "operationId")?).await?;
        require(
            record_slot.as_ref().is_none_or(|record| {
                record.get("requestHash") == Some(&IvyJson::from(request_hash))
            }),
            "operation_conflict",
            "Bootstrap operation ID already belongs to another exact request.",
        )?;
        if let Some(record) = record_slot.as_ref().filter(|record| {
            record.get("phase") == Some(&"succeeded".into())
                || record.get("errorCode") == Some(&"restore_reconciliation_required".into())
        }) {
            return Ok(record.clone());
        }
        let previous = request.required("previous")?;
        let next = request.required("next")?;
        let path_value = IvyJson::String(node_path::from_native(path));
        let hash = IvyJson::from(hash_json(config)?);
        require(
            next.get("configPath") == Some(&path_value)
                && previous.get("configPath") == Some(&path_value)
                && next.get("hostId") == config.get("hostId")
                && previous.get("hostId") == config.get("hostId")
                && next.get("runtimeRoot") == config.get("runtimeRoot")
                && next.get("configHash") == Some(&hash)
                && previous.get("configurationHash") == Some(&hash),
            "configuration_changed",
            "Bootstrap maintenance belongs to a different installation or configuration.",
        )?;
        *journal_slot =
            Some(HostJournal::open_with_clock(config.clone(), self.clock.clone()).await?);
        let journal = journal_slot.as_ref().unwrap();
        let candidate = journal.candidate(&text(next, "candidateId")?).await?;
        require(
            candidate.get("componentId") == Some(&"host-executor".into())
                && next.get("os")
                    == Some(&IvyJson::from(if cfg!(windows) {
                        "win32"
                    } else {
                        std::env::consts::OS
                    })),
            "target_conflict",
            "Maintenance requires this platform's checked host-executor candidate.",
        )?;
        let artifacts = config::path_value(config.required("artifactRoot")?)?;
        let manifest =
            artifact::verify_candidate(candidate.clone(), artifacts.clone(), &self.node).await?;
        let pending = array(next.required("processes")?)?
            .iter()
            .filter(|process| {
                process.get("componentId") != Some(&"host-executor".into())
                    && process.get("candidateId").is_some_and(truthy)
            })
            .map(|process| async {
                let selected = journal.candidate(&text(process, "candidateId")?).await?;
                require(
                    selected.get("componentId") == process.get("componentId"),
                    "target_conflict",
                    "Bootstrap process release belongs to another component.",
                )?;
                let manifest =
                    artifact::verify_candidate(selected.clone(), artifacts.clone(), &self.node)
                        .await?;
                Ok::<_, IvyError>(IvyJson::object([
                    ("candidate", selected),
                    ("manifest", manifest),
                ]))
            });
        let mut preferred = vec![IvyJson::object([
            ("candidate", candidate.clone()),
            ("manifest", manifest),
        ])];
        preferred.extend(futures_util::future::try_join_all(pending).await?);
        let components =
            bootstrap::bootstrap_components(config, journal, &candidate, &preferred, &self.compare)
                .await?;
        let plan = bootstrap::configuration_bootstrap_plan(
            config,
            &node_path::from_native(path),
            &candidate,
            next.get("linuxResources")
                .and_then(|value| value.get("budget")),
            &components,
        )?;
        require(
            hash_json(next)? == hash_json(&plan)?,
            "target_conflict",
            "Next bootstrap plan differs from the verified candidate/configuration.",
        )?;
        let previous_owners = array(previous.required("owners")?)?;
        let processes = array(next.required("processes")?)?;
        require(
            previous.get("installationId") == next.get("installationId")
                && previous_owners.len() == processes.len()
                && previous_owners
                    .iter()
                    .map(|owner| js(owner, "instanceId"))
                    .collect::<IvyResult<BTreeSet<_>>>()?
                    .len()
                    == processes.len()
                && previous_owners.iter().all(|owner| {
                    processes.iter().any(|process| {
                        process.get("instanceId") == owner.get("instanceId")
                            && process.get("name") == owner.get("name")
                    })
                }),
            "target_conflict",
            "Bootstrap instance identities or OS names changed.",
        )?;
        if record_slot.is_none() {
            let parent = root.parent().ok_or_else(IvyError::internal)?;
            if !backup_files::exists(parent).await? {
                backup_files::private_directory(parent.to_owned(), self.job_launcher.clone())
                    .await?;
            }
            if !backup_files::exists(root).await? {
                backup_files::private_directory(root.to_owned(), self.job_launcher.clone()).await?;
            }
            let now = stamp(self.clock.as_ref())?;
            let record = IvyJson::object([
                ("schemaVersion", IvyJson::Number(1.0)),
                ("operationId", request.required("operationId")?.clone()),
                ("requestHash", request_hash.into()),
                ("request", request.clone()),
                ("createdAt", now.clone()),
                ("updatedAt", now),
                ("phase", "checking".into()),
                ("errorCode", IvyJson::Null),
                ("definitions", IvyJson::Array(Vec::new())),
            ]);
            *record_slot = Some(record.clone());
            config::atomic_json(root.join("record.json"), record).await?;
        }
        let record = record_slot.as_mut().unwrap();
        let ids = processes
            .iter()
            .map(|process| js(process, "instanceId"))
            .collect::<IvyResult<Vec<_>>>()?;
        *owner_slot =
            Some(private_backup::offline_bootstrap_owners(config, &ids, self.clock.clone()).await?);
        journal.begin_maintenance().await?;
        let plans = candidate_plans(config, path, journal, Some(next), &self.compare).await?;
        let owners = self.inspect_owned(&plans, journal, true).await?;
        self.offline(journal, &owners).await?;
        require(
            hash_json(&current_configuration(path).await?)? == text(next, "configHash")?,
            "configuration_changed",
            "Host configuration changed before bootstrap publication.",
        )?;
        bootstrap::retain_bootstrap_plan(next, self.job_launcher.clone()).await?;
        if let Some(prepare) = self.os.prepare(next.clone()) {
            prepare.await?;
        }
        let mut evidence = BTreeMap::new();
        let mut definitions = array(record.required("definitions")?)?.to_vec();
        if definitions.is_empty() {
            for owner in &owners {
                let prior = previous_owners
                    .iter()
                    .find(|prior| prior.get("instanceId") == owner.get("instanceId"))
                    .ok_or_else(IvyError::internal)?;
                require(
                    owner.get("definitionHash") == prior.get("definitionHash")
                        && owner.get("candidateId") == prior.get("candidateId"),
                    "target_conflict",
                    "OS bootstrap changed after its original snapshot.",
                )?;
                let desired = self.os.render(owner.clone(), next.clone()).await?;
                self.os.check(desired.clone()).await?;
                let old = owner.required("definition")?.clone();
                let id = text(owner, "instanceId")?;
                for (name, definition) in [("prior", &old), ("desired", &desired)] {
                    let file = root.join(format!("{id}.{name}.json"));
                    if backup_files::exists(&file).await? {
                        require(
                            hash_json(
                                &config::json_file(file, config::CONFIGURATION_BYTES).await?,
                            )? == hash_json(definition)?,
                            "storage_invalid",
                            "Incomplete retained definition evidence differs from this request.",
                        )?;
                    } else {
                        config::atomic_json(file, definition.clone()).await?;
                    }
                }
                evidence.insert(id, (old, desired));
            }
            definitions = owners
                .iter()
                .map(|owner| {
                    let id = text(owner, "instanceId")?;
                    Ok(IvyJson::object([
                        ("instanceId", owner.required("instanceId")?.clone()),
                        ("name", owner.required("name")?.clone()),
                        ("previousHash", owner.required("definitionHash")?.clone()),
                        (
                            "desiredHash",
                            evidence.get(&id).unwrap().1.required("hash")?.clone(),
                        ),
                        ("applied", false.into()),
                    ]))
                })
                .collect::<IvyResult<Vec<_>>>()?;
            record.insert("definitions", IvyJson::Array(definitions.clone()));
        } else {
            require(
                definitions.len() == owners.len()
                    && definitions
                        .iter()
                        .map(|value| js(value, "instanceId"))
                        .collect::<IvyResult<BTreeSet<_>>>()?
                        .len()
                        == owners.len(),
                "storage_invalid",
                "Retained bootstrap definition set is incomplete.",
            )?;
            for definition in &definitions {
                let old = previous_owners.iter().find(|owner| {
                    owner.get("instanceId") == definition.get("instanceId")
                        && owner.get("name") == definition.get("name")
                });
                require(
                    old.is_some_and(|old| {
                        old.get("definitionHash") == definition.get("previousHash")
                    }),
                    "storage_invalid",
                    "Retained bootstrap definition identity changed.",
                )?;
                let id = text(definition, "instanceId")?;
                let prior = config::json_file(
                    root.join(format!("{id}.prior.json")),
                    config::CONFIGURATION_BYTES,
                )
                .await?;
                let desired = config::json_file(
                    root.join(format!("{id}.desired.json")),
                    config::CONFIGURATION_BYTES,
                )
                .await?;
                self.os.check(prior.clone()).await?;
                self.os.check(desired.clone()).await?;
                let mut owner = old.unwrap().clone();
                owner.insert("definition", prior.clone());
                require(
                    prior.get("hash") == definition.get("previousHash")
                        && desired.get("hash") == definition.get("desiredHash")
                        && self.os.render(owner, next.clone()).await?.get("hash")
                            == desired.get("hash"),
                    "storage_invalid",
                    "Retained definitions are not the exact old/new request.",
                )?;
                evidence.insert(id, (prior, desired));
            }
        }
        for owner in &owners {
            let definition = definitions
                .iter()
                .find(|definition| definition.get("instanceId") == owner.get("instanceId"))
                .ok_or_else(IvyError::internal)?;
            require(
                ["previousHash", "desiredHash"]
                    .iter()
                    .any(|key| definition.get(key) == owner.get("definitionHash")),
                "target_conflict",
                "An OS definition has a third state; maintenance will not overwrite it.",
            )?;
        }
        record.insert("phase", "updating".into());
        record.insert("errorCode", IvyJson::Null);
        self.save(root, record).await?;
        for index in 0..definitions.len() {
            require(
                hash_json(&current_configuration(path).await?)? == text(next, "configHash")?,
                "configuration_changed",
                "Host configuration changed during bootstrap maintenance.",
            )?;
            let id = text(&definitions[index], "instanceId")?;
            let (prior, desired) = evidence.get(&id).ok_or_else(IvyError::internal)?;
            let mut original = previous_owners
                .iter()
                .find(|owner| owner.get("instanceId") == definitions[index].get("instanceId"))
                .ok_or_else(IvyError::internal)?
                .clone();
            original.insert("definition", prior.clone());
            self.os
                .apply(original, desired.clone(), plans.clone())
                .await?;
            definitions[index].insert("applied", true.into());
            record.insert("definitions", IvyJson::Array(definitions.clone()));
            self.save(root, record).await?;
        }
        self.os.reload().await?;
        let after = self.inspect_owned(&plans, journal, true).await?;
        self.offline(journal, &after).await?;
        require(
            after.iter().all(|owner| {
                processes
                    .iter()
                    .find(|process| process.get("instanceId") == owner.get("instanceId"))
                    .and_then(|process| process.get("candidateId"))
                    == owner.get("candidateId")
                    && evidence
                        .get(&text(owner, "instanceId").unwrap_or_default())
                        .and_then(|value| value.1.get("hash"))
                        == owner.get("definitionHash")
            }),
            "target_conflict",
            "OS definitions did not retain the selected bootstrap.",
        )?;
        require(
            hash_json(&current_configuration(path).await?)? == text(next, "configHash")?,
            "configuration_changed",
            "Host configuration changed before the final bootstrap outcome.",
        )?;
        bootstrap::publish_bootstrap_instance_configurations(next, config, journal).await?;
        config::atomic_json(
            config::path_value(config.required("runtimeRoot")?)?
                .join("bootstrap")
                .join(format!("{}.json", text(next, "installationId")?)),
            next.clone(),
        )
        .await?;
        journal.commit_maintenance().await?;
        journal.sync_bootstrap(next.clone(), false).await?;
        record.insert("phase", "succeeded".into());
        self.save(root, record).await?;
        Ok(record.clone())
    }
}
