use ivy_contracts::{
    IvyError, IvyJson as J, IvyResult, JsString, checked_native_contract::native_catalog_path,
};
use ivy_host_runtime::{
    codex_connection::{self, Identity, Mode},
    codex_process,
    codex_rpc::Events,
    codex_windows_shell,
    config::{self, InstanceConfig},
    executor_lock::ExecutorLock,
    health::HealthFile,
    logs::ServiceLog,
    native_path, native_plan,
    process::{self, Command, LaunchContext},
};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

fn text(value: &J, key: &str) -> IvyResult<JsString> {
    value
        .required(key)?
        .as_js_string()
        .cloned()
        .ok_or_else(IvyError::internal)
}
fn path(value: &J, key: &str) -> IvyResult<PathBuf> {
    let path = native_path::to_native(&text(value, key)?);
    if !path.is_absolute() {
        return Err(IvyError::new(
            "invalid_arguments",
            "Server paths must be absolute.",
        ));
    }
    Ok(path)
}
pub async fn run() -> IvyResult<()> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let selected = config::configuration_path(&args, std::env::var_os("IVY_INSTANCE_CONFIG"))?;
    let config = config::native_instance_config(selected).await?;
    let health = HealthFile::new(config.clone());
    health
        .write(false, "Verifying shared server ownership.".into(), None)
        .await?;
    let result = Box::pin(serve(&config, &health)).await;
    let _ = health
        .write(false, "Shared server stopped.".into(), None)
        .await;
    result
}
async fn serve(config: &InstanceConfig, health: &HealthFile) -> IvyResult<()> {
    if config.json().get("componentId") != Some(&"codex-server".into()) {
        return Err(IvyError::internal());
    }
    let settings = config.json().required("settings")?;
    let args = foreground_arguments(settings)?;
    let home = ivy_host_runtime::paths::realpath(&path(settings, "codexHome")?)
        .map_err(|_| IvyError::internal())?;
    // One owner per actual home, independent of AgentManager's lifetime and data.
    let _lock = ExecutorLock::acquire(home.join(".ivy-shared-server"), false).await?;
    let log = Arc::new(Mutex::new(ServiceLog::open(
        &config.data_root().join("logs"),
    )?));
    let helper = native_plan::native_binary_path(config.artifact_root(), "ivy-job");
    let executable = codex_process::verify_executable(
        &path(settings, "nativeExecutable")?,
        &text(settings, "nativeExecutableHash")?,
    )
    .await?;
    let version = text(settings, "nativeVersion")?;
    let mut environment = BTreeMap::from([("CODEX_HOME".into(), home.clone().into_os_string())]);
    #[cfg(windows)]
    {
        let context = native_path::Context::current().map_err(|_| IvyError::internal())?;
        let shell = Box::pin(codex_windows_shell::verify_windows_shell(
            settings.get("windowsShell"),
            &home,
            &helper,
            &context,
        ))
        .await?;
        let inherited = std::env::vars_os()
            .map(|(k, v)| {
                (
                    native_path::from_native(&PathBuf::from(k)),
                    native_path::from_native(&PathBuf::from(v)),
                )
            })
            .collect();
        environment.extend(codex_windows_shell::windows_shell_environment(
            &shell, &inherited,
        )?);
        verify_start_definitions(
            settings,
            &home,
            config.artifact_root(),
            &helper,
            &shell,
            log.clone(),
        )
        .await?;
    }
    Box::pin(codex_process::verify_cli_version(
        &executable,
        &home,
        &version,
        environment.clone(),
        &helper,
    ))
    .await?;
    let executables = BTreeMap::from([("codex".into(), executable)]);
    let server = process::spawn(
        &Command {
            executable: "codex".into(),
            args,
            cwd: None,
            timeout_ms: 30_000,
        },
        &home,
        &executables,
        LaunchContext {
            environment: environment.clone(),
            job_launcher: Some(helper.clone()),
            on_output: Some(Arc::new(move |stream, text| {
                let _ = log
                    .lock()
                    .unwrap()
                    .append(stream, text, &chrono::Utc::now().to_rfc3339());
            })),
            ..Default::default()
        },
    )
    .await?;
    let result = async {
        let socket = path(settings, "socketPath")?;
        let catalog = native_catalog_path(
            &native_path::from_native(&config.artifact_root().join("dist")),
            &version,
            None,
            None,
        )?;
        let catalog = config::json_file(native_path::to_native(&catalog), 2 * 1024 * 1024).await?;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
        loop {
            if server.completion.current().is_some() {
                return Err(IvyError::new(
                    "native_exit",
                    "The foreground shared server stopped before readiness.",
                ));
            }
            if let Ok(proxy) = process::spawn(
                &Command {
                    executable: "codex".into(),
                    args: vec!["app-server".into(), "proxy".into(), "--sock".into(), socket.clone().into_os_string()],
                    cwd: None,
                    timeout_ms: 30_000,
                },
                &home,
                &executables,
                LaunchContext {
                    environment: environment.clone(),
                    job_launcher: Some(helper.clone()),
                    raw_output: true,
                    ..Default::default()
                },
            ).await {
                if let Ok(connection) = codex_connection::connect(
                    proxy,
                    Identity {
                        data_root: config.data_root().to_owned(),
                        home: home.clone(),
                        epoch: uuid::Uuid::new_v4().to_string(),
                        native_version: version.clone(),
                        client_name: "ivy-server-health".into(),
                        client_version: env!("CARGO_PKG_VERSION").into(),
                        mode: Mode::ExternalProxy,
                    },
                    catalog.clone(),
                    Events::default(),
                ).await {
                    connection.close().await?;
                    break;
                }
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(IvyError::new("service_not_ready", "Shared server did not pass its native handshake."));
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        let mut interval = tokio::time::interval(Duration::from_secs(2));
        loop {
            tokio::select! { biased;
                finished = server.completion.wait() => return if finished.exit_code == Some(0) { Ok(()) } else { Err(IvyError::new("native_exit", "Shared server exited.")) },
                _ = interval.tick() => {
                    if health.control().await?.is_some() { return Ok(()); }
                    health.write(true, "Shared server passed its native handshake.".into(), None).await?;
                }
            }
        }
    }.await;
    server.stop(Duration::from_secs(5)).await?;
    result
}
pub fn foreground_arguments(settings: &J) -> IvyResult<Vec<std::ffi::OsString>> {
    let values = settings
        .required("args")?
        .as_array()
        .ok_or_else(IvyError::internal)?;
    if values.len() != 3
        || values[0] != J::from("app-server")
        || values[1] != J::from("--listen")
        || !values[2].as_str()?.starts_with("unix://")
    {
        return Err(IvyError::new(
            "invalid_arguments",
            "Use a foreground local server command; detached daemon starts cannot be supervised here.",
        ));
    }
    values
        .iter()
        .map(|v| {
            v.as_js_string()
                .map(native_path::to_native)
                .map(PathBuf::into_os_string)
                .ok_or_else(IvyError::internal)
        })
        .collect()
}
#[cfg(windows)]
async fn verify_start_definitions(
    settings: &J,
    home: &std::path::Path,
    root: &std::path::Path,
    helper: &std::path::Path,
    shell: &J,
    log: Arc<Mutex<ServiceLog>>,
) -> IvyResult<()> {
    let input = J::object([
        ("home", J::String(native_path::from_native(home))),
        (
            "legacyTasks",
            settings
                .get("legacyStartTasks")
                .cloned()
                .unwrap_or(J::Array(vec![])),
        ),
    ]);
    let source = include_str!("../assets/inspect-starts.ps1");
    let encoded = ivy_contracts::json::stringify(&input, 65536)?;
    let result = process::run_command(
        &Command {
            executable: "shell".into(),
            args: vec![
                "-NoLogo".into(),
                "-NoProfile".into(),
                "-NonInteractive".into(),
                "-Command".into(),
                format!(
                    "{source}\nAssert-IvySharedServerStarts -InputJson '{}'",
                    encoded.replace('\'', "''")
                )
                .into(),
            ],
            cwd: None,
            timeout_ms: 30_000,
        },
        root,
        &BTreeMap::from([(
            "shell".into(),
            native_path::to_native(&text(shell, "executable")?),
        )]),
        LaunchContext {
            job_launcher: Some(helper.to_owned()),
            maximum_output_bytes: Some(64 * 1024),
            on_output: Some(Arc::new(move |stream, text| {
                let _ = log
                    .lock()
                    .unwrap()
                    .append(stream, text, &chrono::Utc::now().to_rfc3339());
            })),
            ..Default::default()
        },
        None,
    )
    .await?;
    if result.exit_code != Some(0) {
        return Err(IvyError::new(
            "target_conflict",
            "Enabled or unrecognized shared-server startup definitions require an explicit migration.",
        ));
    }
    Ok(())
}
