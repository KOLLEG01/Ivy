//! Agent launch admission and ownership; native catalogs retain their own capabilities.
use crate::{
    claude_launch::{assert_claude_subscription_config, checked_claude_adapter},
    environment_manager::ManagedEnvironment,
    native_connection::{
        AgentNativeConnection, NativeConnectionSettings, NativeStopEvidence, connect_agent_native,
    },
    native_handshake::NativeMode,
};
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString, agent_schema::validate_agent,
    checked_native_contract::native_catalog_path,
};
use ivy_host_runtime::{
    codex_daemon,
    codex_home::{
        HomeContext, expected_server_home_json, instance_owned_codex_home_json,
        resolve_codex_home_json, uses_external_app_server_json,
    },
    codex_rpc::Events,
    codex_windows_shell, config,
    node_path::{self, Context},
    paths, process,
    process_fence::require_clear_process_stop_fence,
};
use ivy_sdk::{rpc::RpcClient, service::ServiceFuture};
use std::{collections::BTreeMap, ffi::OsString, future::Future, path::PathBuf, sync::Arc};
use tokio::sync::oneshot;

pub type NativeFileHash = dyn Fn(PathBuf) -> ServiceFuture<String> + Send + Sync;
pub type BeforeNativeLaunch = dyn Fn(JsString) -> ServiceFuture<()> + Send + Sync;
pub type BeginNativeEpoch = dyn Fn(JsString) -> IvyResult<Events> + Send + Sync;
/// A snapshot of the executing account's inputs, never a substituted instance
/// home. The caller obtains these through the host; launch changes no parent env.
pub struct NativeHomeInputs {
    pub paths: Context,
    pub environment: BTreeMap<JsString, JsString>,
    pub user_home: Option<JsString>,
    pub cwd: JsString,
    pub account_home: JsString,
}
impl NativeHomeInputs {
    pub fn context(&self) -> HomeContext<'_> {
        HomeContext {
            paths: &self.paths,
            environment: &self.environment,
            user_home: self.user_home.as_ref(),
            cwd: &self.cwd,
            account_home: &self.account_home,
        }
    }
}
pub struct NativeProcessOptions {
    pub artifact_root: JsString,
    pub data_root: JsString,
    pub settings: IvyJson,
    pub client_version: JsString,
    pub hive_credential: Option<JsString>,
    pub home_inputs: NativeHomeInputs,
    pub before_launch: Option<Arc<BeforeNativeLaunch>>,
    pub begin_epoch: Arc<BeginNativeEpoch>,
    pub file_hash: Arc<NativeFileHash>,
    pub record_stop: Arc<NativeStopEvidence>,
}
pub fn managed_environment_before_launch(
    environment: ManagedEnvironment,
    bootstrap: Arc<dyn RpcClient>,
) -> Arc<BeforeNativeLaunch> {
    Arc::new(move |home| {
        let run = environment.before_launch(home, bootstrap.clone());
        Box::pin(async move { run?.wait().await })
    })
}
pub(crate) fn require(value: bool, code: &str, message: &str) -> IvyResult<()> {
    if value {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
pub(crate) async fn native_realpath(path: PathBuf) -> IvyResult<PathBuf> {
    tokio::task::spawn_blocking(move || paths::realpath(&path))
        .await
        .map_err(|_| IvyError::internal())?
        .map_err(|_| IvyError::internal())
}
fn text<'a>(value: &'a IvyJson, key: &str) -> IvyResult<&'a JsString> {
    value
        .required(key)?
        .as_js_string()
        .ok_or_else(IvyError::internal)
}
async fn directory(path: &std::path::Path) -> IvyResult<()> {
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || {
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(path).map_err(|_| IvyError::internal())
    })
    .await
    .map_err(|_| IvyError::internal())?
}
fn exact_version(version: &JsString) -> bool {
    let units = &version.0;
    let mut end = 2;
    if !units.starts_with(&[48, 46]) {
        return false;
    }
    for part in 0..2 {
        let start = end;
        while units.get(end).is_some_and(|c| (48..=57).contains(c)) {
            end += 1;
        }
        if start == end {
            return false;
        }
        if part == 0 {
            if units.get(end) != Some(&46) {
                return false;
            }
            end += 1;
        }
    }
    end == units.len()
}
fn whitespace(c: u16) -> bool {
    matches!(c, 9..=13 | 32 | 160 | 5760 | 8192..=8202 | 8232 | 8233 | 8239 | 8287 | 12288 | 65279)
}
fn cli_version(stdout: &str, version: &JsString) -> bool {
    let units: Vec<_> = stdout.encode_utf16().collect();
    let start = units
        .iter()
        .position(|c| !whitespace(*c))
        .unwrap_or(units.len());
    let end = units
        .iter()
        .rposition(|c| !whitespace(*c))
        .map_or(start, |i| i + 1);
    units[start..end]
        .iter()
        .copied()
        .eq("codex-cli ".encode_utf16().chain(version.0.iter().copied()))
}

/// Once invoked, preflight/bootstrap belongs to this owner. If its observer is
/// dropped, a subsequently acquired connection is dropped and retires its child.
pub fn start_native_process(
    options: NativeProcessOptions,
) -> impl Future<Output = IvyResult<AgentNativeConnection>> + Send {
    let (send, receive) = oneshot::channel();
    tokio::spawn(async move {
        let _ = send.send(start(options).await);
    });
    async move { receive.await.map_err(|_| IvyError::internal())? }
}
async fn start(options: NativeProcessOptions) -> IvyResult<AgentNativeConnection> {
    let settings = &options.settings;
    validate_agent("Settings", settings)?;
    let context = options.home_inputs.context();
    let paths = context.paths;
    let home = resolve_codex_home_json(settings, &context)?;
    let owned_home = instance_owned_codex_home_json(settings, &options.data_root, &context)?;
    let server = settings.get("appServer");
    let claude = server.filter(|server| server.get("mode") == Some(&"claude-adapter".into()));
    if claude.is_some() {
        require(
            owned_home.is_some()
                && settings.get("patcherEnabled") != Some(&false.into())
                && settings.get("skillsRoot").is_none(),
            "invalid_arguments",
            "Claude adapter requires an instance-owned home, skills and managed environment.",
        )?;
    }
    let external = uses_external_app_server_json(settings)?;
    let explicit_external =
        server.filter(|server| server.get("mode") == Some(&"external-proxy".into()));
    require(
        node_path::is_absolute(paths.style, &options.data_root)
            && node_path::is_absolute(paths.style, &options.artifact_root)
            && node_path::is_absolute(paths.style, text(settings, "nativeExecutable")?)
            && !paths.inside(&options.artifact_root, &home.path)
            && !paths.inside(&home.path, &options.artifact_root)
            && home.path != paths.resolve(std::slice::from_ref(&options.data_root)),
        "invalid_arguments",
        "Codex home must not overlap executable artifacts or replace the instance data root.",
    )?;
    let socket = explicit_external
        .and_then(|server| server.get("socketPath"))
        .map(|socket| {
            socket
                .as_js_string()
                .cloned()
                .ok_or_else(IvyError::internal)
        })
        .transpose()?;
    if external {
        let expected = expected_server_home_json(settings, &context)?;
        require(
            socket
                .as_ref()
                .is_none_or(|socket| node_path::is_absolute(paths.style, socket))
                && expected
                    .as_js_string()
                    .is_some_and(|home| node_path::is_absolute(paths.style, home)),
            "invalid_arguments",
            "An explicit external socket and expected server home must be absolute.",
        )?;
    }
    require_clear_process_stop_fence(node_path::filesystem_path(&options.data_root)).await?;
    let version = text(settings, "nativeVersion")?;
    require(
        exact_version(version),
        "native_catalog_unavailable",
        "Native version must identify an exact generated catalog.",
    )?;
    let dist = node_path::join(paths.style, &[options.artifact_root.clone(), "dist".into()]);
    let catalog_path = native_catalog_path(&dist, version, None, None)?;
    let catalog =
        config::json_file(node_path::filesystem_path(&catalog_path), 2 * 1024 * 1024).await?;
    validate_agent("Catalog", &catalog)?;
    require(
        catalog.get("version") == Some(&IvyJson::String(version.clone()))
            && catalog.get("nativeExecutableHash") == settings.get("nativeExecutableHash"),
        "native_build_mismatch",
        "Native settings do not match the generated catalog executable identity.",
    )?;
    let executable = native_realpath(node_path::filesystem_path(text(
        settings,
        "nativeExecutable",
    )?))
    .await?;
    let executable_name = node_path::from_native(&executable).to_english_lowercase();
    require(
        tokio::fs::metadata(&executable)
            .await
            .map_err(|_| IvyError::internal())?
            .is_file()
            && ![".cmd", ".bat", ".ps1"]
                .iter()
                .any(|suffix| executable_name.0.ends_with(&JsString::from(*suffix).0)),
        "native_build_mismatch",
        "Native owner requires the direct executable, not a moving shell wrapper.",
    )?;
    let expected_hash = claude
        .and_then(|claude| claude.get("nodeExecutableHash"))
        .unwrap_or(settings.required("nativeExecutableHash")?);
    require(
        expected_hash == &IvyJson::from((options.file_hash)(executable.clone()).await?),
        "native_build_mismatch",
        "Configured native executable bytes differ from their generated schema build.",
    )?;
    let adapter_script = if let Some(claude) = claude {
        Some(
            checked_claude_adapter(
                text(claude, "adapterRoot")?,
                text(settings, "nativeExecutableHash")?,
                paths,
                &options.file_hash,
            )
            .await?,
        )
    } else {
        None
    };
    let claude_executable = if let Some(claude) = claude {
        let executable = native_realpath(node_path::filesystem_path(text(
            claude,
            "claudeExecutable",
        )?))
        .await?;
        require(
            node_path::is_absolute(paths.style, text(claude, "claudeExecutable")?)
                && tokio::fs::metadata(&executable)
                    .await
                    .map_err(|_| IvyError::internal())?
                    .is_file()
                && claude.get("claudeExecutableHash")
                    == Some(&IvyJson::from(
                        (options.file_hash)(executable.clone()).await?,
                    )),
            "native_build_mismatch",
            "Claude CLI differs from the configured executable identity.",
        )?;
        Some(executable)
    } else {
        None
    };
    directory(&node_path::filesystem_path(&options.data_root)).await?;
    let data_root = native_realpath(node_path::filesystem_path(&options.data_root)).await?;
    let mut ancestor = paths.resolve(std::slice::from_ref(&home.path));
    loop {
        match tokio::fs::metadata(node_path::filesystem_path(&ancestor)).await {
            Ok(_) => break,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let parent = node_path::dirname(paths.style, &ancestor);
                require(
                    parent != ancestor,
                    "invalid_arguments",
                    "Native home has no owned existing ancestor.",
                )?;
                ancestor = parent;
            }
            Err(_) => return Err(IvyError::internal()),
        }
    }
    if owned_home.is_some() {
        let actual_ancestor = native_realpath(node_path::filesystem_path(&ancestor)).await?;
        require(
            paths.inside(
                &node_path::from_native(&data_root),
                &node_path::from_native(&actual_ancestor),
            ),
            "target_conflict",
            "Owned native home traverses an existing link outside this instance data.",
        )?;
    }
    directory(&node_path::filesystem_path(&home.path)).await?;
    let native_home = native_realpath(node_path::filesystem_path(&home.path)).await?;
    if owned_home.is_some() {
        require(
            paths.inside(
                &node_path::from_native(&data_root),
                &node_path::from_native(&native_home),
            ),
            "target_conflict",
            "Owned native home resolves outside this instance data.",
        )?;
    }
    let claude_config = claude.map(|_| data_root.join("claude-config"));
    let adapter_home = claude.map(|_| data_root.join("claude-adapter"));
    if let (Some(config), Some(adapter_home)) = (&claude_config, &adapter_home) {
        directory(config).await?;
        directory(adapter_home).await?;
        let real_config = native_realpath(config.clone()).await?;
        let real_adapter = native_realpath(adapter_home.clone()).await?;
        require(
            paths.inside(
                &node_path::from_native(&data_root),
                &node_path::from_native(&real_config),
            ) && paths.inside(
                &node_path::from_native(&data_root),
                &node_path::from_native(&real_adapter),
            ),
            "target_conflict",
            "Claude runtime data must remain inside this instance.",
        )?;
        assert_claude_subscription_config(config, context.environment).await?;
    }
    if let Some(before_launch) = &options.before_launch {
        before_launch(node_path::from_native(&native_home)).await?;
    }
    let job_launcher = node_path::filesystem_path(&node_path::join(
        paths.style,
        &[
            options.artifact_root.clone(),
            "dist".into(),
            "native".into(),
            "ivy-job.exe".into(),
        ],
    ));
    require(
        cfg!(windows) || settings.get("windowsShell").is_none(),
        "native_shell_mismatch",
        "Windows shell configuration belongs only to the Windows native owner.",
    )?;
    let windows_shell = if cfg!(windows) && (!external || socket.is_none()) {
        Some(
            codex_windows_shell::verify_windows_shell(
                settings.get("windowsShell"),
                &native_home,
                &job_launcher,
                paths,
            )
            .await?,
        )
    } else {
        None
    };
    let mut environment = BTreeMap::from([(
        OsString::from("CODEX_HOME"),
        native_home.clone().into_os_string(),
    )]);
    if let Some(shell) = &windows_shell {
        environment.extend(codex_windows_shell::windows_shell_environment(
            shell,
            context.environment,
        )?);
    }
    if !external && claude.is_none() {
        if let Some(credential) = options
            .hive_credential
            .as_ref()
            .filter(|credential| !credential.0.is_empty())
        {
            environment.insert(
                "IVY_HIVE_TOKEN".into(),
                node_path::to_native(credential).into_os_string(),
            );
        }
    }
    if let (Some(claude), Some(config), Some(adapter_home), Some(cli)) =
        (claude, &claude_config, &adapter_home, &claude_executable)
    {
        environment.extend(
            [
                ("CLAUDE_CONFIG_DIR", config.clone().into_os_string()),
                ("CLAUDE_CODEX_HOME", adapter_home.clone().into_os_string()),
                ("CLAUDE_CODEX_CLI", cli.clone().into_os_string()),
                ("CLAUDE_CODEX_RUNTIME_TYPE", "agent-sdk-sidecar".into()),
                ("CLAUDE_CODEX_PROVIDER", "claude-code".into()),
                ("CLAUDE_CODEX_AGENT_LOOP", "native-claude-code-sdk".into()),
                ("CLAUDE_CODEX_DISABLE_CODEX_PROXY", "1".into()),
                ("CLAUDE_CODEX_SUBSCRIPTION_ONLY", "1".into()),
                (
                    "CLAUDE_CODEX_DEFAULT_MODEL",
                    node_path::to_native(text(claude, "defaultModel")?).into_os_string(),
                ),
                (
                    "CLAUDE_CODEX_COMPAT_VERSION",
                    node_path::to_native(version).into_os_string(),
                ),
                (
                    "CLAUDE_CODEX_MCP_SERVERS",
                    data_root.join("claude-mcp.json").into_os_string(),
                ),
                (
                    "CLAUDE_CODEX_INSTRUCTIONS_FILE",
                    native_home.join("AGENTS.override.md").into_os_string(),
                ),
            ]
            .into_iter()
            .map(|(key, value)| (key.into(), value)),
        );
    }
    let executables = BTreeMap::from([("codex".into(), executable.clone())]);
    let run = |args| {
        let command = process::Command {
            executable: "codex".into(),
            args,
            cwd: None,
            timeout_ms: 15_000,
        };
        let environment = environment.clone();
        let job_launcher = job_launcher.clone();
        let home = native_home.clone();
        let executables = executables.clone();
        async move {
            process::run_command(
                &command,
                &home,
                &executables,
                process::LaunchContext {
                    environment,
                    job_launcher: Some(job_launcher),
                    ..Default::default()
                },
                None,
            )
            .await
        }
    };
    let version_probe = run(if let Some(adapter) = &adapter_script {
        vec![
            adapter.clone().into_os_string(),
            "app-server".into(),
            "--help".into(),
        ]
    } else {
        vec!["--version".into()]
    })
    .await?;
    require(
        if claude.is_some() {
            version_probe
                .stdout
                .contains("claude-codex-adapter app-server")
        } else {
            cli_version(&version_probe.stdout, version)
        },
        "native_build_mismatch",
        "Actual public native CLI version differs from its pinned schema catalog.",
    )?;
    if external {
        let help = run(vec!["app-server".into(), "proxy".into(), "--help".into()]).await?;
        let sock = "--sock";
        require(
            help.exit_code == Some(0)
                && help.stdout.match_indices(sock).any(|(start, _)| {
                    help.stdout
                        .as_bytes()
                        .get(start + sock.len())
                        .is_none_or(|c| !c.is_ascii_alphanumeric() && *c != b'_')
                }),
            "native_transport_unavailable",
            "Installed Codex does not expose the explicit app-server proxy socket transport.",
        )?;
    }
    let expected_home = if external {
        let expected = expected_server_home_json(settings, &context)?;
        native_realpath(node_path::filesystem_path(
            expected.as_js_string().ok_or_else(IvyError::internal)?,
        ))
        .await
        .map_err(|_| {
            IvyError::new(
                "native_home_mismatch",
                "The selected external server home is unavailable.",
            )
        })?
    } else {
        native_home.clone()
    };
    require(
        !external || socket.is_some() || expected_home == native_home,
        "native_home_mismatch",
        "Automatic daemon startup must use the resolved Codex home. A different server home requires an explicit endpoint.",
    )?;
    let daemon = if external && socket.is_none() {
        Some(
            codex_daemon::ensure_shared_daemon(codex_daemon::Settings {
                executable: executable.clone(),
                executable_hash: text(settings, "nativeExecutableHash")?.to_utf8()?,
                version: version.clone(),
                home: native_home.clone(),
                job_launcher: job_launcher.clone(),
                environment: environment.clone(),
            })
            .await?,
        )
    } else {
        None
    };
    let socket = socket.or_else(|| {
        daemon
            .as_ref()
            .and_then(|daemon| daemon.get("socketPath"))
            .and_then(IvyJson::as_js_string)
            .cloned()
    });
    let mode = if claude.is_some() {
        NativeMode::ClaudeAdapter
    } else if external {
        NativeMode::ExternalProxy
    } else {
        NativeMode::OwnedStdio
    };
    let epoch = JsString::from(uuid::Uuid::new_v4().to_string());
    let events = (options.begin_epoch)(epoch.clone())?;
    let args = if let Some(adapter) = adapter_script {
        vec![
            adapter.into_os_string(),
            "app-server".into(),
            "--listen".into(),
            "stdio://".into(),
        ]
    } else if external {
        vec![
            "app-server".into(),
            "proxy".into(),
            "--sock".into(),
            node_path::to_native(socket.as_ref().ok_or_else(IvyError::internal)?).into_os_string(),
        ]
    } else {
        vec!["app-server".into()]
    };
    let child = process::spawn(
        &process::Command {
            executable: "codex".into(),
            args,
            cwd: None,
            timeout_ms: 30_000,
        },
        &native_home,
        &executables,
        process::LaunchContext {
            environment,
            job_launcher: Some(job_launcher),
            raw_output: true,
            ..Default::default()
        },
    )
    .await;
    let child = match child {
        Ok(child) => child,
        Err(error) => {
            (events.on_close)("native_launch_failed".into()).await?;
            return Err(error);
        }
    };
    connect_agent_native(
        child,
        NativeConnectionSettings {
            epoch,
            catalog,
            expected_home,
            native_version: version.clone(),
            client_version: options.client_version,
            mode,
            home_source: home.source,
            owns_native_home: !external && owned_home.is_some(),
            executable,
            windows_shell,
            socket_path: socket,
            daemon,
            record_stop: options.record_stop,
        },
        events,
    )
    .await
}
