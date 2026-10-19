//! A published bootstrap instance file is the durable restart request. Markers
//! suppress only the same still-live Windows boot for one minute; a later boot
//! consumes the file without another scheduler action.
use crate::{
    IvyError, IvyJson, IvyResult, Outcome, bootstrap, config, exec_file, layout, node_path, process,
};
use futures_util::future::BoxFuture;
use ivy_contracts::{JsString, core_schema::validate_host, json};
use ivy_sdk::clock::{Clock, parse_date};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    path::PathBuf,
    sync::Arc,
    time::{Duration, UNIX_EPOCH},
};

/// Capture process_started_at at the service entrypoint, before configuration
/// admission. A reconciler's creation time is not the process start time.
#[derive(Clone)]
pub struct RestartContext {
    pub own_instance_id: Option<JsString>,
    pub distribution: Option<PathBuf>,
    pub process_started_at: f64,
    pub platform: String,
}

/// Only scheduler observation/commands vary in fixtures. File admission, durable
/// markers, current-state matching and restart decisions always use this module.
pub trait BootstrapRestartOs: Send + Sync {
    fn inspect(
        &self,
        plans: Vec<IvyJson>,
        distribution: PathBuf,
    ) -> BoxFuture<'static, IvyResult<Vec<IvyJson>>>;
    fn command(
        &self,
        action: &'static str,
        name: JsString,
        distribution: PathBuf,
    ) -> BoxFuture<'static, IvyResult<()>>;
}

pub struct NativeBootstrapRestartOs {
    job_launcher: PathBuf,
}
impl NativeBootstrapRestartOs {
    pub fn new(job_launcher: PathBuf) -> Self {
        Self { job_launcher }
    }
}
fn unknown(message: &str) -> IvyError {
    let mut error = IvyError::new("bootstrap_os_unavailable", message);
    error.outcome = Outcome::Unknown;
    error
}
fn system() -> PathBuf {
    std::env::var_os("SystemRoot")
        .unwrap_or_else(|| "C:\\Windows".into())
        .into()
}
fn options(job_launcher: PathBuf) -> exec_file::FileOptions {
    exec_file::FileOptions {
        job_launcher: Some(job_launcher),
        environment: Some(process::runtime_environment(&BTreeMap::new())),
        ..Default::default()
    }
}
impl NativeBootstrapRestartOs {
    pub(crate) fn call(
        &self,
        action: &'static str,
        data: IvyJson,
        distribution: PathBuf,
    ) -> BoxFuture<'static, IvyResult<IvyJson>> {
        let helper = self.job_launcher.clone();
        Box::pin(async move {
            let command = exec_file::FileCommand {
                executable: system()
                    .join("System32/WindowsPowerShell/v1.0/powershell.exe")
                    .into_os_string(),
                args: [
                    "-NoProfile",
                    "-NonInteractive",
                    "-ExecutionPolicy",
                    "Bypass",
                    "-File",
                ]
                .into_iter()
                .map(OsString::from)
                .chain([distribution
                    .join("dist/packages/host-runtime/assets/bootstrap-maintenance-windows.ps1")
                    .into_os_string()])
                .collect(),
                cwd: None,
                timeout_ms: 60000,
                maximum_buffer_bytes: 2 * 1024 * 1024,
            };
            let input = json::stringify(
                &IvyJson::object([("action", action.into()), ("data", data)]),
                usize::MAX,
            )?
            .into_bytes();
            let result = exec_file::exec_file_input(command, distribution, options(helper), input).await
                .map_err(|_| unknown("The bounded scheduler operation did not complete. Inspect the retained maintenance record."))?;
            if result.failure.is_some() {
                return Err(unknown(
                    "The bounded scheduler operation did not complete. Inspect the retained maintenance record.",
                ));
            }
            let invalid = || unknown("Scheduler returned an invalid result.");
            let result = json::parse(&result.stdout.to_utf8().map_err(|_| invalid())?, usize::MAX)
                .map_err(|_| invalid())?;
            if !truthy(field(&result, "ok")) {
                return Err(IvyError::new_js(
                    result
                        .required("code")
                        .ok()
                        .and_then(IvyJson::as_js_string)
                        .cloned()
                        .ok_or_else(invalid)?,
                    result
                        .required("message")
                        .ok()
                        .and_then(IvyJson::as_js_string)
                        .cloned()
                        .ok_or_else(invalid)?,
                ));
            }
            Ok(result.get("value").cloned().unwrap_or(IvyJson::Null))
        })
    }
}
impl BootstrapRestartOs for NativeBootstrapRestartOs {
    fn inspect(
        &self,
        plans: Vec<IvyJson>,
        distribution: PathBuf,
    ) -> BoxFuture<'static, IvyResult<Vec<IvyJson>>> {
        let call = self.call(
            "inspect",
            IvyJson::object([("plans", IvyJson::Array(plans))]),
            distribution,
        );
        Box::pin(async move {
            call.await?
                .as_array()
                .map(<[IvyJson]>::to_vec)
                .ok_or_else(IvyError::internal)
        })
    }
    fn command(
        &self,
        action: &'static str,
        name: JsString,
        distribution: PathBuf,
    ) -> BoxFuture<'static, IvyResult<()>> {
        let helper = self.job_launcher.clone();
        Box::pin(async move {
            let command = exec_file::FileCommand {
                executable: system().join("System32/schtasks.exe").into_os_string(),
                args: vec![
                    action.into(),
                    "/TN".into(),
                    node_path::to_native(&name).into_os_string(),
                ],
                cwd: None,
                timeout_ms: 30000,
                maximum_buffer_bytes: 65536,
            };
            match exec_file::exec_file(command, distribution, options(helper)).await {
                Ok(result) if result.failure.is_none() => Ok(()),
                _ => Err(unknown(
                    "The selected Windows bootstrap task could not be restarted.",
                )),
            }
        })
    }
}

struct Inner {
    context: RestartContext,
    clock: Arc<dyn Clock>,
    os: Arc<dyn BootstrapRestartOs>,
    sync: tokio::sync::Mutex<()>,
}
#[derive(Clone)]
pub struct BootstrapConfigurationRestarts(Arc<Inner>);
impl BootstrapConfigurationRestarts {
    pub fn new(context: RestartContext, clock: Arc<dyn Clock>, job_launcher: PathBuf) -> Self {
        Self::with_os(
            context,
            clock,
            Arc::new(NativeBootstrapRestartOs::new(job_launcher)),
        )
    }
    pub fn with_os(
        context: RestartContext,
        clock: Arc<dyn Clock>,
        os: Arc<dyn BootstrapRestartOs>,
    ) -> Self {
        Self(Arc::new(Inner {
            context,
            clock,
            os,
            sync: tokio::sync::Mutex::new(()),
        }))
    }
    /// A dropped response waiter cannot abandon a published marker halfway
    /// through its admitted scheduler operation. Close must retain this wait.
    pub async fn reconcile(&self, host: IvyJson) -> IvyResult<bool> {
        let inner = self.0.clone();
        tokio::spawn(async move {
            let _guard = inner.sync.lock().await;
            reconcile(
                host,
                &inner.context,
                inner.clock.as_ref(),
                inner.os.as_ref(),
            )
            .await
        })
        .await
        .map_err(|_| IvyError::internal())?
    }
}

/// Exact Windows End/observe/Run ordering, with retained immutable ownership
/// inspection before every state observation. No command retry is introduced.
pub async fn restart_windows_owner(
    plans: Vec<IvyJson>,
    name: JsString,
    distribution: PathBuf,
    clock: &dyn Clock,
    os: &dyn BootstrapRestartOs,
) -> IvyResult<()> {
    let owners = os.inspect(plans.clone(), distribution.clone()).await?;
    let owner = owners
        .iter()
        .find(|owner| owner.get("name") == Some(&IvyJson::String(name.clone())));
    require(
        owner.is_some_and(|owner| truthy(field(owner, "enabled"))),
        "target_conflict",
        "The selected Windows bootstrap task is not enabled.",
    )?;
    if owner.is_some_and(|owner| truthy(field(owner, "running"))) {
        os.command("/End", name.clone(), distribution.clone())
            .await?;
    }
    let deadline = clock.utc_millis() + 30000;
    loop {
        let owners = os.inspect(plans.clone(), distribution.clone()).await?;
        if !owners
            .iter()
            .find(|owner| owner.get("name") == Some(&IvyJson::String(name.clone())))
            .is_some_and(|owner| truthy(field(owner, "running")))
        {
            break;
        }
        require(
            clock.utc_millis() < deadline,
            "outcome_unknown",
            "Windows bootstrap task did not stop before restart.",
        )?;
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    os.command("/Run", name, distribution).await
}

async fn reconcile(
    host: IvyJson,
    context: &RestartContext,
    clock: &dyn Clock,
    os: &dyn BootstrapRestartOs,
) -> IvyResult<bool> {
    let plans = bootstrap::retained_bootstrap_plans(host.clone()).await?;
    let Some(plan) = plans.first() else {
        return Ok(false);
    };
    let mut restart_own = false;
    for owner in plan
        .required("processes")?
        .as_array()
        .ok_or_else(IvyError::internal)?
    {
        let component = field(owner, "componentId");
        if !truthy(field(owner, "configPath"))
            || !["host-executor", "service-manager"]
                .iter()
                .any(|value| component == &IvyJson::from(*value))
        {
            continue;
        }
        let instance = host
            .required("instances")?
            .as_array()
            .ok_or_else(IvyError::internal)?
            .iter()
            .find(|instance| instance.get("instanceId") == owner.get("instanceId"));
        require(
            instance.is_some_and(|instance| instance.get("componentId") == Some(component)),
            "target_conflict",
            "Selected bootstrap owner is absent from the host configuration.",
        )?;
        let instance = instance.unwrap();
        require(
            truthy(field(owner, "candidateId")) && truthy(field(owner, "artifactRoot")),
            "target_conflict",
            "Selected bootstrap owner has no immutable release.",
        )?;
        let config_path = config::path_value(owner.required("configPath")?)?;
        let current = config::json_file(config_path.clone(), config::CONFIGURATION_BYTES).await?;
        validate_host("InstanceConfig", &current)?;
        let paths = layout::service_paths(&host, instance)?;
        require(
            ["instanceId", "componentId"]
                .iter()
                .all(|key| current.get(key) == instance.get(key))
                && current.get("hostId") == host.get("hostId")
                && current.get("buildId") == owner.get("candidateId")
                && current.get("artifactRoot") == owner.get("artifactRoot")
                && current.get("dataRoot") == paths.get("data")
                && current.get("workRoot") == paths.get("work")
                && current.get("logsRoot") == paths.get("logs"),
            "target_conflict",
            "Bootstrap instance configuration differs from the selected installation.",
        )?;
        let mut desired = current.clone();
        desired.insert("serviceNodeId", instance.required("serviceNodeId")?.clone());
        desired.insert("publicBaseUrl", host.required("publicBaseUrl")?.clone());
        let mut settings = instance.required("settings")?.clone();
        if settings.get("hostConfigPath").is_none_or(IvyJson::is_null) {
            settings.insert(
                "hostConfigPath",
                layout::host_configuration_path_for_style(&host, node_path::Style::native())?,
            );
        }
        desired.insert("settings", settings);
        if let Some(credential) = instance.get("credential").filter(|value| truthy(value)) {
            desired.insert("credential", credential.clone());
        } else {
            desired.remove("credential");
        }
        validate_host("InstanceConfig", &desired)?;
        if json::canonical(&current, usize::MAX)? != json::canonical(&desired, usize::MAX)? {
            config::atomic_json(config_path.clone(), desired).await?;
        }
        let health_path = config::path_value(paths.required("data")?)?.join("health.json");
        let Some(health) =
            config::json_file_if_present(health_path, config::CONFIGURATION_BYTES).await?
        else {
            continue;
        };
        // JSON null is absent here too: the original tests health truthiness.
        if !truthy(&health) {
            continue;
        }
        validate_host("Health", &health)?;
        require(
            ["instanceId", "componentId"]
                .iter()
                .all(|key| health.get(key) == instance.get(key)),
            "target_conflict",
            "Bootstrap health belongs to another instance.",
        )?;
        let written_at = tokio::task::spawn_blocking(move || {
            let metadata = std::fs::metadata(node_path::filesystem_path(&node_path::from_native(
                &config_path,
            )))
            .map_err(|_| IvyError::internal())?;
            let time = metadata.modified().map_err(|_| IvyError::internal())?;
            let millis = |value: Duration| {
                value.as_secs() as f64 * 1000.0 + value.subsec_nanos() as f64 / 1_000_000.0
            };
            Ok::<_, IvyError>(match time.duration_since(UNIX_EPOCH) {
                Ok(value) => millis(value),
                Err(error) => -millis(error.duration()),
            })
        })
        .await
        .map_err(|_| IvyError::internal())??;
        if date(&health, "startedAt")? >= written_at {
            continue;
        }
        if component == &"host-executor".into()
            && instance.get("instanceId")
                == context
                    .own_instance_id
                    .as_ref()
                    .map(|id| IvyJson::String(id.clone()))
                    .as_ref()
        {
            restart_own = written_at > context.process_started_at;
        } else if context.platform == "win32"
            && component == &"service-manager".into()
            && let Some(distribution) = &context.distribution
            && clock.utc_millis() as f64 - date(&health, "observedAt")? < 15000.0
        {
            let id = instance
                .required("instanceId")?
                .as_js_string()
                .ok_or_else(IvyError::internal)?;
            let marker_path = config::path_value(host.required("runtimeRoot")?)?
                .join("bootstrap")
                .join(node_path::to_native(&concat(&[
                    "restart-".into(),
                    id.clone(),
                    ".json".into(),
                ])));
            let marker =
                config::json_file_if_present(marker_path.clone(), config::CONFIGURATION_BYTES)
                    .await?;
            let pending = if let Some(marker) = &marker {
                marker.get("instanceId") == instance.get("instanceId")
                    && marker.get("configWrittenAt") == Some(&IvyJson::Number(written_at))
                    && marker.get("priorBootId") == health.get("bootId")
                    && clock.utc_millis() as f64 - date(marker, "requestedAt")? < 60000.0
            } else {
                false
            };
            if pending {
                continue;
            }
            let stamp = chrono::DateTime::from_timestamp_millis(clock.utc_millis())
                .ok_or_else(IvyError::internal)?
                .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
            config::atomic_json(
                marker_path,
                IvyJson::object([
                    ("instanceId", instance.required("instanceId")?.clone()),
                    ("configWrittenAt", IvyJson::Number(written_at)),
                    ("priorBootId", health.required("bootId")?.clone()),
                    ("requestedAt", stamp.into()),
                ]),
            )
            .await?;
            restart_windows_owner(
                plans.clone(),
                owner
                    .required("name")?
                    .as_js_string()
                    .cloned()
                    .ok_or_else(IvyError::internal)?,
                distribution.clone(),
                clock,
                os,
            )
            .await?;
        }
    }
    Ok(restart_own)
}
fn require(ok: bool, code: &str, message: &str) -> IvyResult<()> {
    if ok {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn field<'a>(value: &'a IvyJson, key: &str) -> &'a IvyJson {
    static NULL: IvyJson = IvyJson::Null;
    value.get(key).unwrap_or(&NULL)
}
fn date(value: &IvyJson, key: &str) -> IvyResult<f64> {
    value
        .get(key)
        .map(|value| json::try_js_string(value).map(|text| parse_date(&text)))
        .unwrap_or(Ok(f64::NAN))
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
fn concat(parts: &[JsString]) -> JsString {
    JsString(
        parts
            .iter()
            .flat_map(|part| part.0.iter().copied())
            .collect(),
    )
}
