//! Intentional Node user workloads, owned by the shared process-tree runtime.
use crate::{engine::{RegisterJob, Runner}, model, policy, schema};
use ivy_contracts::{IvyError, IvyJson, IvyResult, json::{parse, stringify}};
use ivy_host_runtime::{node_runtime::NodeRuntime, process::{self, Command, LaunchContext}};
use ivy_sdk::service::ServiceFuture;
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, ffi::OsString, path::{Path, PathBuf}, sync::Arc, time::Duration};
use tokio::{fs, io::AsyncWriteExt};
use tokio_util::sync::CancellationToken;

/// The native adapter owns the session and its run-local HTTP bridge together.
pub trait McpBridge: Send + Sync {
    fn context(&self) -> IvyJson;
    fn close(&self) -> ServiceFuture<()>;
}
pub trait McpBridgeFactory: Send + Sync {
    fn open(&self, configuration: IvyJson, signal: CancellationToken) -> ServiceFuture<Arc<dyn McpBridge>>;
}

pub struct TaskRunner {
    work_root: PathBuf,
    artifact_root: PathBuf,
    node: NodeRuntime,
    settings: IvyJson,
    job_launcher: Option<PathBuf>,
    mcp: Option<Arc<dyn McpBridgeFactory>>,
}
fn io_error(_: std::io::Error) -> IvyError { IvyError::internal() }
fn text_bytes(value: &IvyJson) -> IvyResult<Vec<u8>> {
    let text = value.as_js_string().ok_or_else(IvyError::internal)?;
    Ok(String::from_utf16_lossy(&text.0).into_bytes())
}
fn check_signal(signal: &CancellationToken) -> IvyResult<()> {
    if signal.is_cancelled() { Err(IvyError::internal()) } else { Ok(()) }
}
fn remove(path: &Path) -> std::io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => std::fs::remove_dir_all(path),
        Ok(_) => std::fs::remove_file(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}
async fn remove_async(path: PathBuf) -> IvyResult<()> {
    tokio::task::spawn_blocking(move || remove(&path)).await.map_err(|_| IvyError::internal())?.map_err(io_error)
}
async fn write_private(path: &Path, bytes: &[u8]) -> IvyResult<()> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)] options.mode(0o600);
    let mut file = options.open(path).await.map_err(io_error)?;
    file.write_all(bytes).await.map_err(io_error)
}
impl TaskRunner {
    pub async fn new(work_root: PathBuf, artifact_root: PathBuf, node: Option<&Path>, settings: IvyJson,
        job_launcher: Option<PathBuf>, mcp: Option<Arc<dyn McpBridgeFactory>>) -> IvyResult<Self> {
        let settings = schema::parse_schema("settingsSchema", &settings)?;
        let node = NodeRuntime::discover(node).await?;
        Ok(Self { work_root: std::path::absolute(work_root).map_err(io_error)?,
            artifact_root, node, settings, job_launcher, mcp })
    }
    fn directory(&self, task_id: &str) -> IvyResult<PathBuf> {
        let valid = !task_id.is_empty() && task_id.len() <= 64 && task_id.bytes().enumerate().all(|(i,b)|
            b.is_ascii_lowercase() || b.is_ascii_digit() || (i > 0 && matches!(b,b'_'|b'-')));
        if !valid { return Err(IvyError::new("invalid_arguments", "Invalid task authentication directory.")); }
        Ok(self.work_root.join(task_id))
    }
    pub fn job_name(&self, task_id: &str) -> IvyResult<String> {
        let path = self.directory(task_id)?;
        // Node's resolve() uses the normal drive/UNC spelling, never Rust's
        // canonicalization-only extended path prefix.
        let text = path.to_string_lossy();
        let text = text.strip_prefix(r"\\?\UNC\").map(|v| format!(r"\\{v}"))
            .unwrap_or_else(|| text.strip_prefix(r"\\?\").unwrap_or(&text).to_owned());
        Ok(format!("Global\\Ivy.Collector.{:x}", Sha256::digest(text.to_lowercase().as_bytes())))
    }
    async fn execute(&self, cwd: &Path, args: Vec<OsString>, timeout_ms: u64,
        signal: &CancellationToken, input: &[u8], job_name: Option<&String>) -> IvyResult<()> {
        check_signal(signal)?;
        let command = Command { executable: "node".into(), args, cwd: None, timeout_ms };
        let context = LaunchContext {
            execution_cwd: Some(cwd.to_owned()), maximum_output_bytes: Some(65536),
            job_launcher: self.job_launcher.clone(), job_name: job_name.cloned(),
            environment: BTreeMap::from([
                ("HOME".into(), cwd.as_os_str().to_owned()),
                ("USERPROFILE".into(), cwd.as_os_str().to_owned()),
                ("CODEX_HOME".into(), cwd.join("home").into_os_string()),
                ("NODE_OPTIONS".into(), "".into()),
                ("NPM_CONFIG_USERCONFIG".into(), cwd.join(".npmrc").into_os_string()),
                ("NPM_CONFIG_CACHE".into(), cwd.join(".npm-cache").into_os_string()),
            ]), ..Default::default()
        };
        let mut run = process::spawn(&command, &self.artifact_root,
            &BTreeMap::from([("node".into(), self.node.executable.clone())]), context).await?;
        let completion = run.completion.clone();
        let stdin = run.stdin.take();
        let execution = async move {
            if let Some(mut stdin) = stdin {
                // Like child.stdin.end(): a broken pipe is not script success or
                // failure evidence. Always observe the owned tree's completion.
                let _ = stdin.write_all(input).await;
                let _ = stdin.shutdown().await;
            }
            completion.wait().await
        };
        let mut expired = false;
        let result = tokio::select! {
            result = execution => result,
            _ = signal.cancelled() => run.stop(Duration::from_millis(100)).await?,
            _ = tokio::time::sleep(Duration::from_millis(timeout_ms)) => {
                expired = true;
                run.stop(Duration::from_millis(100)).await?
            }
        };
        if result.error_code.as_deref() == Some("outcome_unknown") {
            return Err(IvyError::new("outcome_unknown", "Task process termination could not be confirmed."));
        }
        check_signal(signal)?;
        if expired { return Err(IvyError::new("deadline_exceeded", "Task exceeded its time limit.")); }
        if result.exit_code != Some(0) || result.error_code.is_some() {
            let (code, message) = policy::worker_failure(if input.is_empty() { None } else { result.exit_code });
            return Err(IvyError::new(code, message));
        }
        Ok(())
    }
    async fn run_task(&self, task: IvyJson, state: IvyJson, input: IvyJson,
        signal: CancellationToken, register: RegisterJob) -> IvyResult<IvyJson> {
        let id = model::text(&task, "id")?;
        let job_name = if cfg!(windows) { Some(self.job_name(&id)?) } else { None };
        if let Some(name) = &job_name { register(name.clone()).await?; }
        let cwd = self.directory(&id)?;
        fs::create_dir_all(&cwd).await.map_err(io_error)?;
        let dependencies = model::field(&task, "dependencies")?;
        let hash = format!("{:x}", Sha256::digest(stringify(dependencies,usize::MAX)?.as_bytes()));
        let installed = fs::read_to_string(cwd.join("dependencies.sha256")).await.unwrap_or_default();
        if installed != hash {
            let package = IvyJson::object([("name", "collector-task".into()), ("private", true.into()),
                ("type", "module".into()), ("dependencies", dependencies.clone())]);
            fs::write(cwd.join("package.json"), stringify(&package,usize::MAX)?).await.map_err(io_error)?;
            if !dependencies.as_object().ok_or_else(IvyError::internal)?.is_empty() {
                let configured = model::optional_text(&self.settings, "npmCliPath").map(PathBuf::from);
                let cli = self.node.npm_cli(configured.as_deref()).map_err(|_| IvyError::new(
                    "npm_unavailable", "Configure npmCliPath for tasks with npm dependencies."))?;
                let mut args = vec![cli.into_os_string()];
                args.extend(["install","--omit=dev","--ignore-scripts","--no-audit","--no-fund","--loglevel=error"].map(OsString::from));
                self.execute(&cwd,args,120000,&signal,&[],job_name.as_ref()).await?;
            } else if !installed.is_empty() { remove_async(cwd.join("node_modules")).await?; }
            fs::write(cwd.join("dependencies.sha256"), hash).await.map_err(io_error)?;
        }
        remove_async(cwd.join("result.json")).await?;
        write_private(&cwd.join("task.mjs"), &text_bytes(model::field(&task,"script")?)?).await?;
        fs::write(cwd.join("worker.mjs"), policy::WORKER).await.map_err(io_error)?;
        let mut secrets = model::object();
        if let Some(IvyJson::Array(names)) = task.get("secretNames") {
            for name in names {
                let key = name.as_js_string().ok_or_else(IvyError::internal)?;
                let configured = model::field(&self.settings,"secrets")?.get_key(key).ok_or_else(|| IvyError::new(
                    "secret_missing", format!("A task secret is not configured: {}",String::from_utf16_lossy(&key.0))))?;
                secrets.insert(key.clone(),configured.clone());
            }
        }
        let timeout_ms = (model::number(&task,"timeoutSeconds")?.min(model::number(&self.settings,"maximumTimeoutSeconds")?) * 1000.0) as u64;
        let task_signal = signal.child_token();
        let timer_signal = task_signal.clone();
        let timer = tokio::spawn(async move { tokio::time::sleep(Duration::from_millis(timeout_ms)).await; timer_signal.cancel(); });
        struct Timer(tokio::task::JoinHandle<()>);
        impl Drop for Timer { fn drop(&mut self) { self.0.abort(); } }
        let _timer = Timer(timer);
        let bridge = if let Some(configuration) = task.get("mcp") {
            let factory = self.mcp.as_ref().ok_or_else(|| IvyError::new("configuration_invalid", "Task MCP is not configured."))?;
            Some(factory.open(configuration.clone(),task_signal).await?)
        } else { None };
        let result = async {
            let mut context = IvyJson::object([("config",model::field(&task,"config")?.clone()),("secrets",secrets),
                ("state",state),("input",input),("timeoutMs",IvyJson::Number(timeout_ms as f64))]);
            if let Some(bridge) = &bridge { context.insert("mcp",bridge.context()); }
            let memory = model::number(&task,"memoryMb")?.min(model::number(&self.settings,"maximumMemoryMb")?);
            self.execute(&cwd,vec![format!("--max-old-space-size={memory}").into(),"worker.mjs".into()],
                timeout_ms,&signal,stringify(&context,usize::MAX)?.as_bytes(),job_name.as_ref()).await?;
            let result_path = cwd.join("result.json");
            if fs::metadata(&result_path).await.map_err(io_error)?.len() > policy::WORKER_BYTES as u64 {
                return Err(IvyError::new("limit_exceeded", "Task result exceeds 1 MB."));
            }
            let bytes = fs::read(&result_path).await.map_err(io_error)?;
            let output = parse(&String::from_utf8_lossy(&bytes),usize::MAX).map_err(|_| IvyError::internal())?;
            let output = schema::parse_schema("outputSchema",&output)?;
            remove_async(result_path).await?;
            Ok(output)
        }.await;
        if let Some(bridge) = bridge { bridge.close().await?; }
        result
    }
}
impl Runner for TaskRunner {
    fn run(&self, task: IvyJson, state: IvyJson, input: IvyJson, signal: CancellationToken, register: RegisterJob) -> ServiceFuture<IvyJson> {
        // Runner futures are 'static; retain the immutable launch configuration.
        let runner = Self { work_root:self.work_root.clone(),artifact_root:self.artifact_root.clone(),node:self.node.clone(),
            settings:self.settings.clone(),job_launcher:self.job_launcher.clone(),mcp:self.mcp.clone() };
        Box::pin(async move { runner.run_task(task,state,input,signal,register).await })
    }
    fn stopped(&self, task_id: String, job_name: String) -> ServiceFuture<bool> {
        let expected = self.job_name(&task_id);
        Box::pin(async move {
            if !cfg!(windows) || expected? != job_name { return Ok(false); }
            #[cfg(windows)] { return tokio::task::spawn_blocking(move || ivy_host_runtime::platform::windows_job::job_stopped(&job_name))
                .await.map_err(|_| IvyError::internal())?.map_err(io_error); }
            #[cfg(not(windows))] Ok(false)
        })
    }
    fn clear_authentication(&self, task_id: &str) -> IvyResult<()> { remove(&self.directory(task_id)?.join(".auth")).map_err(io_error) }
}
