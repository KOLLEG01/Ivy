//! Claude composition inside the containing Rust service. All paths and the
//! vendor environment come from admission; this owner never reads parent env.
use super::{
    bridge::{Bridge, BridgeLaunch},
    driver::{self, Bindings},
    error::{AdapterError, Result},
    projects::ProjectStore,
    protocol::{Adapter, AdapterClock, Environment},
    rate_limits::RateLimitStore,
    sdk_options::SdkEnvironment,
    store::SessionStore,
    value::decode,
    workflow::WorkflowFiles,
};
use base64::{engine::general_purpose::STANDARD, write::EncoderWriter};
use ivy_contracts::{IvyJson as J, JsString};
use ivy_host_runtime::node_path::{self, Style};
use ivy_host_runtime::process::OwnedProcess;
use std::{
    collections::BTreeMap,
    io::{self, Read, Write},
    path::PathBuf,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::task::JoinHandle;

pub struct Launch {
    pub home: PathBuf,
    pub codex_home: PathBuf,
    pub account_home: PathBuf,
    pub compat_version: JsString,
    pub default_model: JsString,
    pub project_compare: super::value::Compare,
    pub sdk: BridgeLaunch,
    pub mcp_servers_path: Option<PathBuf>,
    pub auto_worktree: bool,
    pub worktree_root: Option<PathBuf>,
    pub subagent_completed: Option<bool>,
    pub subagent_watchdog_ms: Option<u64>,
}
pub struct Runtime {
    pub handle: driver::Handle,
    pub owner: JoinHandle<Result<()>>,
    pub process: Arc<OwnedProcess>,
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

pub async fn start(
    launch: Launch,
    send: Arc<dyn Fn(JsString, J) -> Result<()> + Send + Sync>,
) -> Result<Runtime> {
    let Launch {
        home,
        codex_home,
        account_home,
        compat_version,
        default_model,
        project_compare,
        mut sdk,
        mcp_servers_path,
        auto_worktree,
        worktree_root,
        subagent_completed,
        subagent_watchdog_ms,
    } = launch;
    let cwd = sdk.cwd.clone();
    let env = env_json(&sdk.environment);
    let clock: super::store::Clock = Arc::new(|| now().floor());
    let ids: super::value::IdSource = Arc::new(|| uuid::Uuid::new_v4().to_string().into());
    // Reopen the existing adapter stores; SessionStore owns schema migration.
    let projects = ProjectStore::open(
        &home,
        Style::native(),
        project_compare,
        clock.clone(),
        ids.clone(),
    )?;
    let store = SessionStore::with_clock(&home, &home.join("state.sqlite"), clock.clone())?;
    store.recover_in_progress_turns(&"server restarted before completing turn".into())?;
    let mcp_servers = match mcp_servers_path {
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
            compat_version,
            version_suffix: "claude-codex".into(),
            default_model: J::String(default_model),
            default_effort: "medium".into(),
            mcp_servers,
            startup_statuses,
            config_overrides: J::object([] as [(&str, J); 0]),
            config_path: home.join("config.json"),
            prepare_worktree: Arc::new(move |id, cwd| {
                prepare_worktree(&home, auto_worktree, worktree_root.as_deref(), id, cwd)
            }),
            read_base64: Arc::new(read_base64),
            subagent_completed,
            subagent_watchdog_ms,
        },
        AdapterClock {
            seconds: clock,
            millis: Arc::new(|| (now() * 1000.0).floor()),
        },
        ids,
    );
    for name in [
        "CODEX_HOME",
        "CLAUDE_CODEX_HOME",
        "CLAUDE_CONFIG_DIR",
        "CLAUDE_CODEX_CLI",
        "HOME",
        "USERPROFILE",
    ] {
        if let Some(path) = sdk.environment.get_mut(std::ffi::OsStr::new(name)) {
            *path = super::bridge::sdk_path(&PathBuf::from(&*path)).into_os_string();
        }
    }
    let job_launcher = sdk.job_launcher.clone();
    let (bridge, events) = Bridge::spawn(sdk, Arc::new(|_| {})).await?;
    let process = bridge.owned_process();
    let (handle, owner) = driver::spawn(
        adapter,
        bridge,
        events,
        Bindings {
            sdk: SdkEnvironment {
                home: node_path::from_native(&account_home),
                environment: env,
            },
            workflow_files: WorkflowFiles::host(),
            send,
            git_diff: Arc::new(move |cwd| Box::pin(git_diff(cwd, job_launcher.clone()))),
        },
    );
    Ok(Runtime {
        handle,
        owner,
        process,
    })
}

fn read_base64(path: &JsString, maximum_bytes: usize) -> Result<JsString> {
    let file = std::fs::File::open(node_path::filesystem_path(path))?;
    let length = file.metadata()?.len();
    encode_base64(file, length, maximum_bytes)
}

fn file_capacity() -> AdapterError {
    AdapterError::typed("native_capacity", "File exceeds native response byte limit")
}

fn encode_base64(mut input: impl Read, length: u64, maximum_bytes: usize) -> Result<JsString> {
    if length > maximum_bytes as u64 {
        return Err(file_capacity());
    }
    let mut output = Base64Output {
        units: Vec::with_capacity((length as usize).div_ceil(3) * 4),
        maximum: maximum_bytes.div_ceil(3) * 4,
    };
    let mut encoder = EncoderWriter::new(&mut output, &STANDARD);
    let count = io::copy(&mut input.by_ref().take(maximum_bytes as u64), &mut encoder)?;
    if count == maximum_bytes as u64 {
        // Metadata may be stale or zero for a virtual file. Read at most one
        // extra byte to distinguish an exact fit from a growing/unknown input.
        match input.read_exact(&mut [0]) {
            Ok(()) => return Err(file_capacity()),
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => {}
            Err(error) => return Err(error.into()),
        }
    }
    encoder.finish()?;
    drop(encoder);
    Ok(JsString(output.units))
}

/// Base64 is ASCII; retain only its final UTF-16 representation, with fixed
/// encoder/I/O buffers instead of a whole raw file and a whole UTF-8 string.
struct Base64Output {
    units: Vec<u16>,
    maximum: usize,
}
impl Write for Base64Output {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let needed = self.units.len() + bytes.len();
        if needed > self.maximum {
            return Err(io::Error::new(io::ErrorKind::InvalidData, file_capacity()));
        }
        if needed > self.units.capacity() {
            let capacity = needed
                .max(self.units.capacity().saturating_mul(2))
                .min(self.maximum);
            self.units.reserve_exact(capacity - self.units.len());
        }
        self.units.extend(bytes.iter().copied().map(u16::from));
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
#[path = "../../../../../tests/rust/agent-claude-file-read.rs"]
mod file_read_cases;

fn prepare_worktree(
    home: &std::path::Path,
    enabled: bool,
    root: Option<&std::path::Path>,
    id: &JsString,
    cwd: &JsString,
) -> Result<JsString> {
    if !enabled {
        return Ok(cwd.clone());
    }
    let root = root
        .map(std::path::Path::to_path_buf)
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
async fn git_output(
    cwd: &JsString,
    args: &[&str],
    job_launcher: Option<PathBuf>,
) -> Option<Vec<u8>> {
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
            job_launcher,
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
async fn git_diff(cwd: JsString, job_launcher: Option<PathBuf>) -> Result<JsString> {
    let Some(tracked) =
        git_output(&cwd, &["diff", "--no-ext-diff", "--"], job_launcher.clone()).await
    else {
        return Ok("".into());
    };
    let mut diffs = vec![String::from_utf8_lossy(&tracked).into_owned()];
    if let Some(paths) = git_output(
        &cwd,
        &["ls-files", "--others", "--exclude-standard", "-z"],
        job_launcher,
    )
    .await
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
