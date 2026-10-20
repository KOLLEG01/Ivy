//! Service admission and lifecycle. Production effects are mandatory inputs;
//! no locale/parser/transport or managed-process owner is substituted here.
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString};
use ivy_host_runtime::{
    bootstrap_auto_worker::AutomaticBootstrapWorker,
    bootstrap_maintenance::BootstrapMaintenance,
    bootstrap_restart::BootstrapConfigurationRestarts,
    config::{self, InstanceConfig},
    executor::{ActivationRuntime, ExecutorOptions, HostExecutor, PackageUpdateRuntime},
    health::HealthFile,
    host_config, layout,
    node_path::{self, Style},
    runtime_maintenance,
};
use ivy_sdk::clock::{Clock, SystemClock};
use std::{collections::BTreeMap, ffi::OsString, path::PathBuf, sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;

fn path(value: &IvyJson) -> IvyResult<PathBuf> {
    value
        .as_js_string()
        .map(node_path::to_native)
        .ok_or_else(|| {
            IvyError::new(
                "invalid_arguments",
                "Executor requires an explicit absolute host configuration.",
            )
        })
}
fn option(args: &[OsString], key: &str) -> Option<OsString> {
    args.iter()
        .position(|value| value == key)
        .and_then(|at| args.get(at + 1))
        .cloned()
}

/// Resolve the original executor argument/environment precedence. Instance
/// admission precedes host-file selection, including when --config is explicit.
pub async fn admit(
    args: &[OsString],
    environment: &BTreeMap<JsString, JsString>,
    os_home: &JsString,
    node: &ivy_host_runtime::node_runtime::NodeRuntime,
) -> IvyResult<Startup> {
    let own_path = if args.iter().any(|v| v == "--instance-config") {
        option(args, "--instance-config")
    } else {
        environment
            .get(&"IVY_INSTANCE_CONFIG".into())
            .map(|v| node_path::to_native(v).into_os_string())
    }
    .filter(|value| !value.is_empty());
    let own = match own_path {
        Some(value) => Some(config::instance_config(PathBuf::from(value), node).await?),
        None => None,
    };
    let host_path = if args.iter().any(|v| v == "--config") {
        option(args, "--config").map(PathBuf::from)
    } else if let Some(value) = own
        .as_ref()
        .and_then(|c| c.json().get("settings"))
        .and_then(|s| s.get("hostConfigPath"))
        .filter(|v| !v.is_null())
    {
        Some(path(value)?)
    } else if let Some(value) = environment.get(&"IVY_HOST_CONFIG".into()) {
        Some(node_path::to_native(value))
    } else {
        Some(node_path::to_native(&layout::default_host_config_path(
            environment,
            Style::native(),
            os_home,
        )?))
    }
    .filter(|value| value.is_absolute())
    .ok_or_else(|| {
        IvyError::new(
            "invalid_arguments",
            "Executor requires an explicit absolute host configuration.",
        )
    })?;
    let host = host_config::host_config(host_path.clone()).await?;
    if runtime_maintenance::runtime_reset_active(&host).await? {
        return Err(IvyError::new(
            "maintenance_active",
            "Host execution remains stopped for an incomplete runtime reset.",
        ));
    }
    Ok(Startup {
        host_path,
        host,
        own,
        launch_id: environment.get(&"IVY_LAUNCH_ID".into()).cloned(),
    })
}

pub struct Startup {
    pub host_path: PathBuf,
    pub host: IvyJson,
    pub own: Option<InstanceConfig>,
    pub launch_id: Option<JsString>,
}

/// Construct these from the qualified production preparation/HTTP/OS owners.
/// Packages are required when a credentialed executor has packageUpdates.
pub struct Bindings {
    pub activation: Arc<dyn ActivationRuntime>,
    pub options: ExecutorOptions,
    pub restarts: Option<BootstrapConfigurationRestarts>,
    pub packages: Option<PackageUpdateRuntime>,
}
impl Bindings {
    pub fn native(
        preparation: ivy_host_runtime::prepare::PreparationRuntime,
        distribution: PathBuf,
        job_launcher: PathBuf,
        inspector: Arc<dyn ivy_host_runtime::executor::OfflineHiveInspector>,
        options: ExecutorOptions,
        restarts: Option<BootstrapConfigurationRestarts>,
        packages: Option<PackageUpdateRuntime>,
    ) -> IvyResult<Self> {
        Ok(Self {
            activation: Arc::new(ivy_host_runtime::executor::NativeActivationRuntime::new(
                distribution,
                preparation,
                job_launcher,
                inspector,
            )?),
            options,
            restarts,
            packages,
        })
    }
}

/// Own startup, health/control and close even when a response observer disappears.
/// Explicit stop and coordinator completion both drain the same accepted work.
pub async fn serve(startup: Startup, bindings: Bindings, stop: CancellationToken) -> IvyResult<()> {
    tokio::spawn(serve_owned(startup, bindings, stop))
        .await
        .map_err(|_| IvyError::internal())?
}
async fn serve_owned(
    startup: Startup,
    mut bindings: Bindings,
    stop: CancellationToken,
) -> IvyResult<()> {
    let own_id = startup
        .own
        .as_ref()
        .map(|own| own.json().required("instanceId")?.as_str())
        .transpose()?;
    let credential = startup
        .own
        .as_ref()
        .and_then(|own| own.json().get("credential"))
        .map(|v| v.as_js_string().cloned().ok_or_else(IvyError::internal))
        .transpose()?;
    // HE02: the admitted configured instance wins; componentId is never its ID.
    bindings.options.own_instance_id = own_id;
    let updates = ivy_host_runtime::configuration_updater::update_credential(
        &startup.host,
        bindings.options.own_instance_id.as_deref(),
        credential.as_ref(),
    )?;
    if updates.as_ref().is_some_and(|v| !v.0.is_empty())
        && startup
            .host
            .get("packageUpdates")
            .is_some_and(|v| !v.is_null())
        && bindings.packages.is_none()
    {
        return Err(IvyError::new(
            "unsupported_runtime",
            "Package updates require the qualified production runtime bindings.",
        ));
    }
    let executor = if let Some(packages) = bindings.packages {
        HostExecutor::open_with_updates(
            startup.host,
            startup.host_path,
            credential,
            bindings.options,
            bindings.activation,
            Arc::new(SystemClock),
            bindings.restarts,
            packages,
        )
        .await?
    } else if let Some(restarts) = bindings.restarts {
        HostExecutor::open_with_configuration_and_restarts(
            startup.host,
            startup.host_path,
            credential,
            bindings.options,
            bindings.activation,
            Arc::new(SystemClock),
            restarts,
        )
        .await?
    } else {
        HostExecutor::open_with_configuration(
            startup.host,
            startup.host_path,
            credential,
            bindings.options,
            bindings.activation,
        )
        .await?
    };
    if let Err(error) = executor.start() {
        let _ = executor.close().await;
        return Err(error);
    }
    let health = startup
        .own
        .map(|own| HealthFile::with_launch_id(own, startup.launch_id));
    let mut pulse = tokio::time::interval_at(
        tokio::time::Instant::now() + Duration::from_secs(1),
        Duration::from_secs(1),
    );
    pulse.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let result = loop {
        tokio::select! {
            _ = stop.cancelled() => break Ok(()),
            result = executor.completion() => break result,
            _ = pulse.tick() => {
                // Original health errors are retried at the next serialized tick.
                if let Some(health) = &health {
                    let observed = async {
                        health.write(true, "".into(), None).await?;
                        health.control().await
                    }.await;
                    if matches!(observed, Ok(Some(_))) { break Ok(()); }
                }
            }
        }
    };
    let closed = executor.close().await;
    result.and(closed)
}

/// The independent worker executes the retained automatic update/recovery
/// policy. Its maintenance object must contain the real locale and OS bindings.
pub async fn run_worker(
    maintenance: BootstrapMaintenance,
    config: PathBuf,
    request: PathBuf,
) -> IvyResult<()> {
    AutomaticBootstrapWorker::new(maintenance)
        .run(config, request)
        .await
}

/// Install handlers before admitting startup and keep their cancellation token
/// separate from service work; a signal requests orderly close, never task abort.
pub fn install_signals(stop: CancellationToken) -> IvyResult<tokio::task::JoinHandle<()>> {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut terminate = signal(SignalKind::terminate()).map_err(|_| IvyError::internal())?;
        let mut interrupt = signal(SignalKind::interrupt()).map_err(|_| IvyError::internal())?;
        return Ok(tokio::spawn(async move {
            tokio::select! { _ = terminate.recv() => {}, _ = interrupt.recv() => {}, _ = stop.cancelled() => return }
            stop.cancel();
        }));
    }
    #[cfg(windows)]
    {
        use tokio::signal::windows::{ctrl_break, ctrl_c, ctrl_close, ctrl_shutdown};
        let mut interrupt = ctrl_c().map_err(|_| IvyError::internal())?;
        let mut terminate = ctrl_break().map_err(|_| IvyError::internal())?;
        let mut close = ctrl_close().map_err(|_| IvyError::internal())?;
        let mut shutdown = ctrl_shutdown().map_err(|_| IvyError::internal())?;
        return Ok(tokio::spawn(async move {
            tokio::select! {_ = interrupt.recv()=>{},_ = terminate.recv()=>{},_ = close.recv()=>{},_ = shutdown.recv()=>{},_ = stop.cancelled()=>return}
            stop.cancel();
        }));
    }
    #[cfg(not(any(windows, unix)))]
    Err(IvyError::new(
        "unsupported_runtime",
        "Native service signals require Windows or Unix.",
    ))
}

/// Capture this before Node/configuration discovery for bootstrap restart fences.
pub fn process_started_at() -> f64 {
    SystemClock.utc_millis() as f64
}
