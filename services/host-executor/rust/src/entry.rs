//! Native bootstrap process supervision. Configuration and health retain their
//! public formats; deployment preparation remains in the activation coordinator.
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString, Outcome, core_schema::validate_host};
use ivy_host_runtime::{
    config::{self, InstanceConfig},
    executor_lock::ExecutorLock,
    health::HealthFile,
    host_config, node_path,
    process::{self, Command, LaunchContext, OutputStream},
    runtime_maintenance,
    runtime_owner::stop::{shutdown_control, stop_child},
};
use std::{collections::BTreeMap, ffi::OsString, path::PathBuf, sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;

pub const USAGE: &str = "ivy-host-executor [--config ABS_HOST_CONFIG] [--instance-config ABS_HOST_INSTANCE_CONFIG] [--distribution ABS_ARTIFACT_ROOT] [--job-launcher ABS_EXECUTABLE]\nBootstrap: ivy-host-executor --config ABS_HOST_CONFIG --manager-executable ABS_EXECUTABLE --manager-config ABS_INSTANCE_CONFIG [--instance-config ABS_HOST_INSTANCE_CONFIG] [--job-launcher ABS_EXECUTABLE]";

/// The normal service owns the existing configuration, activation and update lanes.
/// Bootstrap-only direct Manager supervision below remains an explicit mode.
pub async fn run_native(args: &[OsString], stop: CancellationToken) -> IvyResult<()> {
    use ivy_host_runtime::{
        artifact, host_config::HostContext, native_plan, node_runtime::NodeRuntime,
        package_updater::NativePackageClient, portable_http::PortableHttp,
        prepare::PreparationRuntime,
    };
    let started = crate::process_started_at();
    let mut values = BTreeMap::new();
    let mut input = args.iter();
    while let Some(key) = input.next() {
        if ![
            "--config",
            "--instance-config",
            "--distribution",
            "--job-launcher",
        ]
        .iter()
        .any(|allowed| key == *allowed)
        {
            return Err(invalid(USAGE));
        }
        let value = input
            .next()
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .ok_or_else(|| invalid(USAGE))?;
        if values.insert(key.clone(), value).is_some() {
            return Err(invalid("Native service options may appear only once."));
        }
    }
    let context = HostContext::current()?;
    let startup = crate::admit(args, &context.environment, &context.account_home, None).await?;
    if let Some(own) = &startup.own {
        matching_instance(&startup.host, own, "host-executor")?;
    }
    let distribution = values
        .remove(&OsString::from("--distribution"))
        .or_else(|| {
            startup
                .own
                .as_ref()
                .map(|own| own.artifact_root().to_path_buf())
        })
        .ok_or_else(|| {
            invalid("Native host startup requires its instance configuration or --distribution.")
        })?;
    let configured = startup
        .host
        .required("executables")?
        .get("node")
        .map(path)
        .transpose()?;
    let node = NodeRuntime::discover_optional(configured.as_deref()).await?;
    let job = values
        .remove(&OsString::from("--job-launcher"))
        .unwrap_or_else(|| native_plan::native_binary_path(&distribution, "ivy-job"));
    let options = ivy_host_runtime::executor::ExecutorOptions {
        own_instance_id: None,
        account_home: context.account_home,
        build_id: startup
            .own
            .as_ref()
            .and_then(|own| own.json().get("buildId"))
            .and_then(IvyJson::as_js_string)
            .cloned(),
    };
    let preparation = PreparationRuntime {
        node,
        compare: artifact::native_file_compare()?,
        imports: None,
        http: PortableHttp::new(Default::default())?,
    };
    let bindings = crate::Bindings::native_for_startup(
        &startup,
        preparation,
        distribution,
        job,
        options,
        Some(Arc::new(NativePackageClient::new()?)),
        started,
    )?;
    crate::serve(startup, bindings, stop).await
}

#[derive(Debug)]
pub struct Options {
    pub host_config: PathBuf,
    pub manager_executable: PathBuf,
    pub manager_config: PathBuf,
    pub instance_config: Option<PathBuf>,
    pub job_launcher: Option<PathBuf>,
}
fn invalid(message: &str) -> IvyError {
    IvyError::new("invalid_arguments", message)
}
fn path(value: &IvyJson) -> IvyResult<PathBuf> {
    value
        .as_js_string()
        .map(node_path::to_native)
        .ok_or_else(IvyError::internal)
}
impl Options {
    pub fn parse(args: &[OsString], environment: Option<OsString>) -> IvyResult<Self> {
        let mut values = BTreeMap::new();
        let mut args = args.iter();
        while let Some(key) = args.next() {
            if !key.to_str().is_some_and(|key| {
                [
                    "--config",
                    "--manager-executable",
                    "--manager-config",
                    "--instance-config",
                    "--job-launcher",
                ]
                .contains(&key)
            }) {
                return Err(invalid(USAGE));
            }
            let value = args
                .next()
                .filter(|value| !value.to_string_lossy().starts_with("--"))
                .ok_or_else(|| invalid(USAGE))?;
            let value = PathBuf::from(value);
            if !value.is_absolute() || values.insert(key.clone(), value).is_some() {
                return Err(invalid(
                    "Each option needs one absolute path and may appear only once.",
                ));
            }
        }
        if !values.contains_key(&OsString::from("--config")) {
            if let Some(value) = environment {
                let value = PathBuf::from(value);
                if !value.is_absolute() {
                    return Err(invalid(USAGE));
                }
                values.insert("--config".into(), value);
            }
        }
        let mut required = |key| {
            values
                .remove(&OsString::from(key))
                .ok_or_else(|| invalid(USAGE))
        };
        let host_config = required("--config")?;
        let manager_executable = required("--manager-executable")?;
        let manager_config = required("--manager-config")?;
        Ok(Self {
            host_config,
            manager_executable,
            manager_config,
            instance_config: values.remove(&OsString::from("--instance-config")),
            job_launcher: values.remove(&OsString::from("--job-launcher")),
        })
    }
}

fn matching_instance(host: &IvyJson, instance: &InstanceConfig, component: &str) -> IvyResult<()> {
    let value = instance.json();
    let matched = value.get("componentId") == Some(&component.into())
        && value.get("hostId") == host.get("hostId")
        && host
            .required("instances")?
            .as_array()
            .is_some_and(|instances| {
                instances.iter().any(|entry| {
                    ["instanceId", "serviceNodeId", "componentId"]
                        .iter()
                        .all(|key| entry.get(key) == value.get(key))
                        && entry.get("engine") == Some(&"process".into())
                        && entry.get("enabled") == Some(&true.into())
                })
            });
    if !matched {
        return Err(invalid(
            "Native service configuration must match an enabled process instance on this host.",
        ));
    }
    Ok(())
}

pub async fn run(options: Options, stop: CancellationToken) -> IvyResult<()> {
    // Validate both configurations before acquiring ownership or launching a child.
    let host = host_config::host_config(options.host_config.clone()).await?;
    if runtime_maintenance::runtime_reset_active(&host).await? {
        return Err(IvyError::new(
            "runtime_reset_pending",
            "Host runtime reset must finish before startup.",
        ));
    }
    let manager = config::native_instance_config(options.manager_config.clone()).await?;
    matching_instance(&host, &manager, "service-manager")?;
    let configured_host = path(
        manager
            .json()
            .required("settings")?
            .required("hostConfigPath")?,
    )?;
    if tokio::fs::canonicalize(configured_host)
        .await
        .map_err(|_| IvyError::internal())?
        != tokio::fs::canonicalize(&options.host_config)
            .await
            .map_err(|_| IvyError::internal())?
    {
        return Err(invalid(
            "ServiceManager must use the same host configuration.",
        ));
    }
    let own = match &options.instance_config {
        Some(file) => {
            let instance = config::native_instance_config(file.clone()).await?;
            matching_instance(&host, &instance, "host-executor")?;
            Some(instance)
        }
        None => None,
    };
    let build_id = own
        .as_ref()
        .and_then(|instance| instance.json().get("buildId"))
        .cloned()
        .unwrap_or(IvyJson::Null);
    let health = own.map(|instance| {
        let launch_id = std::env::var("IVY_LAUNCH_ID")
            .ok()
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        HealthFile::with_launch_id(instance, Some(launch_id.into()))
    });
    let boot_id = health
        .as_ref()
        .map(|health| health.boot_id().to_owned())
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let mut status = IvyJson::object([
        ("schemaVersion", IvyJson::Number(1.0)),
        ("hostId", host.required("hostId")?.clone()),
        ("pid", IvyJson::Number(std::process::id() as f64)),
        ("bootId", boot_id.into()),
        ("buildId", build_id),
        ("activeDeploymentId", IvyJson::Null),
    ]);
    let lock =
        ExecutorLock::acquire(path(host.required("runtimeRoot")?)?.join("executor"), false).await?;
    let result = async {
        write_status(&host, &mut status, "reconciling", None).await?;
        supervise(options, &host, manager, health, &mut status, stop).await
    }
    .await;
    let finalized = write_status(
        &host,
        &mut status,
        if result.is_ok() { "stopping" } else { "failed" },
        result.as_ref().err().map(|error| error.code.as_str()),
    )
    .await;
    let closed = lock.close().await;
    result.and(finalized).and(closed)
}

async fn write_status(
    host: &IvyJson,
    status: &mut IvyJson,
    state: &str,
    code: Option<&str>,
) -> IvyResult<()> {
    status.insert("state", state.into());
    status.insert("code", code.map(IvyJson::from).unwrap_or(IvyJson::Null));
    status.insert(
        "observedAt",
        chrono::Utc::now()
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
            .into(),
    );
    validate_host("ExecutorStatus", status)?;
    config::atomic_json(
        path(host.required("runtimeRoot")?)?.join("executor.json"),
        status.clone(),
    )
    .await
}

async fn supervise(
    options: Options,
    host: &IvyJson,
    manager: InstanceConfig,
    health: Option<HealthFile>,
    status: &mut IvyJson,
    stop: CancellationToken,
) -> IvyResult<()> {
    if stop.is_cancelled() {
        return Ok(());
    }
    let launch_id: JsString = uuid::Uuid::new_v4().to_string().into();
    let mut environment = BTreeMap::from([
        (
            OsString::from("IVY_INSTANCE_CONFIG"),
            options.manager_config.clone().into_os_string(),
        ),
        (
            OsString::from("IVY_HOST_CONFIG"),
            options.host_config.into_os_string(),
        ),
        (
            OsString::from("IVY_LAUNCH_ID"),
            node_path::to_native(&launch_id).into_os_string(),
        ),
    ]);
    // Node is an optional intentional workload capability, never a native startup prerequisite.
    if let Some(node) = host
        .get("executables")
        .and_then(|values| values.get("node"))
    {
        environment.insert("IVY_NODE_EXECUTABLE".into(), path(node)?.into_os_string());
    } else if let Some(node) = std::env::var_os("IVY_NODE_EXECUTABLE") {
        environment.insert("IVY_NODE_EXECUTABLE".into(), node);
    }
    let redact = manager
        .json()
        .get("credential")
        .and_then(IvyJson::as_js_string)
        .cloned()
        .into_iter()
        .collect();
    let command = Command {
        executable: "manager".into(),
        args: vec!["--config".into(), options.manager_config.into_os_string()],
        cwd: None,
        timeout_ms: 0,
    };
    let executables = BTreeMap::from([("manager".into(), options.manager_executable)]);
    let allow_windows_breakaway = host
        .required("instances")?
        .as_array()
        .is_some_and(|instances| {
            instances
                .iter()
                .find(|instance| instance.get("instanceId") == manager.json().get("instanceId"))
                .and_then(|instance| instance.get("process"))
                .and_then(|process| process.get("allowWindowsBreakaway"))
                == Some(&true.into())
        });
    let job_launcher = options.job_launcher.or_else(|| {
        std::env::current_exe()
            .ok()
            .and_then(|path| path.parent().map(|parent| parent.join("ivy-job.exe")))
            .filter(|path| path.is_file())
    });
    let child = process::spawn(
        &command,
        manager.artifact_root(),
        &executables,
        LaunchContext {
            environment,
            job_launcher,
            redact,
            allow_windows_breakaway,
            on_output: Some(Arc::new(|stream, text| match stream {
                OutputStream::Stdout => print!("{text}"),
                OutputStream::Stderr => eprint!("{text}"),
            })),
            ..Default::default()
        },
    )
    .await?;
    let mut pulse = tokio::time::interval(Duration::from_secs(1));
    pulse.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let result = loop {
        tokio::select! {
            _ = stop.cancelled() => break Ok(()),
            result = child.completion.wait() => break if result.exit_code == Some(0) && result.error_code.is_none() { Ok(()) } else {
                let outcome = if result.error_code.as_deref() == Some("outcome_unknown") { Outcome::Unknown } else { Outcome::Completed };
                Err(IvyError::new(result.error_code.clone().unwrap_or_else(|| "service_manager_exited".into()), "ServiceManager exited before HostExecutor stopped.").with_outcome(outcome))
            },
            _ = pulse.tick() => {
                // The accepted native launch is independent of Manager's aggregate
                // readiness; Manager consumes this status to establish its health.
                if let Err(error) = write_status(host, status, "ready", None).await {
                    break Err(error);
                }
                if let Some(health) = &health {
                    let ready = manager_ready(&manager, &launch_id).await;
                    let _ = health.write(ready, "".into(), None).await;
                    if matches!(health.control().await, Ok(Some(_))) { break Ok(()); }
                }
            }
        }
    };
    let stopping = write_status(host, status, "stopping", None).await;
    let closed = stop_child(&child, Duration::from_secs(30), async {
        let value = config::json_file(
            manager.data_root().join("health.json"),
            config::CONFIGURATION_BYTES,
        )
        .await?;
        validate_host("Health", &value)?;
        let id = manager
            .json()
            .required("instanceId")?
            .as_js_string()
            .ok_or_else(IvyError::internal)?;
        let requested_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        if let Some(control) = shutdown_control(&value, id, &launch_id, &requested_at) {
            config::atomic_json(manager.data_root().join("control.json"), control).await?;
        }
        Ok(())
    })
    .await
    .map(|_| ());
    if let Some(health) = &health {
        let _ = health.write(false, "Stopped".into(), None).await;
    }
    result.and(stopping).and(closed)
}

async fn manager_ready(manager: &InstanceConfig, launch_id: &JsString) -> bool {
    let Ok(value) = ivy_host_runtime::health::check_health(manager, None, false).await else {
        return false;
    };
    value.get("launchId").and_then(IvyJson::as_js_string) == Some(launch_id)
}
