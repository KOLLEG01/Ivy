//! Exact Docker launch ownership and passive observation, using the shared process owner.
use crate::process::ProcessResult;
use crate::{IvyError, IvyJson, IvyResult, linux_host::trimmed, linux_resources, node_path};
use futures_util::future::BoxFuture;
use ivy_contracts::{JsString, core_schema::validate_host, hash::hash_json, json};
use std::{path::PathBuf, sync::Arc};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
mod host;
pub use host::NativeDockerHost;

/// Files, paths, process execution, resource membership and observation delay are
/// explicit OS boundaries. Container policy remains in the adapter.
pub trait DockerHost: Send + Sync {
    fn platform(&self) -> &str;
    fn paths(&self) -> &node_path::Context;
    fn mkdir(&self, path: JsString) -> BoxFuture<'static, IvyResult<()>>;
    fn configuration(&self, path: JsString) -> BoxFuture<'static, IvyResult<IvyJson>>;
    fn verify_resources(
        &self,
        config: IvyJson,
        instance: JsString,
        scope: IvyJson,
    ) -> BoxFuture<'static, IvyResult<()>>;
    fn execute(
        &self,
        config: IvyJson,
        args: Vec<JsString>,
        timeout_ms: f64,
        redact: Vec<JsString>,
    ) -> BoxFuture<'static, IvyResult<ProcessResult>>;
    fn sleep(&self) -> BoxFuture<'static, ()>;
}
pub trait DockerCommand: Send + Sync {
    fn run(
        &self,
        config: IvyJson,
        args: Vec<JsString>,
        timeout_ms: f64,
        extra_secrets: Vec<JsString>,
    ) -> BoxFuture<'static, IvyResult<ProcessResult>>;
}
impl<F> DockerCommand for F
where
    F: Fn(
            IvyJson,
            Vec<JsString>,
            f64,
            Vec<JsString>,
        ) -> BoxFuture<'static, IvyResult<ProcessResult>>
        + Send
        + Sync,
{
    fn run(
        &self,
        config: IvyJson,
        args: Vec<JsString>,
        timeout_ms: f64,
        extra_secrets: Vec<JsString>,
    ) -> BoxFuture<'static, IvyResult<ProcessResult>> {
        self(config, args, timeout_ms, extra_secrets)
    }
}

#[derive(Clone)]
pub struct DockerAdapter {
    host: Arc<dyn DockerHost>,
    command: Option<Arc<dyn DockerCommand>>,
}
impl DockerAdapter {
    pub fn new(bootstrap_root: PathBuf) -> IvyResult<Self> {
        Ok(Self::with_host(Arc::new(NativeDockerHost::new(
            bootstrap_root,
        )?)))
    }
    pub fn with_host(host: Arc<dyn DockerHost>) -> Self {
        Self {
            host,
            command: None,
        }
    }
    pub fn with_command(host: Arc<dyn DockerHost>, command: Arc<dyn DockerCommand>) -> Self {
        Self {
            host,
            command: Some(command),
        }
    }
    async fn command(
        &self,
        host: &IvyJson,
        args: Vec<JsString>,
        timeout_ms: f64,
        mut extra_secrets: Vec<JsString>,
    ) -> IvyResult<ProcessResult> {
        if let Some(command) = &self.command {
            return command
                .run(host.clone(), args, timeout_ms, extra_secrets)
                .await;
        }
        require(
            self.host.platform() == "linux"
                && host
                    .get("executables")
                    .and_then(|v| v.get("docker"))
                    .and_then(IvyJson::as_js_string)
                    .is_some_and(|path| !path.0.is_empty()),
            "unsupported_runtime",
            "Docker ownership requires the configured Linux host daemon.",
        )?;
        for instance in array(host.required("instances")?)? {
            extra_secrets.push(
                instance
                    .get("credential")
                    .filter(|v| !matches!(v, IvyJson::Null))
                    .map(text)
                    .transpose()?
                    .unwrap_or_default(),
            );
            if instance.get("componentId") == Some(&"hive".into()) {
                for credential in array(instance.required("settings")?.required("credentials")?)? {
                    extra_secrets.push(text(credential.required("token")?)?);
                }
            }
        }
        let mut command = vec!["--host".into(), "unix:///var/run/docker.sock".into()];
        command.extend(args);
        self.host
            .execute(host.clone(), command, timeout_ms, extra_secrets)
            .await
    }
    async fn inspect(&self, host: &IvyJson, instance: &JsString) -> IvyResult<Option<IvyJson>> {
        let name = owned_container_name(host, instance)?;
        let listed = self
            .command(
                host,
                vec![
                    "container".into(),
                    "ls".into(),
                    "--all".into(),
                    "--no-trunc".into(),
                    "--filter".into(),
                    concat(&["name=^/".into(), name.clone(), "$".into()]),
                    "--format".into(),
                    "{{.ID}}".into(),
                ],
                10000.0,
                vec![],
            )
            .await?;
        let listed = trimmed(&listed.stdout.into());
        let ids: Vec<_> = listed
            .0
            .split(|c| *c == 10)
            .map(|v| v.strip_suffix(&[13]).unwrap_or(v))
            .filter(|v| !v.is_empty())
            .collect();
        if ids.is_empty() {
            return Ok(None);
        }
        require(
            ids.len() == 1
                && ids[0].len() == 64
                && ids[0].iter().all(|c| matches!(*c,48..=57|97..=102)),
            "outcome_unknown",
            "Owned Docker name has an ambiguous identity.",
        )?;
        let id = JsString(ids[0].to_vec());
        let inspected = self
            .command(
                host,
                vec![
                    "container".into(),
                    "inspect".into(),
                    "--format".into(),
                    "{{json .}}".into(),
                    id.clone(),
                ],
                10000.0,
                vec![],
            )
            .await?;
        let raw = json::parse(&inspected.stdout, usize::MAX).map_err(|_| IvyError::internal())?;
        if matches!(raw, IvyJson::Null) {
            return Err(IvyError::internal());
        }
        let labels = raw.get("Config").and_then(|v| v.get("Labels"));
        require(
            labels.is_some_and(|labels| {
                labels.as_object().is_some()
                    && labels.get("dev.ivy.host") == host.get("hostId")
                    && labels.get("dev.ivy.instance") == Some(&IvyJson::String(instance.clone()))
            }) && raw.get("Name") == Some(&IvyJson::String(concat(&["/".into(), name.clone()]))),
            "target_conflict",
            "Existing Docker container is not owned by this Ivy instance.",
        )?;
        let labels = labels.unwrap();
        let state = raw.get("State").ok_or_else(IvyError::internal)?;
        if matches!(state, IvyJson::Null) {
            return Err(IvyError::internal());
        }
        let mut value = IvyJson::object([
            ("name", IvyJson::String(name)),
            ("hostId", host.required("hostId")?.clone()),
            ("instanceId", IvyJson::String(instance.clone())),
        ]);
        for (key, source, source_key) in [
            ("containerId", &raw, "Id"),
            ("imageId", &raw, "Image"),
            ("candidateId", labels, "dev.ivy.candidate"),
            ("launchId", labels, "dev.ivy.launch"),
            ("state", state, "Status"),
            ("exitCode", state, "ExitCode"),
            ("startedAt", state, "StartedAt"),
        ] {
            // Require every observed field before admitting the container state.
            value.insert(
                key,
                source.get(source_key).cloned().ok_or_else(outside_json)?,
            );
        }
        value.insert(
            "cgroupParent",
            raw.get("HostConfig")
                .and_then(|v| v.get("CgroupParent"))
                .cloned()
                .ok_or_else(outside_json)?,
        );
        validate_host("ContainerState", &value)?;
        require(
            value.get("containerId") == Some(&IvyJson::String(id)),
            "target_conflict",
            "Docker identity changed during inspection.",
        )?;
        Ok(Some(value))
    }
    pub async fn inspect_container(
        &self,
        host: IvyJson,
        instance: JsString,
    ) -> IvyResult<Option<IvyJson>> {
        let adapter = self.clone();
        owned(async move { adapter.inspect(&host, &instance).await }).await
    }
    async fn stopped(
        &self,
        host: &IvyJson,
        instance: &JsString,
        grace_ms: f64,
        expected: Option<&JsString>,
    ) -> IvyResult<Option<IvyJson>> {
        let Some(value) = self.inspect(host, instance).await? else {
            return Ok(None);
        };
        require(
            expected.is_none_or(|id| {
                id.0.is_empty() || value.get("containerId") == Some(&IvyJson::String(id.clone()))
            }),
            "outcome_unknown",
            "The container to stop no longer has its observed identity.",
        )?;
        if states(&value, &["running", "restarting"]) {
            let failed = self
                .command(
                    host,
                    vec![
                        "container".into(),
                        "stop".into(),
                        "-t".into(),
                        (grace_ms / 1000.0).ceil().to_string().into(),
                        field(&value, "containerId")?,
                    ],
                    grace_ms + 15000.0,
                    vec![],
                )
                .await
                .is_err();
            let observed = self.inspect(host, instance).await?;
            require(
                observed.as_ref().is_some_and(|v| {
                    v.get("containerId") == value.get("containerId")
                        && states(v, &["exited", "dead"])
                }),
                "outcome_unknown",
                if failed {
                    "Docker stop result remains unknown after re-inspection."
                } else {
                    "Docker did not establish the expected stopped container."
                },
            )?;
            return Ok(observed);
        }
        require(
            states(&value, &["created", "exited", "dead"]),
            "outcome_unknown",
            "Paused/removing container needs explicit owner reconciliation.",
        )?;
        Ok(Some(value))
    }
    pub async fn stop_container(
        &self,
        host: IvyJson,
        instance: JsString,
        grace_ms: f64,
        expected: Option<JsString>,
    ) -> IvyResult<Option<IvyJson>> {
        let adapter = self.clone();
        owned(async move {
            adapter
                .stopped(&host, &instance, grace_ms, expected.as_ref())
                .await
        })
        .await
    }
    pub async fn verify_container_image(
        &self,
        host: IvyJson,
        instance: IvyJson,
        candidate: IvyJson,
        manifest: IvyJson,
    ) -> IvyResult<()> {
        let adapter = self.clone();
        owned(async move {
            require(
                instance.get("componentId") == Some(&"hive".into())
                    && instance.get("docker").and_then(IvyJson::as_object).is_some()
                    && candidate.get("dockerImage").and_then(IvyJson::as_js_string)
                        .is_some_and(|image| !image.0.is_empty())
                    && manifest.get("entrypoint").and_then(|v| v.get("executable"))
                        == Some(&"node".into()),
                "unsupported_runtime",
                "Docker activation requires a prepared Hive image with the pinned Node entry runtime.",
            )?;
            let docker = instance.required("docker")?;
            require(
                repository(&field(docker, "imageRepository")?),
                "invalid_arguments",
                "Hive image repository must be an explicit local repository name without a tag.",
            )?;
            for port in array(docker.required("ports")?)? {
                require(
                    ip(&field(port, "host")?) != 0,
                    "invalid_arguments",
                    "Docker port bindings require explicit IP addresses.",
                )?;
            }
            let image = adapter.command(
                &host,
                vec!["image".into(), "inspect".into(), "--format".into(), "{{.Id}}".into(), field(&candidate, "dockerImage")?],
                10000.0,
                vec![],
            ).await?;
            require(
                trimmed(&image.stdout.into()) == field(&candidate, "dockerImage")?,
                "build_mismatch",
                "Prepared immutable Docker image is unavailable.",
            )
        })
        .await
    }
    pub async fn start_container(
        &self,
        host: IvyJson,
        instance: IvyJson,
        target: IvyJson,
        config: IvyJson,
        candidate: IvyJson,
        manifest: IvyJson,
        allow_restart: bool,
        resources: Option<IvyJson>,
    ) -> IvyResult<ManagedContainer> {
        let adapter = self.clone();
        owned(async move {
            adapter
                .start(
                    host,
                    instance,
                    target,
                    config,
                    candidate,
                    manifest,
                    allow_restart,
                    resources,
                )
                .await
        })
        .await
    }
    async fn start(
        &self,
        host: IvyJson,
        instance: IvyJson,
        target: IvyJson,
        config: IvyJson,
        candidate: IvyJson,
        manifest: IvyJson,
        allow_restart: bool,
        resources: Option<IvyJson>,
    ) -> IvyResult<ManagedContainer> {
        require(
            instance.get("componentId") == Some(&"hive".into())
                && instance
                    .get("docker")
                    .and_then(IvyJson::as_object)
                    .is_some()
                && candidate
                    .get("dockerImage")
                    .and_then(IvyJson::as_js_string)
                    .is_some_and(|image| !image.0.is_empty()),
            "unsupported_runtime",
            "The Docker driver requires a prepared Hive image and explicit bindings.",
        )?;
        require(
            manifest.get("entrypoint").and_then(|v| v.get("executable")) == Some(&"node".into()),
            "unsupported_runtime",
            "This Hive image provides the pinned Node entry runtime.",
        )?;
        let settings = config.required("settings")?;
        validate_host("HiveSettings", settings)?;
        let work = optional_root(&config, "workRoot")?;
        let logs = optional_root(&config, "logsRoot")?;
        let restart_policy = docker_restart_policy(&manifest)?;
        let instance_id = field(&instance, "instanceId")?;
        if let Some(resources) = &resources {
            linux_resources::validate_linux_resource_scope(
                resources,
                Some(&linux_resources::installation_identity(&host)?),
            )?;
            self.host
                .verify_resources(host.clone(), instance_id.clone(), resources.clone())
                .await?;
            let driver = self
                .command(
                    &host,
                    vec![
                        "info".into(),
                        "--format".into(),
                        "{{.CgroupDriver}} {{.CgroupVersion}}".into(),
                    ],
                    10000.0,
                    vec![],
                )
                .await?;
            require(
                trimmed(&driver.stdout.into()) == JsString::from("systemd 2"),
                "unsupported_runtime",
                "Shared Linux resources require Docker systemd cgroup v2.",
            )?;
        }
        let launch = Launch {
            revision: target.required("revision")?.clone(),
            candidate: candidate.required("candidateId")?.clone(),
            image: candidate.required("dockerImage")?.clone(),
            slice: resources
                .as_ref()
                .map(|v| v.required("slice").cloned())
                .transpose()?,
        };
        let mut current = self.inspect(&host, &instance_id).await?;
        if let Some(value) = &current {
            if !launch.matches(value) {
                require(
                    states(value, &["created", "exited", "dead"]),
                    "outcome_unknown",
                    "A different live launch must be explicitly stopped before replacement.",
                )?;
                self.command(
                    &host,
                    vec![
                        "container".into(),
                        "rm".into(),
                        field(value, "containerId")?,
                    ],
                    10000.0,
                    vec![],
                )
                .await?;
                current = None;
            }
        }
        if current.is_none() {
            let mut mounts = vec![];
            let paths = self.host.paths();
            let data = field(&config, "dataRoot")?;
            let backup = field(settings.required("backup")?, "directory")?;
            self.host.mkdir(data.clone()).await?;
            self.host.mkdir(backup.clone()).await?;
            for (path, writable) in [
                (data, true),
                (backup, true),
                (field(&config, "artifactRoot")?, false),
                (
                    node_path::dirname(paths.style, &field(&target, "configPath")?),
                    false,
                ),
            ] {
                mount(paths.style, path, writable, &mut mounts)?;
            }
            for path in work.iter().chain(logs.iter()) {
                self.host.mkdir(path.clone()).await?;
                mount(paths.style, path.clone(), true, &mut mounts)?;
            }
            let docker = instance.required("docker")?;
            let mut ports = vec![];
            for port in array(docker.required("ports")?)? {
                let address = field(port, "host")?;
                let kind = ip(&address);
                require(
                    kind != 0,
                    "invalid_arguments",
                    "Docker port bindings require an explicit IP address.",
                )?;
                ports.push("--publish".into());
                ports.push(concat(&[
                    if kind == 6 {
                        concat(&["[".into(), address, "]".into()])
                    } else {
                        address
                    },
                    ":".into(),
                    port.required("port")?
                        .as_f64()
                        .ok_or_else(IvyError::internal)?
                        .to_string()
                        .into(),
                    ":".into(),
                    port.required("containerPort")?
                        .as_f64()
                        .ok_or_else(IvyError::internal)?
                        .to_string()
                        .into(),
                ]));
            }
            let image = field(&candidate, "dockerImage")?;
            let build = field(&candidate, "buildId")?;
            self.command(
                &host,
                vec![
                    "image".into(),
                    "tag".into(),
                    image.clone(),
                    concat(&[
                        field(docker, "imageRepository")?,
                        ":".into(),
                        JsString(build.0.get(7..).unwrap_or_default().to_vec()),
                    ]),
                ],
                10000.0,
                vec![],
            )
            .await?;
            let mut args: Vec<JsString> = vec!["container", "create", "--name"]
                .into_iter()
                .map(Into::into)
                .collect();
            args.push(owned_container_name(&host, &instance_id)?);
            args.extend(["--pull", "never", "--restart"].map(Into::into));
            args.push(restart_policy);
            args.extend(
                [
                    "--read-only",
                    "--cap-drop",
                    "ALL",
                    "--security-opt",
                    "no-new-privileges=true",
                    "--pids-limit",
                    "128",
                    "--memory",
                    "768m",
                    "--memory-swap",
                    "768m",
                    "--tmpfs",
                    "/tmp:rw,nosuid,nodev,size=64m",
                    "--log-opt",
                    "max-size=1m",
                    "--log-opt",
                    "max-file=3",
                ]
                .map(Into::into),
            );
            if let Some(slice) = &launch.slice {
                args.extend(["--cgroup-parent".into(), text(slice)?]);
            }
            for (key, value) in [
                ("dev.ivy.host", host.required("hostId")?),
                ("dev.ivy.instance", instance.required("instanceId")?),
                ("dev.ivy.candidate", candidate.required("candidateId")?),
                ("dev.ivy.launch", target.required("revision")?),
            ] {
                args.extend([
                    "--label".into(),
                    concat(&[key.into(), "=".into(), text(value)?]),
                ]);
            }
            args.extend(mounts);
            args.extend(ports);
            let entry = manifest.required("entrypoint")?;
            let entry_args: Vec<_> = array(entry.required("args")?)?
                .iter()
                .map(text)
                .collect::<IvyResult<_>>()?;
            let cwd = if let Some(work) = &work {
                work.clone()
            } else {
                paths.resolve(&[
                    field(&candidate, "artifactRoot")?,
                    entry
                        .get("cwd")
                        .filter(|v| !matches!(v, IvyJson::Null))
                        .map(text)
                        .transpose()?
                        .unwrap_or_else(|| ".".into()),
                ])
            };
            args.extend([
                "--env".into(),
                concat(&["IVY_INSTANCE_CONFIG=".into(), field(&target, "configPath")?]),
                "--env".into(),
                concat(&["IVY_LAUNCH_ID=".into(), field(&target, "revision")?]),
                "--workdir".into(),
                cwd,
                "--entrypoint".into(),
                "/usr/local/bin/node".into(),
                image,
            ]);
            if work.is_some()
                && entry_args
                    .first()
                    .is_some_and(|v| v.0.starts_with(&JsString::from("dist/").0))
            {
                args.push(
                    paths.resolve(&[field(&candidate, "artifactRoot")?, entry_args[0].clone()]),
                );
                args.extend(entry_args.into_iter().skip(1));
            } else {
                args.extend(entry_args);
            }
            let failed = self.command(&host, args, 30000.0, vec![]).await.is_err();
            current = self.inspect(&host, &instance_id).await?;
            require(
                current.as_ref().is_some_and(|v| launch.matches(v)),
                "outcome_unknown",
                if failed {
                    "Container creation needs reconciliation; no second identity was created."
                } else {
                    "Created container does not match the accepted target."
                },
            )?;
        }
        let mut current = current.unwrap();
        if states(&current, &["created"]) || allow_restart && states(&current, &["exited", "dead"])
        {
            let failed = self
                .command(
                    &host,
                    vec![
                        "container".into(),
                        "start".into(),
                        field(&current, "containerId")?,
                    ],
                    30000.0,
                    vec![],
                )
                .await
                .is_err();
            let observed = self.inspect(&host, &instance_id).await?;
            require(
                observed.as_ref().is_some_and(|v| {
                    launch.matches(v)
                        && v.get("containerId") == current.get("containerId")
                        && states(v, &["running", "exited", "dead"])
                        && field(v, "startedAt")
                            .is_ok_and(|v| !v.0.starts_with(&JsString::from("0001-").0))
                }),
                "outcome_unknown",
                if failed {
                    "Docker start needs reconciliation against its existing identity."
                } else {
                    "Docker start was not observed."
                },
            )?;
            current = observed.unwrap();
        }
        let identity = field(&current, "containerId")?;
        let config_path = field(&target, "configPath")?;
        let cancellation = CancellationToken::new();
        let (sender, receiver) = watch::channel(None);
        let managed = ManagedContainer {
            adapter: self.clone(),
            host,
            instance: instance_id,
            identity,
            launch,
            config_path,
            cancellation,
            completion: ContainerCompletion(receiver),
        };
        let observer = managed.clone();
        tokio::spawn(async move {
            let value = observer.observe().await;
            let _ = sender.send(Some(value));
        });
        Ok(managed)
    }
    async fn result_of(
        &self,
        host: &IvyJson,
        value: &IvyJson,
        path: &JsString,
    ) -> IvyResult<ProcessResult> {
        let config = self.host.configuration(path.clone()).await.ok();
        let mut secrets = vec![];
        if let Some(credentials) = config
            .as_ref()
            .and_then(|v| v.get("settings"))
            .and_then(|v| v.get("credentials"))
            .filter(|v| !matches!(v, IvyJson::Null))
        {
            for credential in array(credentials)? {
                secrets.push(
                    credential
                        .get("token")
                        .map(text)
                        .transpose()?
                        .unwrap_or_default(),
                );
            }
        }
        let logs = self
            .command(
                host,
                vec![
                    "container".into(),
                    "logs".into(),
                    "--tail".into(),
                    "100".into(),
                    field(value, "containerId")?,
                ],
                5000.0,
                secrets,
            )
            .await
            .ok();
        Ok(ProcessResult {
            exit_code: Some(
                value
                    .required("exitCode")?
                    .as_f64()
                    .ok_or_else(IvyError::internal)? as i32,
            ),
            signal: None,
            error_code: None,
            stdout: logs.as_ref().map(|v| v.stdout.clone()).unwrap_or_default(),
            stderr: logs.as_ref().map(|v| v.stderr.clone()).unwrap_or_default(),
            truncated: logs.as_ref().is_some_and(|v| v.truncated),
        })
    }
}

#[derive(Clone)]
struct Launch {
    revision: IvyJson,
    candidate: IvyJson,
    image: IvyJson,
    slice: Option<IvyJson>,
}
impl Launch {
    fn matches(&self, value: &IvyJson) -> bool {
        value.get("launchId") == Some(&self.revision)
            && value.get("candidateId") == Some(&self.candidate)
            && value.get("imageId") == Some(&self.image)
            && self
                .slice
                .as_ref()
                .is_none_or(|slice| value.get("cgroupParent") == Some(slice))
    }
}
#[derive(Clone)]
pub struct ContainerCompletion(watch::Receiver<Option<IvyResult<ProcessResult>>>);
impl ContainerCompletion {
    pub async fn wait(&self) -> IvyResult<ProcessResult> {
        let mut receiver = self.0.clone();
        loop {
            if let Some(value) = receiver.borrow_and_update().clone() {
                return value;
            }
            if receiver.changed().await.is_err() {
                return Ok(unknown());
            }
        }
    }
}
#[derive(Clone)]
pub struct ManagedContainer {
    pub completion: ContainerCompletion,
    adapter: DockerAdapter,
    host: IvyJson,
    instance: JsString,
    identity: JsString,
    launch: Launch,
    config_path: JsString,
    cancellation: CancellationToken,
}
impl ManagedContainer {
    /// Cancel observation only. Container state and restart policy stay owned by Docker.
    pub fn dispose(&self) {
        self.cancellation.cancel();
    }
    pub async fn verify(&self) -> IvyResult<()> {
        let owner = self.clone();
        owned(async move {
            let observed = owner.adapter.inspect(&owner.host, &owner.instance).await?;
            require(
                observed.as_ref().is_some_and(|v| {
                    v.get("containerId") == Some(&IvyJson::String(owner.identity.clone()))
                        && owner.launch.matches(v)
                        && states(v, &["running"])
                }),
                "service_not_ready",
                "Exact Docker launch is not observed running.",
            )
        })
        .await
    }
    pub async fn stop(&self, grace_ms: f64) -> IvyResult<ProcessResult> {
        let owner = self.clone();
        owned(async move {
            let stopped = owner
                .adapter
                .stopped(
                    &owner.host,
                    &owner.instance,
                    grace_ms,
                    Some(&owner.identity),
                )
                .await?;
            match stopped {
                Some(value) => {
                    owner
                        .adapter
                        .result_of(&owner.host, &value, &owner.config_path)
                        .await
                }
                None => Ok(unknown()),
            }
        })
        .await
    }
    async fn observe(&self) -> IvyResult<ProcessResult> {
        while !self.cancellation.is_cancelled() {
            if let Ok(observed) = self.adapter.inspect(&self.host, &self.instance).await {
                let Some(observed) = observed else {
                    return Ok(unknown());
                };
                if observed.get("containerId") != Some(&IvyJson::String(self.identity.clone()))
                    || !self.launch.matches(&observed)
                {
                    return Ok(unknown());
                }
                if states(&observed, &["created", "exited", "dead"]) {
                    return self
                        .adapter
                        .result_of(&self.host, &observed, &self.config_path)
                        .await;
                }
            }
            tokio::select! {biased;_ = self.cancellation.cancelled()=>break,_ = self.adapter.host.sleep()=>{}}
        }
        Ok(unknown())
    }
}
fn unknown() -> ProcessResult {
    ProcessResult {
        exit_code: None,
        signal: None,
        error_code: Some("outcome_unknown".into()),
        stdout: String::new(),
        stderr: String::new(),
        truncated: false,
    }
}
async fn owned<T: Send + 'static>(
    work: impl std::future::Future<Output = IvyResult<T>> + Send + 'static,
) -> IvyResult<T> {
    tokio::spawn(work).await.map_err(|_| IvyError::internal())?
}
fn require(ok: bool, code: &str, message: &str) -> IvyResult<()> {
    if ok {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn outside_json() -> IvyError {
    IvyError::new("invalid_arguments", "Value is outside the JSON domain.")
}
fn text(value: &IvyJson) -> IvyResult<JsString> {
    value.as_js_string().cloned().ok_or_else(IvyError::internal)
}
fn field(value: &IvyJson, key: &str) -> IvyResult<JsString> {
    text(value.required(key)?)
}
fn array(value: &IvyJson) -> IvyResult<&[IvyJson]> {
    value.as_array().ok_or_else(IvyError::internal)
}
fn optional_root(config: &IvyJson, key: &str) -> IvyResult<Option<JsString>> {
    config
        .get(key)
        .map(|value| {
            value
                .as_js_string()
                .filter(|path| !path.0.is_empty())
                .cloned()
                .ok_or_else(|| {
                    IvyError::new(
                        "invalid_arguments",
                        "Docker roots must be nonempty path strings.",
                    )
                })
        })
        .transpose()
}
fn concat(values: &[JsString]) -> JsString {
    JsString(values.iter().flat_map(|v| v.0.iter().copied()).collect())
}
fn states(value: &IvyJson, states: &[&str]) -> bool {
    states
        .iter()
        .any(|state| value.get("state") == Some(&IvyJson::from(*state)))
}
pub fn container_name(
    host: JsString,
    instance: JsString,
    recovery: Option<IvyJson>,
) -> IvyResult<String> {
    let mut value = IvyJson::object([
        ("hostId", IvyJson::String(host)),
        ("instanceId", IvyJson::String(instance)),
    ]);
    if let Some(recovery) = recovery {
        value.insert("recovery", recovery);
    }
    Ok(format!("ivy-next-{}", &hash_json(&value)?[7..31]))
}
fn owned_container_name(host: &IvyJson, instance: &JsString) -> IvyResult<JsString> {
    let recovery = host
        .get("restoredFrom")
        .map(|v| -> IvyResult<IvyJson> {
            Ok(IvyJson::object([
                ("runtimeRoot", host.required("runtimeRoot")?.clone()),
                ("backupId", v.required("backupId")?.clone()),
            ]))
        })
        .transpose()?;
    Ok(container_name(field(host, "hostId")?, instance.clone(), recovery)?.into())
}
pub fn docker_restart_policy(manifest: &IvyJson) -> IvyResult<JsString> {
    let Some(restart) = manifest.get("restart") else {
        return Ok("never".into());
    };
    let policy = restart.get("policy").and_then(IvyJson::as_js_string);
    require(
        restart.as_object().is_some()
            && policy.is_some_and(|policy| {
                ["never", "on-failure", "always"]
                    .iter()
                    .any(|value| policy == &JsString::from(*value))
            }),
        "invalid_arguments",
        "Docker restart requires a declared restart policy.",
    )?;
    Ok(policy.unwrap().clone())
}
fn repository(value: &JsString) -> bool {
    let mut component = false;
    for unit in &value.0 {
        match *unit {
            48..=57 | 97..=122 => component = true,
            46 | 95 | 45 | 47 if component => component = false,
            _ => return false,
        }
    }
    component
}
fn ip(value: &JsString) -> u8 {
    let Ok(value) = value.to_utf8() else {
        return 0;
    };
    let (address, zone) = value
        .split_once('%')
        .map_or((value.as_str(), None), |(a, z)| (a, Some(z)));
    if let Some(zone) = zone {
        if zone.is_empty()
            || !zone
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-.:".contains(&b))
        {
            return 0;
        }
    }
    match address.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(_)) if zone.is_none() => 4,
        Ok(std::net::IpAddr::V6(_)) => 6,
        _ => 0,
    }
}
fn mount(
    style: node_path::Style,
    path: JsString,
    writable: bool,
    args: &mut Vec<JsString>,
) -> IvyResult<()> {
    require(
        node_path::is_absolute(style, &path)
            && path != JsString::from("/")
            && !path.0.iter().any(|c| *c == 44 || *c <= 31),
        "invalid_arguments",
        "Docker mount needs an explicit unambiguous absolute path.",
    )?;
    args.extend([
        "--mount".into(),
        concat(&[
            "type=bind,src=".into(),
            path.clone(),
            ",dst=".into(),
            path,
            if writable {
                "".into()
            } else {
                ",readonly".into()
            },
        ]),
    ]);
    Ok(())
}
