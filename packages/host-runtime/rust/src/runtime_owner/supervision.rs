use super::{
    policy::{self, ExitAction, Policy},
    stop,
};
use crate::{
    IvyError, IvyJson, IvyResult,
    accepted_configuration::same_installation,
    config::{self, CONFIGURATION_BYTES, InstanceConfig},
    executor_lock::ExecutorLock,
    journal::HostJournal,
    layout::service_paths,
    node_path::{from_native, to_native},
    node_runtime::NodeRuntime,
    process::{self, Command, LaunchContext, OwnedProcess, ProcessResult},
};
use ivy_contracts::{JsString, Outcome, core_schema::validate_host, json};
use std::{
    collections::BTreeMap,
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

/// Must resolve the immutable HostConfig associated with this exact target.
/// The host authority owns association lookup and the approved legacy fallback.
/// There is deliberately no default that treats a missing implementation as legacy.
pub type TargetPolicyResolver = Arc<
    dyn Fn(HostJournal, IvyJson) -> Pin<Box<dyn Future<Output = IvyResult<IvyJson>> + Send>>
        + Send
        + Sync,
>;

struct Control {
    pending: Mutex<Option<Owner>>,
    cancel: CancellationToken,
    result: watch::Receiver<Option<IvyResult<()>>>,
}
impl Drop for Control {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

/// One Windows process owner. Cancellation requests shutdown; all close callers
/// observe the same completed cleanup, even when an earlier waiter is dropped.
#[derive(Clone)]
pub struct RuntimeOwner(Arc<Control>);
impl RuntimeOwner {
    pub async fn open(
        host: IvyJson,
        instance_id: String,
        bootstrap_root: PathBuf,
        node: NodeRuntime,
        resolve_policy: TargetPolicyResolver,
    ) -> IvyResult<Self> {
        require(
            cfg!(windows),
            "unsupported_runtime",
            "RuntimeOwner is reserved for Windows process services; Linux uses systemd directly.",
        )?;
        validate_host("HostConfig", &host)?;
        let instance = instance(&host, &instance_id)?;
        require(
            instance.get("engine") == Some(&IvyJson::from("process")),
            "unsupported_runtime",
            "RuntimeOwner cannot own Docker instances.",
        )?;
        let lock = ExecutorLock::acquire(
            path(host.required("runtimeRoot")?)?
                .join("owners")
                .join(&instance_id),
            false,
        )
        .await?;
        let journal = match HostJournal::open(host.clone()).await {
            Ok(value) => value,
            Err(error) => {
                let _ = lock.close().await;
                return Err(error);
            }
        };
        let previous = match journal.observations().await {
            Ok(value) => value,
            Err(error) => {
                let _ = journal.close().await;
                let _ = lock.close().await;
                return Err(error);
            }
        };
        let previous = previous.get(&instance_id);
        let blocked = previous.is_some_and(|value| {
            value.get("state") == Some(&IvyJson::from("unknown"))
                && value.get("code") == Some(&IvyJson::from("outcome_unknown"))
                && value.get("ownerPid") == Some(&IvyJson::Number(std::process::id() as f64))
        });
        let cancel = CancellationToken::new();
        let (finished, result) = watch::channel(None);
        let owner = Owner {
            host,
            instance_id,
            bootstrap_root,
            node,
            resolve_policy,
            lock,
            journal,
            boot_id: uuid::Uuid::new_v4().to_string(),
            cancel: cancel.clone(),
            finished,
            epoch: Instant::now(),
            policy: Policy::new(blocked),
            selected: None,
            child: None,
            restart_code: "restart_backoff".into(),
            restart_at: None,
            last_observation: String::new(),
        };
        Ok(Self(Arc::new(Control {
            pending: Mutex::new(Some(owner)),
            cancel,
            result,
        })))
    }
    pub fn start(&self) -> IvyResult<()> {
        let owner = self
            .0
            .pending
            .lock()
            .map_err(|_| IvyError::internal())?
            .take();
        if let Some(owner) = owner {
            tokio::spawn(owner.run());
        }
        Ok(())
    }
    pub async fn completion(&self) -> IvyResult<()> {
        if self
            .0
            .pending
            .lock()
            .map_err(|_| IvyError::internal())?
            .is_some()
        {
            return Ok(());
        }
        let mut result = self.0.result.clone();
        loop {
            if let Some(value) = result.borrow_and_update().clone() {
                return value;
            }
            result.changed().await.map_err(|_| IvyError::internal())?;
        }
    }
    pub async fn close(&self) -> IvyResult<()> {
        self.0.cancel.cancel();
        self.start()?;
        self.completion().await
    }
}

struct Selected {
    target: IvyJson,
    host: IvyJson,
    config: Option<InstanceConfig>,
    manifest: IvyJson,
}
struct Owner {
    host: IvyJson,
    instance_id: String,
    bootstrap_root: PathBuf,
    node: NodeRuntime,
    resolve_policy: TargetPolicyResolver,
    lock: ExecutorLock,
    journal: HostJournal,
    boot_id: String,
    cancel: CancellationToken,
    finished: watch::Sender<Option<IvyResult<()>>>,
    epoch: Instant,
    policy: Policy,
    selected: Option<Selected>,
    child: Option<OwnedProcess>,
    restart_code: String,
    restart_at: Option<String>,
    last_observation: String,
}
impl Owner {
    fn elapsed(&self) -> u64 {
        self.epoch.elapsed().as_millis().min(u64::MAX as u128) as u64
    }
    async fn observe(
        &mut self,
        state: &str,
        code: Option<&str>,
        message: &str,
        health: Option<IvyJson>,
    ) -> IvyResult<()> {
        let target = self.selected.as_ref().map(|value| &value.target);
        let build = self
            .selected
            .as_ref()
            .and_then(|value| value.config.as_ref())
            .and_then(|config| config.json().get("buildId"))
            .cloned()
            .unwrap_or(IvyJson::Null);
        let next = self
            .restart_at
            .clone()
            .map(IvyJson::from)
            .unwrap_or(IvyJson::Null);
        let mut value = IvyJson::object([
            ("schemaVersion", IvyJson::Number(1.0)),
            ("instanceId", self.instance_id.clone().into()),
            ("ownerPid", IvyJson::Number(std::process::id() as f64)),
            ("ownerBootId", self.boot_id.clone().into()),
            ("observedAt", now().into()),
            (
                "targetRevision",
                target
                    .and_then(|v| v.get("revision"))
                    .cloned()
                    .unwrap_or(IvyJson::Null),
            ),
            (
                "candidateId",
                target
                    .and_then(|v| v.get("candidateId"))
                    .cloned()
                    .unwrap_or(IvyJson::Null),
            ),
            ("buildId", build),
            ("state", state.into()),
            ("health", health.unwrap_or(IvyJson::Null)),
            (
                "restartCount",
                IvyJson::Number(self.policy.restart_count as f64),
            ),
            ("nextRestartAt", next),
            ("code", code.map(IvyJson::from).unwrap_or(IvyJson::Null)),
            (
                "message",
                IvyJson::String(JsString(message.encode_utf16().take(4096).collect())),
            ),
        ]);
        let original = value.clone();
        value.insert("observedAt", "".into());
        if let Some(mut health) = value.get("health").filter(|v| !v.is_null()).cloned() {
            health.insert("observedAt", "".into());
            value.insert("health", health);
        }
        let semantic = json::stringify(&value, ivy_contracts::limits::MANAGEMENT_FRAME_BYTES)?;
        if semantic != self.last_observation {
            self.journal
                .record_observation(&self.instance_id, original)
                .await?;
            self.last_observation = semantic;
        }
        Ok(())
    }
    async fn stop_child(&mut self) -> IvyResult<()> {
        if self.child.is_none() {
            return Ok(());
        }
        let mut observed = self.observe("draining", None, "", None).await;
        let selected = self.selected.as_ref().ok_or_else(IvyError::internal)?;
        let data = match &selected.config {
            Some(value) => value.data_root().to_owned(),
            None => path(
                service_paths(&selected.host, &self.instance_id.clone().into())?
                    .required("data")?,
            )?,
        };
        let launch = selected
            .target
            .required("revision")?
            .as_js_string()
            .ok_or_else(IvyError::internal)?
            .clone();
        let instance_id = JsString::from(self.instance_id.clone());
        let grace = number(selected.manifest.get("shutdown"), "timeoutMs", 15_000);
        let request = async move {
            let health = config::json_file(data.join("health.json"), CONFIGURATION_BYTES).await?;
            validate_host("Health", &health)?;
            if let Some(control) = stop::shutdown_control(&health, &instance_id, &launch, &now()) {
                config::atomic_json(data.join("control.json"), control).await?;
            }
            Ok(())
        };
        let result = stop::stop_child(
            self.child.as_ref().unwrap(),
            Duration::from_millis(grace),
            request,
        )
        .await;
        self.child = None;
        if let Err(error) = result {
            if error.code == "outcome_unknown" {
                self.policy.blocked = true;
                let _ = self.observe("unknown", Some("outcome_unknown"), "The owned process tree did not establish a stopped outcome; no replacement will start.", None).await;
            }
            return Err(error);
        }
        retain(
            &mut observed,
            self.observe("stopped", None, "Owned process stopped.", None)
                .await,
        );
        observed
    }
    async fn load(&self, target: IvyJson) -> IvyResult<Selected> {
        let host = (self.resolve_policy)(self.journal.clone(), target.clone()).await?;
        validate_host("HostConfig", &host)?;
        same_installation(&self.host, &host)?;
        let config_path = path(target.required("configPath")?)?;
        require(
            crate::paths::inside(&path(host.required("runtimeRoot")?)?, &config_path)
                .map_err(|_| IvyError::internal())?,
            "target_conflict",
            "Instance configuration leaves host runtime data.",
        )?;
        let manifest = self
            .journal
            .manifest(&target.required("candidateId")?.as_str()?)
            .await?;
        if target.get("desired") == Some(&IvyJson::from("stopped")) {
            let config = config::instance_config(config_path, &self.node).await.ok();
            return Ok(Selected {
                target,
                host,
                config,
                manifest,
            });
        }
        let config = config::instance_config(config_path, &self.node).await?;
        require(
            config.json().get("instanceId") == Some(&self.instance_id.clone().into())
                && config.json().get("hostId") == host.get("hostId"),
            "target_conflict",
            "Runtime target has no matching local identity.",
        )?;
        let candidate = self
            .journal
            .candidate(&target.required("candidateId")?.as_str()?)
            .await?;
        require(
            config.json().get("artifactRoot") == candidate.get("artifactRoot")
                && config.json().get("dataRoot")
                    == service_paths(&host, &self.instance_id.clone().into())?.get("data"),
            "build_mismatch",
            "Runtime target identity does not match its accepted candidate or service storage.",
        )?;
        require(
            manifest.get("connectsToHive") != Some(&IvyJson::Bool(true))
                || config
                    .json()
                    .get("credential")
                    .is_some_and(|v| v.as_js_string().is_some_and(|v| !v.0.is_empty())),
            "invalid_arguments",
            "A Hive service requires its own credential.",
        )?;
        let selected = Selected {
            target,
            host,
            config: Some(config),
            manifest,
        };
        self.verify_launch(&selected).await?; // Verify the next release before draining a healthy child.
        Ok(selected)
    }
    async fn verify_launch(&self, selected: &Selected) -> IvyResult<()> {
        let config = selected.config.as_ref().ok_or_else(IvyError::internal)?;
        let command = command(selected.manifest.required("entrypoint")?)?;
        let executables = executables(&selected.host)?;
        process::resolve_command(&command, config.artifact_root(), &executables)?;
        let candidate = self
            .journal
            .candidate(&selected.target.required("candidateId")?.as_str()?)
            .await?;
        let selected_entry = if command.executable == "node" {
            command.args.first().map(|v| from_native(Path::new(v)))
        } else {
            Some(command.executable.clone().into())
        };
        crate::artifact::verify_launch_candidate(
            candidate,
            path(selected.host.required("artifactRoot")?)?,
            selected_entry,
        )
        .await?;
        let requirements = selected.manifest.required("requirements")?;
        let os = requirements
            .get("os")
            .and_then(IvyJson::as_array)
            .map(|v| v.iter().map(IvyJson::as_str).collect::<IvyResult<Vec<_>>>())
            .transpose()?;
        let arch = requirements
            .get("arch")
            .and_then(IvyJson::as_array)
            .map(|v| v.iter().map(IvyJson::as_str).collect::<IvyResult<Vec<_>>>())
            .transpose()?;
        let os_refs = os
            .as_ref()
            .map(|v| v.iter().map(String::as_str).collect::<Vec<_>>());
        let arch_refs = arch
            .as_ref()
            .map(|v| v.iter().map(String::as_str).collect::<Vec<_>>());
        require(
            crate::package_requirements::runtime_requirements_satisfied(
                &crate::package_requirements::Requirements {
                    node: &requirements.required("node")?.as_str()?,
                    os: os_refs.as_deref(),
                    arch: arch_refs.as_deref(),
                },
                &crate::package_requirements::Runtime {
                    node: &self.node.version,
                    os: if cfg!(windows) { "win32" } else { "linux" },
                    arch: if cfg!(target_arch = "aarch64") {
                        "arm64"
                    } else {
                        "x64"
                    },
                },
            )?,
            "unsupported_runtime",
            "Selected release does not support the configured Node runtime and platform.",
        )?;
        Ok(())
    }
    async fn launch(&mut self) -> IvyResult<()> {
        let selected = self.selected.as_ref().ok_or_else(IvyError::internal)?;
        let refreshed =
            config::instance_config(path(selected.target.required("configPath")?)?, &self.node)
                .await?;
        let previous = selected
            .config
            .as_ref()
            .ok_or_else(IvyError::internal)?
            .json();
        require(
            [
                "instanceId",
                "hostId",
                "componentId",
                "serviceNodeId",
                "artifactRoot",
                "buildId",
                "dataRoot",
            ]
            .iter()
            .all(|key| refreshed.json().get(key) == previous.get(key)),
            "configuration_changed",
            "Reloaded instance configuration no longer belongs to the selected release and storage owner.",
        )?;
        self.selected.as_mut().unwrap().config = Some(refreshed);
        self.verify_launch(self.selected.as_ref().unwrap()).await?;
        self.observe("starting", None, "", None).await?;
        let selected = self.selected.as_ref().unwrap();
        let config = selected.config.as_ref().unwrap();
        let mut command = command(selected.manifest.required("entrypoint")?)?;
        let mut context = self.context(selected)?;
        if command.executable == "node"
            && command
                .args
                .first()
                .is_some_and(|v| v.to_string_lossy().starts_with("dist/"))
        {
            if let Some(work) = config.json().get("workRoot") {
                let root = path(work)?;
                tokio::fs::create_dir_all(&root)
                    .await
                    .map_err(|_| IvyError::internal())?;
                command.args[0] = config
                    .artifact_root()
                    .join(&command.args[0])
                    .into_os_string();
                context.execution_cwd = Some(root);
            }
        }
        if let Some(logs) = config.json().get("logsRoot") {
            let root = path(logs)?;
            let log = tokio::task::spawn_blocking(move || crate::logs::ServiceLog::open(&root))
                .await
                .map_err(|_| IvyError::internal())??;
            let log = Mutex::new(log);
            context.on_output = Some(Arc::new(move |stream, text| {
                if let Ok(mut log) = log.lock() {
                    let _ = log.append(stream, text, &now());
                }
            }));
        }
        let instance = instance(&selected.host, &self.instance_id)?;
        context.allow_windows_breakaway = instance
            .get("process")
            .and_then(|v| v.get("allowWindowsBreakaway"))
            == Some(&IvyJson::Bool(true));
        self.child = Some(
            process::spawn(
                &command,
                config.artifact_root(),
                &executables(&selected.host)?,
                context,
            )
            .await?,
        );
        self.policy.launched(self.elapsed());
        self.restart_at = None;
        Ok(())
    }
    fn context(&self, selected: &Selected) -> IvyResult<LaunchContext> {
        let mut environment = BTreeMap::new();
        environment.insert(
            "IVY_INSTANCE_CONFIG".into(),
            path(selected.target.required("configPath")?)?.into_os_string(),
        );
        environment.insert(
            "IVY_LAUNCH_ID".into(),
            to_native(
                selected
                    .target
                    .required("revision")?
                    .as_js_string()
                    .ok_or_else(IvyError::internal)?,
            )
            .into_os_string(),
        );
        Ok(LaunchContext {
            environment,
            job_launcher: Some(self.bootstrap_root.join("dist/native/ivy-job.exe")),
            redact: secrets(&selected.host, selected.config.as_ref())?,
            ..Default::default()
        })
    }
    async fn backoff(&mut self, code: &str) -> IvyResult<()> {
        let restart = self
            .selected
            .as_ref()
            .and_then(|v| v.manifest.get("restart"));
        self.policy.backoff(
            self.elapsed(),
            number(restart, "minimumDelayMs", 1000),
            number(restart, "maximumDelayMs", 60_000),
        );
        self.restart_code = code.into();
        self.restart_at = self.policy.next_restart.map(|at| {
            (chrono::Utc::now()
                + chrono::Duration::milliseconds(
                    at.saturating_sub(self.elapsed()).min(i64::MAX as u64) as i64,
                ))
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
        });
        self.observe(
            "failed",
            Some(code),
            "Readiness or process exit failed; restart backoff is active.",
            None,
        )
        .await
    }
    async fn tick(&mut self) -> IvyResult<()> {
        if self.policy.blocked {
            return self
                .observe(
                    "unknown",
                    Some("outcome_unknown"),
                    "Runtime ownership is blocked until the last process outcome is reconciled.",
                    None,
                )
                .await;
        }
        let Some(target) = self.journal.target(&self.instance_id).await? else {
            self.stop_child().await?;
            self.selected = None;
            return self.observe("idle", None, "", None).await;
        };
        if target.get("desired") == Some(&IvyJson::from("stopped")) {
            self.stop_child().await?;
            if self
                .selected
                .as_ref()
                .and_then(|v| v.target.get("revision"))
                != target.get("revision")
            {
                self.selected = Some(self.load(target).await?);
            }
            return self.observe("stopped", None, "", None).await;
        }
        if self
            .selected
            .as_ref()
            .and_then(|v| v.target.get("revision"))
            != target.get("revision")
        {
            let selected = self.load(target.clone()).await?;
            self.stop_child().await?;
            self.selected = Some(selected);
            self.policy.select_running_target();
            self.restart_at = None;
        }
        if let Some(result) = self.child.as_ref().and_then(|v| v.completion.current()) {
            self.child = None;
            let restart = self
                .selected
                .as_ref()
                .unwrap()
                .manifest
                .required("restart")?
                .required("policy")?
                .as_str()?;
            match self
                .policy
                .exited(&restart, result.exit_code, result.error_code.as_deref())
            {
                ExitAction::Blocked => return self
                    .observe(
                        "unknown",
                        Some("outcome_unknown"),
                        "The owned process outcome is unknown; no second launch will be attempted.",
                        None,
                    )
                    .await,
                ExitAction::Exited => {
                    let code = process_code(&result);
                    return self
                        .observe(
                            "exited",
                            if result.exit_code != Some(0) || result.error_code.is_some() {
                                Some(code.as_str())
                            } else {
                                None
                            },
                            "",
                            None,
                        )
                        .await;
                }
                ExitAction::Restart => self.backoff(&process_code(&result)).await?,
            }
        }
        if self.child.is_none() {
            if !self.policy.restart_allowed {
                return self.observe("exited", None, "", None).await;
            }
            if !self.policy.can_launch(self.elapsed()) {
                let code = self.restart_code.clone();
                return self.observe("failed", Some(&code), "", None).await;
            }
            if let Err(error) = self.launch().await {
                if error.code == "outcome_unknown" {
                    self.policy.blocked = true;
                    return self
                        .observe("unknown", Some(&error.code), &error.message, None)
                        .await;
                }
                return self.backoff(&error.code).await;
            }
        }
        let selected = self.selected.as_ref().unwrap();
        let config = selected.config.as_ref().ok_or_else(IvyError::internal)?;
        let mut health = None;
        let checked = crate::health::check_health(config, None, !self.policy.manifest_ready).await;
        if checked
            .as_ref()
            .is_err_and(|error| error.code == "outcome_unknown")
        {
            self.policy.unready(self.elapsed());
            return self
                .observe(
                    "starting",
                    Some("service_not_ready"),
                    "Health dependency outcome is not yet observable.",
                    None,
                )
                .await;
        }
        let result = async {
            let value = checked?;
            require(
                value.get("launchId") == target.get("revision"),
                "service_not_ready",
                "Health belongs to a previous launch.",
            )?;
            health = Some(value);
            if !self.policy.manifest_ready {
                let command = command(
                    selected
                        .manifest
                        .required("readiness")?
                        .required("command")?,
                )?;
                let mut context = self.context(selected)?;
                let args = command
                    .args
                    .iter()
                    .map(|v| v.to_string_lossy().to_string())
                    .collect::<Vec<_>>();
                context.without_job = policy::direct_health_probe(&command.executable, &args);
                process::run_command(
                    &command,
                    config.artifact_root(),
                    &executables(&selected.host)?,
                    context,
                    None,
                )
                .await?;
            }
            Ok::<(), IvyError>(())
        }
        .await;
        match result {
            Ok(()) => {
                self.policy.ready(self.elapsed());
                self.observe("ready", None, "", health).await
            }
            Err(error) if error.code == "outcome_unknown" => {
                self.policy.blocked = true;
                self.observe("unknown", Some(&error.code), &error.message, health)
                    .await
            }
            Err(error) => {
                self.policy.unready(self.elapsed());
                if self.policy.readiness_expired(
                    self.elapsed(),
                    number(
                        self.selected
                            .as_ref()
                            .and_then(|v| v.manifest.get("readiness")),
                        "timeoutMs",
                        30_000,
                    ),
                ) {
                    self.stop_child().await?;
                    self.backoff("readiness_deadline").await
                } else {
                    self.observe(
                        "starting",
                        Some("service_not_ready"),
                        &format!("Readiness failed: {}.", error.code),
                        health,
                    )
                    .await
                }
            }
        }
    }
    async fn run(mut self) {
        while !self.cancel.is_cancelled() {
            if let Err(error) = self.tick().await {
                if error.code == "outcome_unknown" {
                    self.policy.blocked = true;
                }
                let _ = self
                    .observe(
                        if self.policy.blocked {
                            "unknown"
                        } else {
                            "failed"
                        },
                        Some(&error.code),
                        &error.message,
                        None,
                    )
                    .await;
            }
            tokio::select! { _ = self.cancel.cancelled() => break, _ = tokio::time::sleep(Duration::from_millis(500)) => () }
        }
        let mut result = self.stop_child().await;
        if !self.policy.blocked {
            retain(
                &mut result,
                self.observe("stopped", None, "Runtime owner stopped.", None)
                    .await,
            );
        }
        retain(&mut result, self.journal.close().await);
        retain(&mut result, self.lock.close().await);
        self.finished.send_replace(Some(result));
    }
}

fn retain(result: &mut IvyResult<()>, next: IvyResult<()>) {
    if result.is_ok() {
        *result = next;
    }
}
fn require(ok: bool, code: &str, message: &str) -> IvyResult<()> {
    if ok {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}
fn path(value: &IvyJson) -> IvyResult<PathBuf> {
    value
        .as_js_string()
        .map(to_native)
        .ok_or_else(IvyError::internal)
}
fn instance<'a>(host: &'a IvyJson, id: &str) -> IvyResult<&'a IvyJson> {
    host.required("instances")?
        .as_array()
        .ok_or_else(IvyError::internal)?
        .iter()
        .find(|v| v.get("instanceId") == Some(&IvyJson::from(id)))
        .ok_or_else(|| IvyError::new("not_found", "Service instance is not configured."))
}
fn number(value: Option<&IvyJson>, key: &str, fallback: u64) -> u64 {
    value
        .and_then(|v| v.get(key))
        .and_then(IvyJson::as_f64)
        .map(|v| v as u64)
        .unwrap_or(fallback)
}
fn command(value: &IvyJson) -> IvyResult<Command> {
    Ok(Command {
        executable: value.required("executable")?.as_str()?,
        args: value
            .required("args")?
            .as_array()
            .ok_or_else(IvyError::internal)?
            .iter()
            .map(|v| path(v).map(PathBuf::into_os_string))
            .collect::<IvyResult<_>>()?,
        cwd: value.get("cwd").map(path).transpose()?,
        timeout_ms: number(Some(value), "timeoutMs", 30_000),
    })
}
fn executables(host: &IvyJson) -> IvyResult<BTreeMap<String, PathBuf>> {
    let IvyJson::Object(values) = host.required("executables")? else {
        return Err(IvyError::internal());
    };
    values
        .iter()
        .map(|(key, value)| Ok((key.to_utf8()?, path(value)?)))
        .collect()
}
fn process_code(result: &ProcessResult) -> String {
    let mut code = result.error_code.clone().unwrap_or_else(|| {
        if result.exit_code == Some(0) {
            "process_stopped".into()
        } else {
            "process_exited".into()
        }
    });
    let lines = result.stderr.trim().lines().collect::<Vec<_>>();
    for line in lines.iter().skip(lines.len().saturating_sub(4)) {
        if let Ok(value) = json::parse(line.trim_end_matches('\r'), 65536) {
            if let Some(candidate) = value
                .get("code")
                .and_then(|v| v.as_str().ok())
                .filter(|v| policy::valid_process_code(v))
            {
                code = candidate;
            }
        }
    }
    code
}
fn secrets(host: &IvyJson, config: Option<&InstanceConfig>) -> IvyResult<Vec<JsString>> {
    fn collect(value: &IvyJson, result: &mut Vec<JsString>) {
        match value {
            IvyJson::String(value) if !value.0.is_empty() => {
                if !result.contains(value) {
                    result.push(value.clone());
                }
            }
            IvyJson::Array(values) => values.iter().for_each(|v| collect(v, result)),
            IvyJson::Object(values) => values.iter().for_each(|(_, v)| collect(v, result)),
            _ => (),
        }
    }
    fn selected(
        instance: &IvyJson,
        settings: &IvyJson,
        result: &mut Vec<JsString>,
    ) -> IvyResult<()> {
        if let Some(paths) = instance.get("secretPaths").and_then(IvyJson::as_array) {
            let root = IvyJson::object([("settings", settings.clone())]);
            for pointer in paths {
                let mut value = Some(&root);
                for part in ivy_contracts::schema::pointer_parts(
                    pointer.as_js_string().ok_or_else(IvyError::internal)?,
                )? {
                    value = value.and_then(|v| match v {
                        IvyJson::Object(values) => values
                            .iter()
                            .find(|(key, _)| key == &part)
                            .map(|(_, value)| value),
                        _ => None,
                    });
                }
                if let Some(value) = value {
                    collect(value, result);
                }
            }
        }
        Ok(())
    }
    let mut result = Vec::new();
    for instance in host
        .required("instances")?
        .as_array()
        .ok_or_else(IvyError::internal)?
    {
        if let Some(credential) = instance.get("credential") {
            collect(credential, &mut result);
        }
        selected(instance, instance.required("settings")?, &mut result)?;
    }
    if let Some(config) = config {
        if let Some(credential) = config.json().get("credential") {
            collect(credential, &mut result);
        }
        selected(
            instance(host, &config.json().required("instanceId")?.as_str()?)?,
            config.json().required("settings")?,
            &mut result,
        )?;
    }
    Ok(result)
}
