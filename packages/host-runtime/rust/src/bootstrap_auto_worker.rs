//! Independent automatic-bootstrap worker policy. Production entrypoints run
//! this in a separate native process; no Node/TypeScript worker is invoked here.
use crate::{
    IvyError, IvyJson, IvyResult, Outcome, bootstrap, bootstrap_automatic,
    bootstrap_maintenance::BootstrapMaintenance, config, host_config, journal::HostJournal, layout,
    node_path,
};
use futures_util::future::BoxFuture;
use ivy_contracts::{JsString, core_schema::validate_host, hash::hash_json};
use ivy_sdk::clock::{Clock, parse_date};
use std::{collections::BTreeMap, path::PathBuf, sync::Arc, time::Duration};

fn require(value: bool, code: &str, message: &str) -> IvyResult<()> {
    if value {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn require_succeeded(result: &IvyJson, fallback: &str, message: &str) -> IvyResult<()> {
    if result.get("phase") == Some(&"succeeded".into()) {
        return Ok(());
    }
    let code = match result.get("errorCode").filter(|value| !value.is_null()) {
        Some(value) => value
            .as_js_string()
            .cloned()
            .ok_or_else(IvyError::internal)?,
        None => fallback.into(),
    };
    Err(IvyError::new_js(code, message.into()))
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
fn chosen<'a>(plan: &'a IvyJson, instance: &IvyJson) -> IvyResult<Option<&'a IvyJson>> {
    Ok(array(plan.required("processes")?)?
        .iter()
        .find(|value| value.get("instanceId") == Some(instance))
        .map(|value| {
            value
                .get("candidateId")
                .filter(|value| !value.is_null())
                .unwrap_or(plan.get("candidateId").unwrap())
        }))
}
async fn matching_plan(config: &IvyJson, snapshot: &IvyJson) -> IvyResult<IvyJson> {
    for plan in bootstrap::retained_bootstrap_plans(config.clone()).await? {
        let mut matches = true;
        for owner in array(snapshot.required("owners")?)? {
            if chosen(&plan, owner.required("instanceId")?)? != owner.get("candidateId") {
                matches = false;
                break;
            }
        }
        if matches {
            return Ok(plan);
        }
    }
    Err(IvyError::new(
        "not_found",
        "The prior exact bootstrap plan is not retained.",
    ))
}
pub trait BootstrapReadiness: Send + Sync {
    fn wait(
        &self,
        config: IvyJson,
        plan: IvyJson,
        states: IvyJson,
        not_before: i64,
        timeout_ms: i64,
    ) -> BoxFuture<'_, IvyResult<()>>;
}
pub struct NativeBootstrapReadiness {
    clock: Arc<dyn Clock>,
}
impl NativeBootstrapReadiness {
    pub fn new(clock: Arc<dyn Clock>) -> Self {
        Self { clock }
    }
}
impl BootstrapReadiness for NativeBootstrapReadiness {
    fn wait(
        &self,
        config: IvyJson,
        plan: IvyJson,
        states: IvyJson,
        not_before: i64,
        timeout_ms: i64,
    ) -> BoxFuture<'_, IvyResult<()>> {
        Box::pin(async move {
            let journal = HostJournal::open_with_clock(config.clone(), self.clock.clone()).await?;
            let result = async {
                let mut expected = BTreeMap::new();
                for process in array(plan.required("processes")?)? {
                    let candidate = journal.candidate(&process.get("candidateId").filter(|value| !value.is_null()).unwrap_or(plan.required("candidateId")?).as_str()?).await?;
                    expected.insert(js(process, "instanceId")?, candidate.required("buildId")?.clone());
                }
                let deadline = self.clock.utc_millis() + timeout_ms;
                loop {
                    let mut ready = true;
                    for state in array(states.required("owners")?)? {
                        let running = state.get("running").and_then(IvyJson::as_bool).ok_or_else(|| IvyError::new("invalid_arguments", "Bootstrap owner running state must be boolean."))?;
                        if !running { continue; }
                        let instance = array(config.required("instances")?)?.iter().find(|instance| instance.get("instanceId") == state.get("instanceId"))
                            .ok_or_else(|| IvyError::new("not_found", "Bootstrap instance disappeared."))?;
                        let check = async {
                            let paths = layout::service_paths(&config, instance)?;
                            let health = config::json_file(config::path_value(paths.required("data")?)?.join("health.json"), config::CONFIGURATION_BYTES).await?;
                            validate_host("Health", &health)?;
                            let observed = parse_date(&js(&health, "observedAt")?);
                            Ok::<_, IvyError>(observed.is_finite() && health.get("ready") == Some(&IvyJson::Bool(true)) && expected.get(&js(state, "instanceId")?) == health.get("buildId")
                                && !(observed < not_before as f64 || self.clock.utc_millis() as f64 - observed > 15000.0))
                        }.await;
                        if !check.unwrap_or(false) { ready = false; }
                    }
                    if ready { return Ok(()); }
                    require(self.clock.utc_millis() < deadline, "readiness_failed", "Updated bootstrap owners did not become ready before the rollback deadline.")?;
                    tokio::time::sleep(Duration::from_millis(500)).await;
                }
            }.await;
            journal.close().await?;
            result
        })
    }
}
#[derive(Clone)]
pub struct AutomaticBootstrapWorker {
    maintenance: BootstrapMaintenance,
    readiness: Arc<dyn BootstrapReadiness>,
}
impl AutomaticBootstrapWorker {
    pub fn new(maintenance: BootstrapMaintenance) -> Self {
        let readiness = Arc::new(NativeBootstrapReadiness::new(maintenance.clock().clone()));
        Self {
            maintenance,
            readiness,
        }
    }
    pub fn with_readiness(
        maintenance: BootstrapMaintenance,
        readiness: Arc<dyn BootstrapReadiness>,
    ) -> Self {
        Self {
            maintenance,
            readiness,
        }
    }
    /// Keep pause/update/recovery/resume owned after an observer disconnects.
    pub async fn run(&self, config_path: PathBuf, request_path: PathBuf) -> IvyResult<()> {
        let owner = self.clone();
        tokio::spawn(async move { owner.run_owned(config_path, request_path).await })
            .await
            .map_err(|_| IvyError::internal())?
    }
    async fn save(
        &self,
        config: &IvyJson,
        record: &mut IvyJson,
        phase: &str,
        error_code: IvyJson,
    ) -> IvyResult<()> {
        record.insert("phase", phase.into());
        record.insert("errorCode", error_code);
        bootstrap_automatic::save_automatic_bootstrap_record(
            config,
            record,
            self.maintenance.clock().as_ref(),
        )
        .await
    }
    async fn resume_ready(
        &self,
        config: &IvyJson,
        plan: &IvyJson,
        original: &IvyJson,
        plans: Vec<IvyJson>,
    ) -> IvyResult<()> {
        let at = self.maintenance.clock().utc_millis();
        self.maintenance
            .os()
            .resume(original.clone(), plans)
            .ok_or_else(IvyError::internal)?
            .await?;
        self.readiness
            .wait(config.clone(), plan.clone(), original.clone(), at, 90_000)
            .await
    }
    async fn rollback(
        &self,
        path: &PathBuf,
        config: &mut IvyJson,
        record: &mut IvyJson,
        request: &IvyJson,
        prior: &IvyJson,
        failure: &IvyError,
    ) -> IvyResult<()> {
        self.save(
            config,
            record,
            "rolling_back",
            IvyJson::String(failure.code_js()),
        )
        .await?;
        *config = host_config::host_config(path.clone()).await?;
        let mut plans = bootstrap::retained_bootstrap_plans(config.clone()).await?;
        plans.extend([request.required("next")?.clone(), prior.clone()]);
        let current = self.maintenance.inspect(path.clone()).await?;
        self.maintenance
            .os()
            .pause(current.clone(), plans)
            .ok_or_else(IvyError::internal)?
            .await?;
        let mut operation_id = js(request, "operationId")?;
        operation_id.0.extend("-rollback".encode_utf16());
        let rollback_request = IvyJson::object([
            ("schemaVersion", IvyJson::Number(1.0)),
            ("operationId", IvyJson::String(operation_id)),
            ("previous", current),
            ("next", prior.clone()),
        ]);
        validate_host("BootstrapMaintenanceRequest", &rollback_request)?;
        let result = self
            .maintenance
            .update(path.clone(), rollback_request)
            .await?;
        require_succeeded(
            &result,
            "bootstrap_rollback_failed",
            "Automatic bootstrap rollback did not succeed.",
        )?;
        *config = host_config::host_config(path.clone()).await?;
        let plans = bootstrap::retained_bootstrap_plans(config.clone()).await?;
        self.resume_ready(config, prior, request.required("previous")?, plans)
            .await?;
        self.save(
            config,
            record,
            "rolled_back",
            IvyJson::String(failure.code_js()),
        )
        .await
    }
    async fn run_owned(&self, config_path: PathBuf, request_path: PathBuf) -> IvyResult<()> {
        let cwd = std::env::current_dir().map_err(|_| IvyError::internal())?;
        let resolve = |path: PathBuf| {
            let path = cwd.join(path);
            let absolute = std::path::absolute(&path).unwrap_or(path);
            node_path::lexical(&absolute)
        };
        let path = resolve(config_path);
        let request_path = resolve(request_path);
        let request = config::json_file(request_path, config::CONFIGURATION_BYTES).await?;
        validate_host("BootstrapMaintenanceRequest", &request)?;
        let mut config = host_config::host_config(path.clone()).await?;
        let record =
            bootstrap_automatic::automatic_bootstrap_record(&config, &js(&request, "operationId")?)
                .await?;
        let exact = match &record {
            Some(record) => record.get("requestHash") == Some(&hash_json(&request)?.into()),
            None => false,
        };
        require(
            exact,
            "storage_invalid",
            "Automatic bootstrap request has no exact retained record.",
        )?;
        let mut record = record.unwrap();
        if ["succeeded", "rolled_back"]
            .iter()
            .any(|phase| record.get("phase") == Some(&IvyJson::from(*phase)))
        {
            return Ok(());
        }
        require(
            record.get("phase") != Some(&"needs_attention".into()),
            "bootstrap_update_failed",
            "Automatic bootstrap maintenance needs explicit attention.",
        )?;
        require(
            self.maintenance.os().can_pause_and_resume(),
            "unsupported_runtime",
            "This host cannot hand bootstrap ownership to offline maintenance.",
        )?;
        let original = request.required("previous")?;
        let prior = matching_plan(&config, original).await?;
        let starting = text(&record, "phase")?;
        let normal = async {
            if ["updating", "verifying", "rolling_back"].contains(&starting.as_str()) {
                return self.rollback(&path, &mut config, &mut record, &request, &prior,
                    &IvyError::new("outcome_unknown", "An interrupted automatic bootstrap update is rolled back before normal execution resumes.").with_outcome(Outcome::Unknown)).await;
            }
            self.save(&config, &mut record, "checking", IvyJson::Null).await?;
            let mut plans = bootstrap::retained_bootstrap_plans(config.clone()).await?; plans.push(request.required("next")?.clone());
            self.maintenance.os().pause(original.clone(), plans).ok_or_else(IvyError::internal)?.await?;
            self.save(&config, &mut record, "updating", IvyJson::Null).await?;
            let result = self.maintenance.update(path.clone(), request.clone()).await?;
            require_succeeded(&result, "bootstrap_update_failed", "Automatic bootstrap maintenance did not succeed.")?;
            config = host_config::host_config(path.clone()).await?;
            let plans = bootstrap::retained_bootstrap_plans(config.clone()).await?;
            let at = self.maintenance.clock().utc_millis();
            self.maintenance.os().resume(original.clone(), plans).ok_or_else(IvyError::internal)?.await?;
            self.save(&config, &mut record, "verifying", IvyJson::Null).await?;
            self.readiness.wait(config.clone(), request.required("next")?.clone(), original.clone(), at, 90_000).await?;
            self.save(&config, &mut record, "succeeded", IvyJson::Null).await
        }.await;
        let Err(failure) = normal else {
            return Ok(());
        };
        let recovery = async {
            if record.get("phase") == Some(&"checking".into()) {
                config = host_config::host_config(path.clone()).await?;
                let mut plans = bootstrap::retained_bootstrap_plans(config.clone()).await?;
                plans.extend([request.required("next")?.clone(), prior.clone()]);
                let current = self.maintenance.inspect(path.clone()).await?;
                let states = array(current.required("owners")?)?;
                let expected = array(original.required("owners")?)?;
                require(
                    states.len() == expected.len()
                        && states.iter().all(|owner| {
                            expected.iter().any(|state| {
                                ["instanceId", "name", "candidateId", "definitionHash"]
                                    .iter()
                                    .all(|key| state.get(key) == owner.get(key))
                            })
                        }),
                    "target_conflict",
                    "Bootstrap ownership changed before automatic maintenance acquired it.",
                )?;
                self.resume_ready(&config, &prior, original, plans).await?;
                self.save(
                    &config,
                    &mut record,
                    "rolled_back",
                    IvyJson::String(failure.code_js()),
                )
                .await
            } else {
                self.rollback(&path, &mut config, &mut record, &request, &prior, &failure)
                    .await
            }
        }
        .await;
        if let Err(error) = recovery {
            self.save(
                &config,
                &mut record,
                "needs_attention",
                IvyJson::String(error.code_js()),
            )
            .await?;
            return Err(error);
        }
        Ok(())
    }
}
