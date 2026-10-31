//! Systemd ownership, bounded commands, definition replacement and launch units.
use crate::linux_host::FileMetadata;
use crate::{
    IvyError, IvyJson, IvyResult, bootstrap,
    linux_host::trimmed,
    linux_resources::{installation_identity, validate_linux_resource_scope},
    linux_service_identity::linux_process_user,
    runtime_owner::stop::StopPolicy,
};
use futures_util::future::BoxFuture;
use ivy_contracts::{JsString, hash::hash_json};
use std::collections::BTreeMap;
use std::{io, path::PathBuf, sync::Arc};
mod host;
pub use host::NativeSystemdHost;

pub trait UnitFile: Send {
    fn write<'a>(&'a mut self, bytes: Vec<u8>) -> BoxFuture<'a, io::Result<()>>;
    fn sync(&mut self) -> BoxFuture<'_, io::Result<()>>;
    fn close(self: Box<Self>) -> BoxFuture<'static, io::Result<()>>;
}

/// Files, commands and clocks are explicit OS boundaries. Administrative policy
/// remains in SystemdAdapter for both the native host and injected hosts.
pub trait SystemdHost: Send + Sync {
    fn platform(&self) -> &str;
    fn user_id(&self) -> Option<u32>;
    fn metadata(&self, path: JsString) -> BoxFuture<'static, io::Result<FileMetadata>>;
    fn read(&self, path: JsString) -> BoxFuture<'static, io::Result<Vec<u8>>>;
    fn open(
        &self,
        path: JsString,
        exclusive_write: bool,
    ) -> BoxFuture<'static, io::Result<Box<dyn UnitFile>>>;
    fn rename(&self, from: JsString, to: JsString) -> BoxFuture<'static, io::Result<()>>;
    fn unlink(&self, path: JsString) -> BoxFuture<'static, io::Result<()>>;
    fn execute(
        &self,
        executable: JsString,
        args: Vec<JsString>,
    ) -> BoxFuture<'static, IvyResult<JsString>>;
    fn uuid(&self) -> String;
    fn now_ms(&self) -> f64;
    fn sleep(&self, milliseconds: u64) -> BoxFuture<'static, ()>;
}
pub trait SystemdCommand: Send + Sync {
    fn run(&self, config: IvyJson, args: Vec<JsString>) -> BoxFuture<'static, IvyResult<JsString>>;
}
impl<F> SystemdCommand for F
where
    F: Fn(IvyJson, Vec<JsString>) -> BoxFuture<'static, IvyResult<JsString>> + Send + Sync,
{
    fn run(&self, config: IvyJson, args: Vec<JsString>) -> BoxFuture<'static, IvyResult<JsString>> {
        self(config, args)
    }
}

#[derive(Clone)]
pub struct SystemdAdapter {
    host: Arc<dyn SystemdHost>,
    command: Option<Arc<dyn SystemdCommand>>,
}
impl SystemdAdapter {
    pub fn new(bootstrap_root: PathBuf) -> Self {
        Self::with_host(Arc::new(NativeSystemdHost::new(bootstrap_root)))
    }
    pub fn with_host(host: Arc<dyn SystemdHost>) -> Self {
        Self {
            host,
            command: None,
        }
    }
    pub fn with_command(host: Arc<dyn SystemdHost>, command: Arc<dyn SystemdCommand>) -> Self {
        Self {
            host,
            command: Some(command),
        }
    }
    async fn command(&self, config: &IvyJson, args: Vec<JsString>) -> IvyResult<JsString> {
        if let Some(command) = &self.command {
            return command.run(config.clone(), args).await;
        }
        require(
            self.host.platform() == "linux",
            "unsupported_runtime",
            "Systemd is only available on Linux.",
        )?;
        let executables = config.required("executables")?;
        if matches!(executables, IvyJson::Null) {
            return Err(IvyError::internal());
        }
        let executable = match executables
            .get("systemctl")
            .filter(|value| !matches!(value, IvyJson::Null))
        {
            Some(value) => text(value)?,
            None => "/usr/bin/systemctl".into(),
        };
        require(
            executable.0.first() == Some(&47) && !executable.0.iter().any(|unit| *unit <= 31),
            "invalid_arguments",
            "Systemd executable must be an absolute path.",
        )?;
        self.host.execute(executable, args).await
    }
    async fn state(&self, config: &IvyJson, unit: &JsString) -> IvyResult<SystemdState> {
        require_unit(unit)?;
        let output = self
            .command(
                config,
                vec![
                    "show".into(),
                    "--no-pager".into(),
                    "--property=LoadState,ActiveState,SubState,MainPID".into(),
                    unit.clone(),
                ],
            )
            .await?;
        let state = parse_systemd_state(&output);
        require(
            state.load_state == JsString::from("loaded")
                || state.load_state == JsString::from("not-found"),
            "target_conflict",
            "Systemd reported an unexpected unit load state.",
        )?;
        Ok(state)
    }
    pub async fn inspect_unit(&self, config: IvyJson, unit: JsString) -> IvyResult<SystemdState> {
        let adapter = self.clone();
        tokio::spawn(async move { adapter.state(&config, &unit).await })
            .await
            .map_err(|_| IvyError::internal())?
    }
    pub async fn read_unit(&self, _config: IvyJson, unit: JsString) -> IvyResult<Option<JsString>> {
        let adapter = self.clone();
        tokio::spawn(async move {
            require_unit(&unit)?;
            let path = definition_path(&unit);
            let metadata = match adapter.host.metadata(path.clone()).await {
                Ok(value) => value,
                Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
                Err(_) => return Err(IvyError::internal()),
            };
            require(
                metadata.is_file
                    && !metadata.is_symlink
                    && metadata.uid == Some(0)
                    && metadata.mode & 0o022 == 0
                    && metadata.size <= 65_536,
                "target_conflict",
                "Systemd unit must be a bounded administrator-owned regular file.",
            )?;
            match adapter.host.read(path).await {
                Ok(bytes) => Ok(Some(JsString(
                    crate::process_output::Utf8Decoder::preserving_bom().decode(&bytes, true),
                ))),
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
                Err(_) => Err(IvyError::internal()),
            }
        })
        .await
        .map_err(|_| IvyError::internal())?
    }
    pub async fn stop_unit(
        &self,
        config: IvyJson,
        unit: JsString,
        policy: StopPolicy,
    ) -> IvyResult<SystemdState> {
        let adapter = self.clone();
        tokio::spawn(async move {
            require(
                policy
                    .timeout()
                    .is_none_or(|timeout| timeout <= std::time::Duration::from_secs(600)),
                "invalid_arguments",
                "Systemd stop timeout is invalid.",
            )?;
            require_unit(&unit)?;
            let current = adapter.state(&config, &unit).await?;
            if stopped(&current) {
                return Ok(current);
            }
            let deadline = policy
                .timeout()
                .map(|timeout| adapter.host.now_ms() + timeout.as_millis() as f64);
            adapter
                .command(
                    &config,
                    vec!["stop".into(), "--no-block".into(), unit.clone()],
                )
                .await?;
            let mut observed = adapter.state(&config, &unit).await?;
            while !stopped(&observed) && deadline.is_none_or(|end| adapter.host.now_ms() < end) {
                adapter
                    .host
                    .sleep(if deadline.is_none() { 1000 } else { 100 })
                    .await;
                observed = adapter.state(&config, &unit).await?;
            }
            if !stopped(&observed) {
                return Err(IvyError::new(
                    "outcome_unknown",
                    "Systemd did not establish a stopped unit outcome.",
                )
                .with_outcome(ivy_contracts::Outcome::Unknown));
            }
            Ok(observed)
        })
        .await
        .map_err(|_| IvyError::internal())?
    }
    pub async fn replace_unit(
        &self,
        _config: IvyJson,
        unit: JsString,
        content: JsString,
    ) -> IvyResult<()> {
        let adapter = self.clone();
        tokio::spawn(async move {
            require_unit(&unit)?;
            let bytes = String::from_utf16_lossy(&content.0).into_bytes();
            require(
                !content.0.is_empty() && bytes.len() <= 65_536 && !content.0.contains(&0),
                "invalid_arguments",
                "Systemd unit content is invalid or unbounded.",
            )?;
            require(
                adapter.host.platform() == "linux" && adapter.host.user_id() == Some(0),
                "unsupported_runtime",
                "Systemd unit replacement needs the systemd administrator.",
            )?;
            let path = definition_path(&unit);
            let mut temporary = path.0.clone();
            temporary.extend(format!(".{}.tmp", adapter.host.uuid()).encode_utf16());
            let temporary = JsString(temporary);
            let mut file = adapter
                .host
                .open(temporary.clone(), true)
                .await
                .map_err(|_| IvyError::internal())?;
            let written = async {
                file.write(bytes).await?;
                file.sync().await
            }
            .await;
            // Node's finally-close replaces an earlier write/sync error. The
            // original leaves this temporary file on those early failures.
            file.close().await.map_err(|_| IvyError::internal())?;
            written.map_err(|_| IvyError::internal())?;
            let installed: io::Result<()> = async {
                adapter.host.rename(temporary.clone(), path).await?;
                let mut directory = adapter
                    .host
                    .open("/etc/systemd/system".into(), false)
                    .await?;
                let synced = directory.sync().await;
                directory.close().await?;
                synced
            }
            .await;
            if installed.is_err() {
                let _ = adapter.host.unlink(temporary).await;
                return Err(IvyError::internal());
            }
            Ok(())
        })
        .await
        .map_err(|_| IvyError::internal())?
    }
    pub async fn reload(&self, config: IvyJson) -> IvyResult<()> {
        let adapter = self.clone();
        tokio::spawn(async move {
            adapter
                .command(&config, vec!["daemon-reload".into()])
                .await?;
            Ok(())
        })
        .await
        .map_err(|_| IvyError::internal())?
    }
    pub async fn start_unit(&self, config: IvyJson, unit: JsString) -> IvyResult<SystemdState> {
        let adapter = self.clone();
        tokio::spawn(async move {
            require_unit(&unit)?;
            adapter
                .command(&config, vec!["start".into(), unit.clone()])
                .await?;
            adapter.state(&config, &unit).await
        })
        .await
        .map_err(|_| IvyError::internal())?
    }
}
fn stopped(state: &SystemdState) -> bool {
    state.active_state == JsString::from("inactive")
        || state.active_state == JsString::from("failed")
}
fn require_unit(unit: &JsString) -> IvyResult<()> {
    let value = &unit.0;
    require(
        value.len() == 42
            && value.starts_with(&JsString::from("ivy-next-").0)
            && value[21] == 45
            && value.ends_with(&JsString::from(".service").0)
            && value[9..21]
                .iter()
                .chain(&value[22..34])
                .all(|unit| matches!(*unit, 48..=57 | 97..=102)),
        "target_conflict",
        "Systemd unit is outside the Ivy installation.",
    )
}
fn definition_path(unit: &JsString) -> JsString {
    let mut path = JsString::from("/etc/systemd/system/").0;
    path.extend_from_slice(&unit.0);
    JsString(path)
}
fn require(ok: bool, code: &str, message: &str) -> IvyResult<()> {
    if ok {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn text(value: &IvyJson) -> IvyResult<JsString> {
    value.as_js_string().cloned().ok_or_else(IvyError::internal)
}
fn instance<'a>(config: &'a IvyJson, id: &JsString) -> IvyResult<Option<&'a IvyJson>> {
    Ok(config
        .required("instances")?
        .as_array()
        .ok_or_else(IvyError::internal)?
        .iter()
        .find(|instance| instance.get("instanceId") == Some(&IvyJson::String(id.clone()))))
}
pub fn systemd_unit_name(config: &IvyJson, id: &JsString) -> IvyResult<String> {
    require(
        instance(config, id)?.is_some_and(|value| value.get("engine") == Some(&"process".into())),
        "unsupported_runtime",
        "Only process instances have systemd units.",
    )?;
    Ok(format!(
        "{}-{}.service",
        installation_identity(config)?,
        &hash_json(&IvyJson::String(id.clone()))?[7..19]
    ))
}
#[derive(Clone, Debug, PartialEq)]
pub struct SystemdState {
    pub load_state: JsString,
    pub active_state: JsString,
    pub sub_state: JsString,
    pub main_pid: Option<u64>,
}
impl SystemdState {
    pub fn json(&self) -> IvyJson {
        IvyJson::object([
            ("loadState", IvyJson::String(self.load_state.clone())),
            ("activeState", IvyJson::String(self.active_state.clone())),
            ("subState", IvyJson::String(self.sub_state.clone())),
            (
                "mainPid",
                self.main_pid
                    .map(|pid| IvyJson::Number(pid as f64))
                    .unwrap_or(IvyJson::Null),
            ),
        ])
    }
}
pub fn parse_systemd_state(output: &JsString) -> SystemdState {
    let output = trimmed(output);
    let mut properties = BTreeMap::new();
    for line in output.0.split(|unit| *unit == 10) {
        let line = line.strip_suffix(&[13]).unwrap_or(line);
        if let Some(index) = line
            .iter()
            .position(|unit| *unit == 61)
            .filter(|index| *index > 0)
        {
            properties.insert(
                JsString(line[..index].to_vec()),
                JsString(line[index + 1..].to_vec()),
            );
        }
    }
    let pid = properties
        .get(&JsString::from("MainPID"))
        .and_then(|value| value.to_utf8().ok())
        .and_then(|value| value.parse::<u32>().ok())
        .filter(|pid| *pid > 0);
    let get = |key: &str| {
        properties
            .get(&JsString::from(key))
            .cloned()
            .unwrap_or_else(|| "unknown".into())
    };
    SystemdState {
        load_state: get("LoadState"),
        active_state: get("ActiveState"),
        sub_state: get("SubState"),
        main_pid: pid.map(u64::from),
    }
}
#[derive(Clone)]
pub struct RenderedSystemdUnit {
    pub unit: String,
    pub content: JsString,
}
pub fn render_systemd_unit(
    config: &IvyJson,
    id: &JsString,
    target: &IvyJson,
    candidate: &IvyJson,
    manifest: &IvyJson,
    resources: Option<&IvyJson>,
) -> IvyResult<RenderedSystemdUnit> {
    require(
        cfg!(target_os = "linux")
            || candidate
                .get("platform")
                .and_then(|platform| platform.get("os"))
                == Some(&"linux".into()),
        "unsupported_runtime",
        "Systemd units require a Linux candidate.",
    )?;
    let instance = instance(config, id)?;
    require(
        instance.is_some_and(|instance| instance.get("engine") == Some(&"process".into())),
        "unsupported_runtime",
        "Only process instances can render systemd units.",
    )?;
    let instance = instance.unwrap();
    require(
        candidate
            .get("platform")
            .and_then(|platform| platform.get("os"))
            == Some(&"linux".into())
            && candidate.get("componentId") == instance.get("componentId")
            && target.get("instanceId") == Some(&IvyJson::String(id.clone()))
            && target.get("candidateId") == candidate.get("candidateId"),
        "target_conflict",
        "Systemd unit identity does not match the accepted target.",
    )?;
    require(
        manifest.get("componentId") == instance.get("componentId")
            && manifest.get("entrypoint").is_some_and(|entry| {
                entry
                    .get("executable")
                    .and_then(IvyJson::as_js_string)
                    .is_some_and(|executable| !executable.0.is_empty())
                    && entry.get("args").and_then(IvyJson::as_array).is_some()
            }),
        "target_conflict",
        "Systemd unit has no matching component entrypoint.",
    )?;
    let installation = installation_identity(config)?;
    if let Some(resources) = resources {
        validate_linux_resource_scope(resources, Some(&installation))?;
    }
    let name = systemd_unit_name(config, id)?;
    let mut process = IvyJson::object([
        ("instanceId", IvyJson::String(id.clone())),
        ("componentId", instance.required("componentId")?.clone()),
        ("name", name.strip_suffix(".service").unwrap().into()),
        ("candidateId", candidate.required("candidateId")?.clone()),
        ("artifactRoot", candidate.required("artifactRoot")?.clone()),
        ("entrypoint", manifest.required("entrypoint")?.clone()),
        ("configPath", target.required("configPath")?.clone()),
    ]);
    if let Some(user) = linux_process_user(&text(instance.required("componentId")?)?, "linux") {
        process.insert("user", user.into());
    }
    if let Some(restart) = manifest.get("restart") {
        require(
            restart.as_object().is_some(),
            "invalid_arguments",
            "Systemd restart must be an object.",
        )?;
        process.insert("restart", restart.clone());
    }
    let mut plan = IvyJson::object([
        ("schemaVersion", IvyJson::Number(1.0)),
        ("os", "linux".into()),
        ("hostId", config.required("hostId")?.clone()),
        ("installationId", installation.into()),
        ("candidateId", candidate.required("candidateId")?.clone()),
        ("artifactRoot", candidate.required("artifactRoot")?.clone()),
        ("configPath", target.required("configPath")?.clone()),
        ("configHash", hash_json(config)?.into()),
        ("runtimeRoot", config.required("runtimeRoot")?.clone()),
        ("processes", IvyJson::Array(vec![process.clone()])),
    ]);
    if let Some(node) = config.required("executables")?.get("node") {
        plan.insert("nodeExecutable", node.clone());
    }
    if let Some(resources) = resources {
        plan.insert("linuxResources", resources.clone());
    }
    let launch = Some(text(target.required("revision")?)?);
    let policy = StopPolicy::for_manifest(manifest);
    let content = if policy == StopPolicy::Cooperative {
        bootstrap::linux_unit_with_stop_policy(&plan, &process, launch, policy)?
    } else {
        bootstrap::linux_unit(&plan, &process, launch)?
    };
    Ok(RenderedSystemdUnit {
        unit: name,
        content,
    })
}
