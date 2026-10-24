//! Production Claude entry: one Rust protocol owner and one private vendor SDK child.
use super::{
    bridge::{Bridge, BridgeLaunch},
    driver::{self, Bindings},
    error::{AdapterError, Result},
    projects::ProjectStore,
    protocol::{Adapter, AdapterClock, Environment},
    rate_limits::RateLimitStore,
    sdk_options::SdkEnvironment,
    store::SessionStore,
    value::{decode, encode},
    workflow::WorkflowFiles,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use ivy_contracts::{IvyJson as J, JsString};
use ivy_host_runtime::node_path::{self, Style};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::mpsc,
    time::{Duration, timeout},
};

const MAXIMUM_BYTES: usize = 32 * 1024 * 1024;
fn required(name: &str) -> Result<PathBuf> {
    std::env::var_os(name)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .filter(|v| v.is_absolute())
        .ok_or_else(|| AdapterError::new(format!("Missing admitted path: {name}")))
}
fn env_json(environment: &BTreeMap<std::ffi::OsString, std::ffi::OsString>) -> J {
    J::Object(
        environment
            .iter()
            .map(|(k, v)| {
                (
                    node_path::from_native(&PathBuf::from(k)),
                    J::String(node_path::from_native(&PathBuf::from(v))),
                )
            })
            .collect(),
    )
}
fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

pub async fn run() -> Result<()> {
    let home = required("CLAUDE_CODEX_HOME")?;
    let codex_home = required("CODEX_HOME")?;
    let account_home = required("IVY_CLAUDE_ACCOUNT_HOME")?;
    let cwd = std::env::current_dir()?;
    let environment: BTreeMap<_, _> = std::env::vars_os().collect();
    let env = env_json(&environment);
    let clock: super::store::Clock = Arc::new(|| now().floor());
    let ids: super::value::IdSource = Arc::new(|| uuid::Uuid::new_v4().to_string().into());
    // Reopen the existing adapter stores; SessionStore owns schema migration.
    let projects = ProjectStore::open(
        &home,
        Style::native(),
        Arc::new(|a, b| crate::runtime::compare_names(a, b, false)),
        clock.clone(),
        ids.clone(),
    )?;
    let store = SessionStore::with_clock(&home, &home.join("state.sqlite"), clock.clone())?;
    store.recover_in_progress_turns(&"server restarted before completing turn".into())?;
    let mcp_servers = match std::env::var_os("CLAUDE_CODEX_MCP_SERVERS") {
        Some(path) => decode(&std::fs::read(path)?)?,
        None => J::Null,
    };
    let startup_statuses = mcp_servers
        .as_object()
        .map(|servers| {
            servers
                .iter()
                .map(|(name, _)| {
                    J::object([
                        ("name", J::String(name.clone())),
                        ("status", "ready".into()),
                        ("error", J::Null),
                    ])
                })
                .collect()
        })
        .unwrap_or_default();
    let adapter = Adapter::new(
        store,
        projects,
        RateLimitStore::open(&home),
        Environment {
            environment: env.clone(),
            discovered_router_models: vec![],
            cwd: node_path::from_native(&cwd),
            codex_home: node_path::from_native(&codex_home),
            platform_os: if cfg!(windows) { "windows" } else { "linux" }.into(),
            cpu: std::env::consts::ARCH.into(),
            compat_version: std::env::var("CLAUDE_CODEX_COMPAT_VERSION")
                .unwrap_or_else(|_| "0.142.3".into())
                .into(),
            version_suffix: "claude-codex".into(),
            default_model: "sonnet".into(),
            default_effort: "medium".into(),
            mcp_servers,
            startup_statuses,
            config_overrides: J::object([] as [(&str, J); 0]),
            config_path: home.join("config.json"),
            prepare_worktree: Arc::new(move |id, cwd| prepare_worktree(&home, id, cwd)),
            read_base64: Arc::new(|path| {
                Ok(STANDARD
                    .encode(std::fs::read(node_path::filesystem_path(path))?)
                    .into())
            }),
            subagent_completed: match std::env::var("CLAUDE_CODEX_SUBAGENT_COMPLETED")
                .ok()
                .as_deref()
            {
                Some("1") => Some(true),
                Some("0") => Some(false),
                _ => None,
            },
            subagent_watchdog_ms: std::env::var("CLAUDE_CODEX_SUBAGENT_TIMEOUT_MS")
                .ok()
                .and_then(|v| v.trim().parse::<u64>().ok())
                .filter(|v| *v >= 100),
        },
        AdapterClock {
            seconds: clock,
            millis: Arc::new(|| (now() * 1000.0).floor()),
        },
        ids,
    );
    let mut sdk_environment = environment;
    for name in [
        "CODEX_HOME",
        "CLAUDE_CODEX_HOME",
        "CLAUDE_CONFIG_DIR",
        "CLAUDE_CODEX_CLI",
        "HOME",
        "USERPROFILE",
    ] {
        if let Some(path) = sdk_environment.get_mut(std::ffi::OsStr::new(name)) {
            *path = super::bridge::sdk_path(&PathBuf::from(&*path)).into_os_string();
        }
    }
    let (bridge, events) = Bridge::spawn(
        BridgeLaunch {
            node_executable: required("IVY_CLAUDE_SDK_NODE")?,
            entry: required("IVY_CLAUDE_SDK_ENTRY")?,
            cwd: cwd.clone(),
            environment: sdk_environment,
        },
        Arc::new(|_| {}),
    )?;
    let (output, mut frames) = mpsc::channel::<Vec<u8>>(128);
    let writer = tokio::spawn(async move {
        let mut stdout = tokio::io::stdout();
        while let Some(mut frame) = frames.recv().await {
            frame.push(b'\n');
            stdout.write_all(&frame).await?;
            stdout.flush().await?;
        }
        Ok::<_, std::io::Error>(())
    });
    let (handle, mut owner) = driver::spawn(
        adapter,
        bridge,
        events,
        Bindings {
            sdk: SdkEnvironment {
                home: node_path::from_native(&account_home),
                environment: env,
            },
            workflow_files: WorkflowFiles::host(),
            send: Arc::new(move |_, message| {
                let frame = encode(&message)?.into_bytes();
                if frame.len() > MAXIMUM_BYTES {
                    return Err(AdapterError::new("Native frame exceeds byte limit"));
                }
                output
                    .try_send(frame)
                    .map_err(|_| AdapterError::new("Native output is closed or backpressured"))
            }),
            git_diff: Arc::new(|cwd| Box::pin(git_diff(cwd))),
        },
    );
    let input = async {
        let mut stdin = tokio::io::stdin();
        let mut chunk = [0u8; 8192];
        let mut buffer = Vec::new();
        loop {
            let count = stdin.read(&mut chunk).await?;
            if count == 0 {
                return Ok::<_, AdapterError>(());
            }
            buffer.extend_from_slice(&chunk[..count]);
            let mut consumed = 0;
            while let Some(n) = buffer[consumed..].iter().position(|b| *b == b'\n') {
                let end = consumed + n;
                if end - consumed > MAXIMUM_BYTES {
                    return Err(AdapterError::new("Native frame exceeds byte limit"));
                }
                let message = decode(&buffer[consumed..end])?;
                consumed = end + 1;
                handle.receive("stdio".into(), message).await?;
            }
            buffer.drain(..consumed);
            if buffer.len() > MAXIMUM_BYTES {
                return Err(AdapterError::new("Native frame exceeds byte limit"));
            }
        }
    };
    tokio::pin!(input);
    let outcome = tokio::select! {
        result=&mut input => result,
        result=&mut owner => {
            writer.abort();
            return result.map_err(|_|AdapterError::new("Claude protocol owner failed"))?;
        },
    };
    handle.stop().await;
    let cleanup = owner
        .await
        .map_err(|_| AdapterError::new("Claude protocol owner failed"))?;
    let _ = timeout(Duration::from_secs(2), writer).await;
    outcome.and(cleanup)
}

fn prepare_worktree(home: &std::path::Path, id: &JsString, cwd: &JsString) -> Result<JsString> {
    if std::env::var("CLAUDE_CODEX_AUTO_WORKTREE").as_deref() != Ok("1") {
        return Ok(cwd.clone());
    }
    let root = std::env::var_os("CLAUDE_CODEX_WORKTREE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join("worktrees"));
    std::fs::create_dir_all(&root)?;
    let label: String = String::from_utf16_lossy(&id.0)
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .take(48)
        .collect();
    let path = root.join(&label);
    let mut command = std::process::Command::new("git");
    command
        .current_dir(node_path::filesystem_path(cwd))
        .args(["worktree", "add", "-b", &format!("claude-codex/{label}")])
        .arg(&path)
        .arg("HEAD");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    if path.exists() || command.output().is_ok_and(|v| v.status.success()) {
        Ok(node_path::from_native(&path))
    } else {
        Ok(cwd.clone())
    }
}
async fn git_output(cwd: &JsString, args: &[&str]) -> Option<Vec<u8>> {
    use ivy_host_runtime::exec_file::{FileCommand, FileOptions, exec_file};
    let cwd = node_path::filesystem_path(cwd);
    let output = exec_file(
        FileCommand {
            executable: "git".into(),
            args: args.iter().map(Into::into).collect(),
            cwd: Some(cwd.clone()),
            timeout_ms: 10_000,
            maximum_buffer_bytes: 5 * 1024 * 1024,
        },
        cwd,
        FileOptions {
            job_launcher: if cfg!(windows) {
                Some(std::env::current_exe().ok()?.parent()?.join("ivy-job.exe"))
            } else {
                None
            },
            ..Default::default()
        },
    )
    .await
    .ok()?;
    output
        .failure
        .is_none()
        .then(|| String::from_utf16_lossy(&output.stdout.0).into_bytes())
}
async fn git_diff(cwd: JsString) -> Result<JsString> {
    let Some(tracked) = git_output(&cwd, &["diff", "--no-ext-diff", "--"]).await else {
        return Ok("".into());
    };
    let mut diffs = vec![String::from_utf8_lossy(&tracked).into_owned()];
    if let Some(paths) =
        git_output(&cwd, &["ls-files", "--others", "--exclude-standard", "-z"]).await
    {
        for name in paths.split(|b| *b == 0).filter(|p| !p.is_empty()).take(50) {
            let name = String::from_utf8_lossy(name);
            let Ok(bytes) =
                tokio::fs::read(node_path::filesystem_path(&cwd).join(name.as_ref())).await
            else {
                continue;
            };
            if bytes.contains(&0) {
                continue;
            }
            let text = String::from_utf8_lossy(&bytes);
            let lines: Vec<_> = if text.is_empty() {
                vec![]
            } else {
                text.strip_suffix('\n')
                    .unwrap_or(&text)
                    .split('\n')
                    .collect()
            };
            diffs.push(format!("diff --git a/{name} b/{name}\nnew file mode 100644\nindex 0000000..0000000\n--- /dev/null\n+++ b/{name}\n@@ -0,0 +1,{} @@\n{}\n",lines.len(),lines.iter().map(|line|format!("+{line}")).collect::<Vec<_>>().join("\n")));
        }
    }
    Ok(diffs
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
        .into())
}
