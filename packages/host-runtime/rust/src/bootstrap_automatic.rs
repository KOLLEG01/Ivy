//! Durable exact-request handoff to an independent bootstrap maintenance owner.
//! The maintenance/OS implementation is supplied explicitly; there is no JS
//! worker or live-deployment fallback in this orchestration.
use crate::{
    IvyError, IvyJson, IvyResult, artifact::FileCompare, backup_files, bootstrap, config,
    journal::HostJournal, node_path, package_updater::BootstrapPackageComponent,
};
use futures_util::future::BoxFuture;
use ivy_contracts::{JsString, core_schema::validate_host, hash::hash_json};
use ivy_sdk::clock::{Clock, SystemClock};
use std::{path::PathBuf, sync::Arc};
use tokio::sync::Mutex;

pub trait AutomaticBootstrapRuntime: Send + Sync {
    fn inspect(
        &self,
        config_path: PathBuf,
        distribution: PathBuf,
    ) -> BoxFuture<'_, IvyResult<IvyJson>>;
    fn launch_automatic(
        &self,
        plan: IvyJson,
        request_path: PathBuf,
        operation_id: String,
    ) -> Option<BoxFuture<'_, IvyResult<()>>>;
}
fn require(value: bool, code: &str, message: &str) -> IvyResult<()> {
    if value {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn stamp(clock: &dyn Clock) -> IvyResult<IvyJson> {
    Ok(chrono::DateTime::from_timestamp_millis(clock.utc_millis())
        .ok_or_else(IvyError::internal)?
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
        .into())
}
pub fn automatic_bootstrap_root(config: &IvyJson, operation_id: &JsString) -> IvyResult<PathBuf> {
    require(
        !operation_id.0.is_empty() && operation_id.0.len() <= 128,
        "invalid_arguments",
        "Automatic bootstrap operation identity is invalid.",
    )?;
    Ok(config::path_value(config.required("runtimeRoot")?)?
        .join("bootstrap-automatic")
        .join(&hash_json(&IvyJson::String(operation_id.clone()))?[7..]))
}
pub async fn automatic_bootstrap_record(
    config: &IvyJson,
    operation_id: &JsString,
) -> IvyResult<Option<IvyJson>> {
    let path = automatic_bootstrap_root(config, operation_id)?.join("record.json");
    if !backup_files::exists(&path).await? {
        return Ok(None);
    }
    let value = config::json_file(path, config::CONFIGURATION_BYTES).await?;
    if value.is_null() {
        return Err(IvyError::internal());
    }
    let mut valid = value.get("schemaVersion") == Some(&IvyJson::Number(1.0))
        && value.get("operationId") == Some(&IvyJson::String(operation_id.clone()));
    if valid {
        // Keep the original startsWith failure for a non-string request hash.
        valid = value
            .required("requestHash")?
            .as_js_string()
            .ok_or_else(IvyError::internal)?
            .0
            .starts_with(&"sha256:".encode_utf16().collect::<Vec<_>>())
            && ["phase", "createdAt", "updatedAt"]
                .iter()
                .all(|key| value.get(key).and_then(IvyJson::as_js_string).is_some());
    }
    require(
        valid,
        "storage_invalid",
        "Automatic bootstrap record is invalid.",
    )?;
    Ok(Some(value))
}
pub async fn save_automatic_bootstrap_record(
    config: &IvyJson,
    value: &mut IvyJson,
    clock: &dyn Clock,
) -> IvyResult<()> {
    value.insert("updatedAt", stamp(clock)?);
    let operation = value
        .required("operationId")?
        .as_js_string()
        .ok_or_else(IvyError::internal)?;
    config::atomic_json(
        automatic_bootstrap_root(config, operation)?.join("record.json"),
        value.clone(),
    )
    .await
}
struct Inner {
    config_path: PathBuf,
    distribution: PathBuf,
    journal: HostJournal,
    runtime: Arc<dyn AutomaticBootstrapRuntime>,
    compare: FileCompare,
    clock: Arc<dyn Clock>,
    job_launcher: PathBuf,
    reconcile: Mutex<()>,
}
#[derive(Clone)]
pub struct AutomaticBootstrapUpdater(Arc<Inner>);
impl AutomaticBootstrapUpdater {
    pub fn new(
        config_path: PathBuf,
        distribution: PathBuf,
        journal: HostJournal,
        runtime: Arc<dyn AutomaticBootstrapRuntime>,
        compare: FileCompare,
        job_launcher: PathBuf,
    ) -> Self {
        Self::with_clock(
            config_path,
            distribution,
            journal,
            runtime,
            compare,
            job_launcher,
            Arc::new(SystemClock),
        )
    }
    pub fn with_clock(
        config_path: PathBuf,
        distribution: PathBuf,
        journal: HostJournal,
        runtime: Arc<dyn AutomaticBootstrapRuntime>,
        compare: FileCompare,
        job_launcher: PathBuf,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self(Arc::new(Inner {
            config_path,
            distribution,
            journal,
            runtime,
            compare,
            clock,
            job_launcher,
            reconcile: Mutex::new(()),
        }))
    }
    /// Keep request publication and its independent-owner launch under one live
    /// owner when a caller stops waiting. Replays never recapture prior state.
    pub async fn reconcile(&self, selected: Vec<BootstrapPackageComponent>) -> IvyResult<bool> {
        let inner = self.0.clone();
        tokio::spawn(async move {
            let _guard = inner.reconcile.lock().await;
            inner.reconcile(selected).await
        })
        .await
        .map_err(|_| IvyError::internal())?
    }
}
impl Inner {
    async fn reconcile(&self, selected: Vec<BootstrapPackageComponent>) -> IvyResult<bool> {
        if selected.is_empty() || !self.journal.unfinished().await?.is_empty() {
            return Ok(false);
        }
        let config = self.journal.config().await?;
        let path = node_path::Context::current()
            .map_err(|_| IvyError::internal())?
            .resolve(&[node_path::from_native(&self.config_path)]);
        let retained = bootstrap::retained_bootstrap_plans(config.clone()).await?;
        let current = retained.first().ok_or_else(|| {
            IvyError::new(
                "not_found",
                "Automatic bootstrap update needs the retained current OS plan.",
            )
        })?;
        let executor = config
            .required("instances")?
            .as_array()
            .ok_or_else(IvyError::internal)?
            .iter()
            .find(|value| value.get("componentId") == Some(&"host-executor".into()))
            .ok_or_else(|| {
                IvyError::new(
                    "not_found",
                    "Automatic bootstrap update needs the configured HostExecutor.",
                )
            })?;
        let executor_id = executor.required("instanceId")?.as_str()?;
        let requested = selected
            .iter()
            .find(|value| value.instance_id == executor_id);
        let installed = self.journal.installed(&executor_id).await?;
        require(
            requested.is_some() || installed.is_some(),
            "not_found",
            "Automatic bootstrap update needs the installed HostExecutor release.",
        )?;
        let host_candidate = match requested {
            Some(value) => value.candidate.clone(),
            None => {
                self.journal
                    .candidate(&installed.unwrap().required("candidateId")?.as_str()?)
                    .await?
            }
        };
        let mut preferred = selected
            .iter()
            .map(|value| {
                IvyJson::object([
                    ("candidate", value.candidate.clone()),
                    ("manifest", value.manifest.clone()),
                ])
            })
            .collect::<Vec<_>>();
        if !preferred.iter().any(|value| {
            value
                .get("candidate")
                .and_then(|value| value.get("candidateId"))
                == host_candidate.get("candidateId")
        }) {
            preferred.insert(
                0,
                IvyJson::object([
                    ("candidate", host_candidate.clone()),
                    (
                        "manifest",
                        self.journal
                            .manifest(&host_candidate.required("candidateId")?.as_str()?)
                            .await?,
                    ),
                ]),
            );
        }
        let components = bootstrap::bootstrap_components(
            &config,
            &self.journal,
            &host_candidate,
            &preferred,
            &self.compare,
        )
        .await?;
        let budget = current
            .get("linuxResources")
            .and_then(|value| value.get("budget"));
        let next = bootstrap::configuration_bootstrap_plan(
            &config,
            &path,
            &host_candidate,
            budget,
            &components,
        )?;
        let desired = next
            .required("processes")?
            .as_array()
            .unwrap()
            .iter()
            .map(|value| {
                Ok((
                    value
                        .required("instanceId")?
                        .as_js_string()
                        .ok_or_else(IvyError::internal)?
                        .clone(),
                    value
                        .get("candidateId")
                        .filter(|v| !v.is_null())
                        .unwrap_or(next.required("candidateId")?)
                        .clone(),
                ))
            })
            .collect::<IvyResult<Vec<_>>>()?;
        if current
            .required("processes")?
            .as_array()
            .unwrap()
            .iter()
            .all(|value| {
                desired
                    .iter()
                    .find(|(id, _)| {
                        Some(id) == value.get("instanceId").and_then(IvyJson::as_js_string)
                    })
                    .is_some_and(|(_, candidate)| {
                        Some(candidate)
                            == value
                                .get("candidateId")
                                .filter(|v| !v.is_null())
                                .or_else(|| current.get("candidateId"))
                    })
            })
        {
            return Ok(false);
        }
        let mut releases = desired;
        releases.sort_by(|left, right| (self.compare)(&left.0, &right.0));
        let hash = hash_json(&IvyJson::object([
            ("hostId", config.required("hostId")?.clone()),
            ("config", next.required("configHash")?.clone()),
            (
                "releases",
                IvyJson::Array(
                    releases
                        .into_iter()
                        .map(|(id, release)| IvyJson::Array(vec![IvyJson::String(id), release]))
                        .collect(),
                ),
            ),
        ]))?;
        let operation = format!("bootstrap-package-{}", &hash[7..47]);
        let operation_js = JsString::from(operation.as_str());
        let root = automatic_bootstrap_root(&config, &operation_js)?;
        let request_path = root.join("request.json");
        let existing = automatic_bootstrap_record(&config, &operation_js).await?;
        if existing.as_ref().is_some_and(|value| {
            ["succeeded", "rolled_back", "needs_attention"]
                .iter()
                .any(|phase| value.get("phase") == Some(&IvyJson::from(*phase)))
        }) {
            return Ok(false);
        }
        let parent = root.parent().ok_or_else(IvyError::internal)?;
        if !backup_files::exists(parent).await? {
            backup_files::private_directory(parent.to_owned(), self.job_launcher.clone()).await?;
        }
        if !backup_files::exists(&root).await? {
            backup_files::private_directory(root.clone(), self.job_launcher.clone()).await?;
        }
        let request = if backup_files::exists(&request_path).await? {
            let request =
                config::json_file(request_path.clone(), config::CONFIGURATION_BYTES).await?;
            validate_host("BootstrapMaintenanceRequest", &request)?;
            require(
                request.get("operationId") == Some(&operation.clone().into())
                    && hash_json(request.required("next")?)? == hash_json(&next)?,
                "operation_conflict",
                "Automatic bootstrap operation belongs to another exact target.",
            )?;
            request
        } else {
            require(
                existing.is_none(),
                "storage_invalid",
                "Automatic bootstrap record has no retained exact request.",
            )?;
            let previous = self
                .runtime
                .inspect(node_path::to_native(&path), self.distribution.clone())
                .await?;
            let request = IvyJson::object([
                ("schemaVersion", IvyJson::Number(1.0)),
                ("operationId", operation.clone().into()),
                ("previous", previous),
                ("next", next),
            ]);
            validate_host("BootstrapMaintenanceRequest", &request)?;
            config::atomic_json(request_path.clone(), request.clone()).await?;
            request
        };
        let request_hash = hash_json(&request)?;
        require(
            existing
                .as_ref()
                .is_none_or(|value| value.get("requestHash") == Some(&request_hash.clone().into())),
            "operation_conflict",
            "Automatic bootstrap operation belongs to another exact request.",
        )?;
        let was_present = existing.is_some();
        let now = stamp(self.clock.as_ref())?;
        let mut record = existing.unwrap_or_else(|| {
            IvyJson::object([
                ("schemaVersion", IvyJson::Number(1.0)),
                ("operationId", operation.clone().into()),
                ("requestHash", request_hash.into()),
                ("phase", "launched".into()),
                ("errorCode", IvyJson::Null),
                ("createdAt", now.clone()),
                ("updatedAt", now),
            ])
        });
        if !was_present {
            save_automatic_bootstrap_record(&config, &mut record, self.clock.as_ref()).await?;
        }
        let launch = self
            .runtime
            .launch_automatic(current.clone(), request_path, operation)
            .ok_or_else(|| {
                IvyError::new(
                    "unsupported_runtime",
                    "This host has no independent automatic bootstrap handoff.",
                )
            })?;
        match launch.await {
            Ok(()) => Ok(true),
            Err(error) => {
                if !was_present {
                    record.insert("phase", "outcome_unknown".into());
                    record.insert("errorCode", IvyJson::String(error.code_js()));
                    save_automatic_bootstrap_record(&config, &mut record, self.clock.as_ref())
                        .await?;
                }
                Err(error)
            }
        }
    }
}
