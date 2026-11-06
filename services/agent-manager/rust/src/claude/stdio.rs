//! Standalone diagnostic entry. Production uses runtime::start in the manager.
use super::{
    bridge::BridgeLaunch,
    error::{AdapterError, Result},
    runtime::{self, Launch},
    value::{decode, encode},
};
use std::{collections::BTreeMap, path::PathBuf, sync::Arc};
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
pub async fn run() -> Result<()> {
    let launch = Launch {
        project_compare: super::projects::project_comparator()
            .map_err(|error| AdapterError::new(error.message_js()))?,
        home: required("CLAUDE_CODEX_HOME")?,
        codex_home: required("CODEX_HOME")?,
        account_home: required("IVY_CLAUDE_ACCOUNT_HOME")?,
        compat_version: std::env::var("CLAUDE_CODEX_COMPAT_VERSION")
            .unwrap_or_else(|_| "0.142.3".into())
            .into(),
        default_model: std::env::var("CLAUDE_CODEX_DEFAULT_MODEL")
            .unwrap_or_else(|_| "sonnet".into())
            .into(),
        sdk: BridgeLaunch {
            node_executable: required("IVY_CLAUDE_SDK_NODE")?,
            entry: required("IVY_CLAUDE_SDK_ENTRY")?,
            cwd: std::env::current_dir()?,
            environment: std::env::vars_os().collect::<BTreeMap<_, _>>(),
            job_launcher: Some(
                std::env::current_exe()?
                    .parent()
                    .ok_or_else(|| AdapterError::new("Executable directory is unavailable"))?
                    .join(if cfg!(windows) {
                        "ivy-job.exe"
                    } else {
                        "ivy-job"
                    }),
            ),
        },
        mcp_servers_path: std::env::var_os("CLAUDE_CODEX_MCP_SERVERS").map(PathBuf::from),
        auto_worktree: std::env::var("CLAUDE_CODEX_AUTO_WORKTREE").as_deref() == Ok("1"),
        worktree_root: std::env::var_os("CLAUDE_CODEX_WORKTREE_ROOT").map(PathBuf::from),
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
    };
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
    let mut runtime = runtime::start(
        launch,
        Arc::new(move |_, message| {
            let frame = encode(&message)?.into_bytes();
            if frame.len() > MAXIMUM_BYTES {
                return Err(AdapterError::new("Native frame exceeds byte limit"));
            }
            output
                .try_send(frame)
                .map_err(|_| AdapterError::new("Native output is closed or backpressured"))
        }),
    )
    .await?;
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
                runtime.handle.receive("stdio".into(), message).await?;
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
        result=&mut runtime.owner => {
            writer.abort();
            return result.map_err(|_|AdapterError::new("Claude protocol owner failed"))?;
        },
    };
    runtime.handle.stop().await;
    let cleanup = runtime
        .owner
        .await
        .map_err(|_| AdapterError::new("Claude protocol owner failed"))?;
    let _ = timeout(Duration::from_secs(2), writer).await;
    outcome.and(cleanup)
}
