//! Executable identity checks used by native launch after catalog preflight.
use super::{
    codex_connection::{self, Identity, Mode, NativeConnection},
    codex_daemon::{self, file_hash},
    codex_home::{HomeContext, app_server, expected_server_home_json, resolve_codex_home_json},
    codex_rpc::Events,
    codex_windows_shell, config, node_path, paths, process,
    process_fence::require_clear_process_stop_fence,
};
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString, agent_schema::validate_agent,
    checked_native_contract::native_catalog_path, json::stringify,
};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    path::{Path, PathBuf},
};

/// The caller validates configured absolute paths and the stop fence first.
pub async fn verify_executable(configured: &Path, expected_hash: &JsString) -> IvyResult<PathBuf> {
    let configured = configured.to_path_buf();
    let executable = tokio::task::spawn_blocking(move || paths::realpath(&configured))
        .await
        .map_err(|_| IvyError::internal())?
        .map_err(|_| IvyError::internal())?;
    let metadata = tokio::fs::metadata(&executable)
        .await
        .map_err(|_| IvyError::internal())?;
    let bytes = executable.as_os_str().as_encoded_bytes();
    let suffix = &bytes[bytes.len().saturating_sub(4)..];
    // A dotfile such as .CMD also matches the original whole-path suffix regex.
    let script = [b".cmd", b".bat", b".ps1"]
        .iter()
        .any(|ending| suffix.eq_ignore_ascii_case(*ending));
    if !metadata.is_file()
        || script
        || JsString::from(file_hash(&executable).await?) != *expected_hash
    {
        return Err(IvyError::new(
            "native_build_mismatch",
            "Configured Codex executable differs from its exact catalog build.",
        ));
    }
    Ok(executable)
}

pub(crate) fn version_command() -> process::Command {
    process::Command {
        executable: "codex".into(),
        args: vec!["--version".into()],
        cwd: None,
        timeout_ms: 15_000,
    }
}

/// Admit the exact pinned CLI version after native Unicode whitespace trimming.
pub(crate) fn verify_version_result(
    result: &process::ProcessResult,
    expected_version: &JsString,
) -> IvyResult<()> {
    let expected = "codex-cli "
        .encode_utf16()
        .chain(expected_version.0.iter().copied());
    if result.exit_code != Some(0) || !result.stdout.trim().encode_utf16().eq(expected) {
        return Err(IvyError::new(
            "native_build_mismatch",
            "Codex CLI version differs from the selected catalog.",
        ));
    }
    Ok(())
}

/// Shell validation and the selected home/environment precede this bounded probe.
/// Process errors retain their original code/outcome; there is no fallback or retry.
pub async fn verify_cli_version(
    executable: &Path,
    home: &Path,
    expected_version: &JsString,
    environment: BTreeMap<OsString, OsString>,
    job_launcher: &Path,
) -> IvyResult<()> {
    let executables = BTreeMap::from([("codex".into(), executable.to_path_buf())]);
    let context = process::LaunchContext {
        environment,
        job_launcher: Some(job_launcher.to_path_buf()),
        ..Default::default()
    };
    let result =
        process::run_command(&version_command(), home, &executables, context, None).await?;
    verify_version_result(&result, expected_version)
}

/// Executing-account inputs come from HostContext::current; this owner never
/// invents an account home, changes process environment or invokes Node for it.
pub struct StartOptions<'a> {
    pub settings: &'a IvyJson,
    pub artifact_root: &'a JsString,
    pub data_root: &'a JsString,
    pub home_context: &'a HomeContext<'a>,
    pub client_name: JsString,
    pub client_version: JsString,
    pub hive_credential: Option<JsString>,
    pub events: Events,
}

fn require(test: bool, code: &str, message: &str) -> IvyResult<()> {
    if test {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn text<'a>(value: &'a IvyJson, key: &str) -> IvyResult<&'a JsString> {
    value
        .get(key)
        .and_then(IvyJson::as_js_string)
        .ok_or_else(|| IvyError::new("invalid_arguments", format!("{key} must be a string.")))
}
fn optional_text<'a>(value: &'a IvyJson, key: &str) -> IvyResult<Option<&'a JsString>> {
    value.get(key).map(|_| text(value, key)).transpose()
}
async fn realpath(path: PathBuf) -> IvyResult<PathBuf> {
    tokio::task::spawn_blocking(move || paths::realpath(&path))
        .await
        .map_err(|_| IvyError::internal())?
        .map_err(|_| IvyError::internal())
}
async fn directories(home: PathBuf, data_root: PathBuf) -> IvyResult<()> {
    tokio::task::spawn_blocking(move || {
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(home).map_err(|_| IvyError::internal())?;
        builder.create(data_root).map_err(|_| IvyError::internal())
    })
    .await
    .map_err(|_| IvyError::internal())?
}

/// Complete Codex-only public launch path shared by Voice and other service
/// clients. Owns the stdio child or proxy tunnel, never the external daemon.
pub async fn start_codex_process(options: StartOptions<'_>) -> IvyResult<NativeConnection> {
    let settings = options.settings;
    let server = app_server(settings)?;
    let mode = server.and_then(|value| value.get("mode"));
    require(
        mode != Some(&"claude-adapter".into()),
        "native_transport_unavailable",
        "Codex Voice requires the Codex app-server.",
    )?;
    let context = options.home_context;
    let home = resolve_codex_home_json(settings, context)?.path;
    let external = mode == Some(&"external-proxy".into())
        || (mode != Some(&"owned-stdio".into()) && cfg!(windows));
    let explicit = if mode == Some(&"external-proxy".into()) {
        server
    } else {
        None
    };
    let executable = text(settings, "nativeExecutable")?;
    require(
        node_path::is_absolute(context.paths.style, executable)
            && node_path::is_absolute(context.paths.style, options.data_root)
            && node_path::is_absolute(context.paths.style, options.artifact_root)
            && !context.paths.inside(options.artifact_root, &home)
            && !context.paths.inside(&home, options.artifact_root),
        "invalid_arguments",
        "Codex executable, runtime storage and home must select distinct absolute paths.",
    )?;
    let data_root = node_path::filesystem_path(options.data_root);
    require_clear_process_stop_fence(data_root.clone()).await?;
    let version = settings
        .get("nativeVersion")
        .and_then(IvyJson::as_js_string)
        .ok_or_else(|| {
            IvyError::new(
                "native_version_unsupported",
                "Supported native platform and version required.",
            )
        })?;
    let executable_hash = text(settings, "nativeExecutableHash")?;
    let socket = match explicit {
        Some(server) => optional_text(server, "socketPath")?.cloned(),
        None => None,
    };
    require(
        socket
            .as_ref()
            .is_none_or(|value| node_path::is_absolute(context.paths.style, value)),
        "invalid_arguments",
        "Codex proxy requires an absolute local control socket.",
    )?;
    let expected = if external {
        let home = expected_server_home_json(settings, context)?;
        let home = home.as_js_string().ok_or_else(|| {
            IvyError::new(
                "invalid_arguments",
                "Expected Codex home must be an absolute path string.",
            )
        })?;
        require(
            node_path::is_absolute(context.paths.style, home),
            "invalid_arguments",
            "Expected Codex home must be an absolute path string.",
        )?;
        Some(home.clone())
    } else {
        None
    };
    let dist = node_path::join(
        context.paths.style,
        &[options.artifact_root.clone(), "dist".into()],
    );
    let catalog_path = native_catalog_path(&dist, version, None, None)?;
    let catalog =
        config::json_file(node_path::filesystem_path(&catalog_path), 2 * 1024 * 1024).await?;
    validate_agent("Catalog", &catalog)?;
    require(
        catalog.get("provider") == Some(&"codex".into())
            && catalog.get("version") == Some(&IvyJson::String(version.clone()))
            && catalog.get("nativeExecutableHash")
                == Some(&IvyJson::String(executable_hash.clone()))
            && catalog
                .required("clientRequests")?
                .as_array()
                .unwrap()
                .iter()
                .any(|entry| {
                    entry.get("method") == Some(&"thread/realtime/start".into())
                        && entry
                            .get("inputSchema")
                            .and_then(|schema| stringify(schema, usize::MAX).ok())
                            .is_some_and(|schema| schema.contains("initialItems"))
                }),
        "native_build_mismatch",
        "Configured Codex must match its pinned catalog with realtime startup items.",
    )?;
    let executable = verify_executable(&node_path::to_native(executable), executable_hash).await?;
    directories(node_path::filesystem_path(&home), data_root.clone()).await?;
    let native_home = realpath(node_path::to_native(&home)).await?;
    let expected_home = if let Some(home) = &expected {
        realpath(node_path::to_native(home)).await?
    } else {
        native_home.clone()
    };
    require(
        !external || socket.is_some() || expected_home == native_home,
        "native_home_mismatch",
        "Automatic shared daemon startup must select the resolved Codex home.",
    )?;
    let job_launcher = node_path::lexical(
        &node_path::to_native(options.artifact_root)
            .join("dist")
            .join("native")
            .join("ivy-job.exe"),
    );
    let shell = if cfg!(windows) && (!external || socket.is_none()) {
        Some(
            codex_windows_shell::verify_windows_shell(
                settings.get("windowsShell"),
                &native_home,
                &job_launcher,
                context.paths,
            )
            .await?,
        )
    } else {
        None
    };
    let mut environment =
        BTreeMap::from([("CODEX_HOME".into(), native_home.clone().into_os_string())]);
    if let Some(shell) = shell {
        environment.extend(codex_windows_shell::windows_shell_environment(
            &shell,
            context.environment,
        )?);
    }
    if !external {
        if let Some(token) = options.hive_credential.filter(|value| !value.0.is_empty()) {
            environment.insert(
                "IVY_HIVE_TOKEN".into(),
                node_path::to_native(&token).into_os_string(),
            );
        }
    }
    verify_cli_version(
        &executable,
        &native_home,
        version,
        environment.clone(),
        &job_launcher,
    )
    .await?;
    let daemon = if external && socket.is_none() {
        Some(
            codex_daemon::ensure_shared_daemon(codex_daemon::Settings {
                executable: executable.clone(),
                executable_hash: executable_hash.to_utf8()?,
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
            .and_then(|value| value.get("socketPath"))
            .and_then(IvyJson::as_js_string)
            .cloned()
    });
    let args = if external {
        vec![
            "app-server".into(),
            "proxy".into(),
            "--sock".into(),
            node_path::to_native(socket.as_ref().ok_or_else(IvyError::internal)?).into_os_string(),
        ]
    } else {
        vec!["app-server".into()]
    };
    let epoch = uuid::Uuid::new_v4().to_string();
    let child = process::spawn(
        &process::Command {
            executable: "codex".into(),
            args,
            cwd: None,
            timeout_ms: 30_000,
        },
        &native_home,
        &BTreeMap::from([("codex".into(), executable)]),
        process::LaunchContext {
            environment,
            job_launcher: Some(job_launcher),
            raw_output: true,
            ..Default::default()
        },
    )
    .await?;
    codex_connection::connect(
        child,
        Identity {
            data_root,
            home: expected_home,
            epoch,
            native_version: version.clone(),
            client_name: options.client_name,
            client_version: options.client_version,
            mode: if external {
                Mode::ExternalProxy
            } else {
                Mode::OwnedStdio
            },
        },
        catalog,
        options.events,
    )
    .await
}
