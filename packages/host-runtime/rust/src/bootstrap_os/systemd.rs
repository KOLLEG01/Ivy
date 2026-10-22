//! Exact retained systemd bootstrap ownership. Reuse the common native file,
//! bounded command, Linux resource and service-identity boundaries.
use super::BootstrapOs;
use crate::{
    IvyError, IvyJson as J, IvyResult, Outcome, bootstrap, config,
    linux_host::{LinuxHost, NativeLinuxHost, trimmed},
    linux_resources, linux_service_identity,
    node_runtime::NodeRuntime,
    systemd::{NativeSystemdHost, SystemdHost},
};
use futures_util::future::BoxFuture;
use ivy_contracts::{
    JsString,
    hash::{digest, hash_json},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    future::Future,
    io,
    path::PathBuf,
    sync::Arc,
};

fn require(value: bool, code: &str, message: &str) -> IvyResult<()> {
    if value {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn array(value: &J) -> IvyResult<&[J]> {
    value.as_array().ok_or_else(IvyError::internal)
}
fn text(value: &J, key: &str) -> IvyResult<JsString> {
    value
        .required(key)?
        .as_js_string()
        .cloned()
        .ok_or_else(IvyError::internal)
}
fn truthy(value: &J) -> bool {
    match value {
        J::Null => false,
        J::Bool(v) => *v,
        J::Number(v) => *v != 0.0 && !v.is_nan(),
        J::String(v) => !v.0.is_empty(),
        _ => true,
    }
}
fn concat(parts: &[JsString]) -> JsString {
    JsString(
        parts
            .iter()
            .flat_map(|part| part.0.iter().copied())
            .collect(),
    )
}
fn unit_path(name: &JsString) -> IvyResult<JsString> {
    let s = &name.0;
    require(
        s.len() == 34
            && s.starts_with(&JsString::from("ivy-next-").0)
            && s[21] == 45
            && s[9..21]
                .iter()
                .chain(&s[22..])
                .all(|u| matches!(*u,48..=57|97..=102)),
        "target_conflict",
        "Invalid owned bootstrap unit name.",
    )?;
    Ok(concat(&[
        "/etc/systemd/system/".into(),
        name.clone(),
        ".service".into(),
    ]))
}
fn properties(output: &JsString) -> BTreeMap<JsString, JsString> {
    trimmed(output)
        .0
        .split(|u| *u == 10)
        .map(|line| {
            let (key, value) = match line.iter().position(|u| *u == 61) {
                Some(i) => (&line[..i], &line[i + 1..]),
                None => (&line[..line.len().saturating_sub(1)], line),
            };
            (JsString(key.to_vec()), JsString(value.to_vec()))
        })
        .collect()
}
fn definition(content: JsString) -> J {
    J::object([
        (
            "hash",
            digest(String::from_utf16_lossy(&content.0).as_bytes()).into(),
        ),
        ("content", J::String(content)),
    ])
}
fn check(definition: &J) -> IvyResult<()> {
    require(
        definition.get("hash")
            == Some(
                &digest(String::from_utf16_lossy(&text(definition, "content")?.0).as_bytes())
                    .into(),
            ),
        "storage_invalid",
        "Retained bootstrap definition changed.",
    )
}
fn owned<T: Send + 'static>(
    work: impl Future<Output = IvyResult<T>> + Send + 'static,
) -> BoxFuture<'static, IvyResult<T>> {
    let owner = tokio::spawn(work);
    Box::pin(async move { owner.await.map_err(|_| IvyError::internal())? })
}

#[derive(Clone)]
pub struct SystemdBootstrapOs {
    files: Arc<dyn SystemdHost>,
    linux: Arc<dyn LinuxHost>,
    node: NodeRuntime,
}
impl SystemdBootstrapOs {
    pub fn new(distribution: PathBuf, node: NodeRuntime) -> Self {
        Self::with_hosts(
            node,
            Arc::new(NativeSystemdHost::new(distribution.clone())),
            Arc::new(NativeLinuxHost::new(distribution)),
        )
    }
    /// Both boundaries describe the same executing host. The native constructor
    /// binds them to one distribution; tests can share an explicit OS model.
    pub fn with_hosts(
        node: NodeRuntime,
        files: Arc<dyn SystemdHost>,
        linux: Arc<dyn LinuxHost>,
    ) -> Self {
        Self { files, linux, node }
    }
    async fn command(&self, args: Vec<JsString>) -> IvyResult<()> {
        self.show(args).await.map(|_| ())
    }
    async fn show(&self, args: Vec<JsString>) -> IvyResult<JsString> {
        self.linux.command("/usr/bin/systemctl",args,60_000,2*1024*1024).await.map_err(|_| {
            IvyError::new("bootstrap_os_unavailable","The bounded OS bootstrap operation did not complete. Inspect the retained maintenance record.").with_outcome(Outcome::Unknown)
        })
    }
    async fn owner(&self, instance: J, plans: Vec<J>) -> IvyResult<J> {
        let name = text(&instance, "name")?;
        let path = unit_path(&name)?;
        let metadata = self
            .files
            .metadata(path.clone())
            .await
            .map_err(|_| IvyError::internal())?;
        require(
            metadata.is_file
                && !metadata.is_symlink
                && metadata.uid == Some(0)
                && metadata.mode & 0o022 == 0
                && metadata.size <= 65536,
            "target_conflict",
            "Bootstrap unit must be a bounded administrator-owned regular file.",
        )?;
        let bytes = self
            .files
            .read(path.clone())
            .await
            .map_err(|_| IvyError::internal())?;
        let content = std::str::from_utf8(&bytes).map_err(|_| {
            IvyError::new(
                "target_conflict",
                "Bootstrap unit must have exact UTF-8 bytes.",
            )
        })?;
        let content = JsString::from(content);
        let mut seen = BTreeSet::new();
        let mut candidates = Vec::new();
        for plan in plans {
            if !seen.insert(hash_json(&plan)?) {
                continue;
            }
            if let Some(process) = array(plan.required("processes")?)?
                .iter()
                .find(|p| p.get("instanceId") == instance.get("instanceId"))
            {
                if bootstrap::linux_unit(&plan, process, None)? == content {
                    candidates.push((plan.clone(), process.clone()));
                }
            }
        }
        require(
            candidates.len() == 1,
            "target_conflict",
            "OS unit does not match one retained immutable bootstrap candidate.",
        )?;
        let (plan, mut owner) = candidates.pop().unwrap();
        if let Some(scope) = plan.get("linuxResources").filter(|v| truthy(v)) {
            linux_resources::inspect_linux_slice_with_host(scope.clone(), self.linux.clone())
                .await?;
        }
        let values=properties(&self.show(vec!["show".into(),concat(&[name,".service".into()]),"--property=ActiveState,SubState,UnitFileState,LoadState,FragmentPath,DropInPaths,Transient".into()]).await?);
        let get = |key: &str| values.get(&JsString::from(key));
        require(
            get("FragmentPath") == Some(&path)
                && get("DropInPaths") == Some(&"".into())
                && get("LoadState") == Some(&"loaded".into())
                && get("Transient") == Some(&"no".into()),
            "target_conflict",
            "Loaded unit has unexpected ownership, overrides or load state.",
        )?;
        require(
            ["enabled", "disabled"]
                .iter()
                .any(|s| get("UnitFileState") == Some(&JsString::from(*s))),
            "target_conflict",
            "Bootstrap unit has an unsupported scheduling state.",
        )?;
        let stopped = ["inactive", "failed"]
            .iter()
            .any(|s| get("ActiveState") == Some(&JsString::from(*s)))
            && ["dead", "failed"]
                .iter()
                .any(|s| get("SubState") == Some(&JsString::from(*s)));
        let definition = definition(content);
        let candidate = owner
            .get("candidateId")
            .filter(|v| !v.is_null())
            .unwrap_or(plan.required("candidateId")?)
            .clone();
        owner.insert("candidateId", candidate);
        owner.insert("definitionHash", definition.required("hash")?.clone());
        owner.insert(
            "enabled",
            (get("UnitFileState") == Some(&"enabled".into())).into(),
        );
        owner.insert("running", (!stopped).into());
        owner.insert("definition", definition);
        Ok(owner)
    }
    async fn inspect_owned(&self, plans: Vec<J>) -> IvyResult<Vec<J>> {
        require(
            !plans.is_empty(),
            "not_found",
            "No retained bootstrap candidate.",
        )?;
        // Like Promise.all, a failing observation does not cancel other admitted
        // owner observations. Each task retains its host and input generation.
        let jobs = array(plans[0].required("processes")?)?
            .iter()
            .cloned()
            .map(|instance| {
                let this = self.clone();
                let plans = plans.clone();
                owned(async move { this.owner(instance, plans).await })
            });
        futures_util::future::try_join_all(jobs).await
    }
    async fn prepare_owned(&self, next: J) -> IvyResult<()> {
        for process in array(next.required("processes")?)?.iter().filter(|p| {
            p.get("user").is_some_and(truthy) && p.get("configPath").is_some_and(truthy)
        }) {
            let identity = J::object([
                ("componentId", process.required("componentId")?.clone()),
                ("user", process.required("user")?.clone()),
                ("configPath", process.required("configPath")?.clone()),
                (
                    "artifactRoot",
                    process
                        .get("artifactRoot")
                        .filter(|v| !v.is_null())
                        .unwrap_or(next.required("artifactRoot")?)
                        .clone(),
                ),
                ("nodeExecutable", next.required("nodeExecutable")?.clone()),
            ]);
            let runtime = config::instance_config(
                config::path_value(process.required("configPath")?)?,
                &self.node,
            )
            .await?;
            linux_service_identity::ensure_linux_process_identity_with_host(
                identity,
                runtime.json().clone(),
                self.linux.clone(),
            )
            .await?;
        }
        if let Some(scope) = next.get("linuxResources").filter(|v| truthy(v)) {
            linux_resources::ensure_linux_slice_with_host(scope.clone(), self.linux.clone())
                .await?;
            self.command(vec!["daemon-reload".into()]).await?;
            linux_resources::inspect_linux_slice_with_host(scope.clone(), self.linux.clone())
                .await?;
        }
        Ok(())
    }
    async fn apply_owned(&self, owner: J, desired: J, plans: Vec<J>) -> IvyResult<()> {
        require(
            self.files.user_id() == Some(0),
            "unsupported_runtime",
            "Bootstrap maintenance needs the systemd administrator.",
        )?;
        check(&desired)?;
        let current = self.owner(owner.clone(), plans.clone()).await?;
        require(
            current.get("running") == Some(&false.into())
                && [owner.get("definitionHash"), desired.get("hash")]
                    .contains(&current.get("definitionHash")),
            "target_conflict",
            "Bootstrap unit changed or started during maintenance.",
        )?;
        if current.get("definitionHash") == desired.get("hash") {
            return Ok(());
        }
        let path = unit_path(&text(&owner, "name")?)?;
        let temporary = concat(&[
            path.clone(),
            ".".into(),
            self.files.uuid().into(),
            ".tmp".into(),
        ]);
        let mut file = self
            .files
            .open(temporary.clone(), true)
            .await
            .map_err(|_| IvyError::internal())?;
        let written = async {
            file.write(String::from_utf16_lossy(&text(&desired, "content")?.0).into_bytes())
                .await
                .map_err(|_| IvyError::internal())?;
            file.sync().await.map_err(|_| IvyError::internal())
        }
        .await;
        file.close().await.map_err(|_| IvyError::internal())?;
        written?;
        // Recheck immediately before replacing this one exact owned unit. There
        // is no multi-unit systemd CAS in the original boundary.
        let published = async {
            let rechecked = self.owner(owner.clone(), plans).await?;
            require(
                rechecked.get("running") == Some(&false.into())
                    && rechecked.get("definitionHash") == current.get("definitionHash"),
                "target_conflict",
                "Bootstrap unit changed before publication.",
            )?;
            self.files
                .rename(temporary.clone(), path)
                .await
                .map_err(|_| IvyError::internal())?;
            let mut directory = self
                .files
                .open("/etc/systemd/system".into(), false)
                .await
                .map_err(|_| IvyError::internal())?;
            let synced = directory.sync().await;
            directory.close().await.map_err(|_| IvyError::internal())?;
            synced.map_err(|_| IvyError::internal())
        }
        .await;
        match self.files.unlink(temporary).await {
            Ok(()) => (),
            Err(error) if error.kind() == io::ErrorKind::NotFound => (),
            Err(_) => return Err(IvyError::internal()),
        }
        published
    }
    async fn exact(&self, snapshot: &J, plans: Vec<J>) -> IvyResult<Vec<J>> {
        let first = plans.first();
        require(
            first.is_some_and(|p| {
                snapshot.get("hostId") == p.get("hostId")
                    && snapshot.get("installationId") == p.get("installationId")
                    && snapshot
                        .get("owners")
                        .and_then(J::as_array)
                        .map(|s| s.len())
                        == p.get("processes").and_then(J::as_array).map(|s| s.len())
            }),
            "target_conflict",
            "Bootstrap snapshot belongs to another installation.",
        )?;
        let owners = self.inspect_owned(plans).await?;
        let states = array(snapshot.required("owners")?)?;
        require(
            owners.iter().all(|owner| {
                states
                    .iter()
                    .find(|s| {
                        s.get("instanceId") == owner.get("instanceId")
                            && s.get("name") == owner.get("name")
                    })
                    .is_some_and(|s| {
                        s.get("candidateId") == owner.get("candidateId")
                            && s.get("definitionHash") == owner.get("definitionHash")
                    })
            }),
            "target_conflict",
            "Bootstrap owners changed after their exact snapshot.",
        )?;
        Ok(owners)
    }
    async fn pause_owned(&self, snapshot: J, plans: Vec<J>) -> IvyResult<()> {
        let mut owners = self.exact(&snapshot, plans.clone()).await?;
        owners.sort_by_key(|owner| owner.get("componentId") == Some(&"host-executor".into()));
        for owner in owners {
            if owner.get("running").is_some_and(truthy) {
                self.command(vec![
                    "stop".into(),
                    concat(&[text(&owner, "name")?, ".service".into()]),
                ])
                .await?;
            }
        }
        require(
            self.inspect_owned(plans)
                .await?
                .iter()
                .all(|o| !o.get("running").is_some_and(truthy)),
            "bootstrap_busy",
            "Bootstrap owners did not stop for maintenance.",
        )
    }
    async fn resume_owned(&self, snapshot: J, plans: Vec<J>) -> IvyResult<()> {
        let owners = self.inspect_owned(plans).await?;
        let states = array(snapshot.required("owners")?)?;
        require(
            owners.len() == states.len()
                && owners.iter().all(|owner| {
                    states
                        .iter()
                        .find(|s| {
                            s.get("instanceId") == owner.get("instanceId")
                                && s.get("name") == owner.get("name")
                        })
                        .is_some_and(|s| s.get("enabled") == owner.get("enabled"))
                }),
            "target_conflict",
            "Bootstrap scheduling state changed during maintenance.",
        )?;
        for state in states {
            if state.get("running").is_some_and(truthy) {
                self.command(vec![
                    "start".into(),
                    concat(&[text(state, "name")?, ".service".into()]),
                ])
                .await?;
            }
        }
        Ok(())
    }
    async fn launch_owned(&self, plan: J, request: PathBuf, operation: String) -> IvyResult<()> {
        require(
            self.linux.user_id() == Some(0),
            "unsupported_runtime",
            "Automatic bootstrap maintenance needs the systemd administrator.",
        )?;
        let hash = digest(operation.as_bytes());
        let unit = concat(&[
            text(&plan, "installationId")?,
            "-maintenance-".into(),
            hash[7..19].into(),
        ]);
        let state = self
            .linux
            .command(
                "/usr/bin/systemctl",
                vec![
                    "show".into(),
                    concat(&[unit.clone(), ".service".into()]),
                    "--property=LoadState,ActiveState".into(),
                ],
                10_000,
                65_536,
            )
            .await
            .unwrap_or_default();
        let state = properties(&state);
        if state.get(&JsString::from("LoadState")) == Some(&JsString::from("loaded"))
            && state.get(&JsString::from("ActiveState")).is_some_and(|v| {
                v == &JsString::from("active") || v == &JsString::from("activating")
            })
        {
            return Ok(());
        }
        let worker = tokio::task::spawn_blocking(move || {
            super::automatic::worker(&plan, &request, "ivy-bootstrap-auto-worker")
        })
        .await
        .map_err(|_| IvyError::internal())??;
        let mut args = vec![
            "--unit".into(),
            unit,
            "--collect".into(),
            "--property=Type=exec".into(),
            "--property=RuntimeMaxSec=15min".into(),
            "--".into(),
            worker.executable,
        ];
        args.extend(worker.args);
        self.linux.command("/usr/bin/systemd-run", args, 60_000, 2 * 1024 * 1024).await
            .map(|_| ()).map_err(|_| IvyError::new("bootstrap_os_unavailable", "The bounded OS bootstrap operation did not complete. Inspect the retained maintenance record.").with_outcome(Outcome::Unknown))
    }
}
impl BootstrapOs for SystemdBootstrapOs {
    fn inspect(&self, plans: Vec<J>) -> BoxFuture<'_, IvyResult<Vec<J>>> {
        let this = self.clone();
        owned(async move { this.inspect_owned(plans).await })
    }
    fn render(&self, owner: J, next: J) -> BoxFuture<'_, IvyResult<J>> {
        owned(async move {
            let process = array(next.required("processes")?)?
                .iter()
                .find(|p| p.get("instanceId") == owner.get("instanceId"));
            require(
                process.is_some(),
                "target_conflict",
                "Next bootstrap plan does not contain the owned process.",
            )?;
            Ok(definition(bootstrap::linux_unit(
                &next,
                process.unwrap(),
                None,
            )?))
        })
    }
    fn check(&self, definition: J) -> BoxFuture<'_, IvyResult<()>> {
        owned(async move { check(&definition) })
    }
    fn apply(&self, owner: J, desired: J, plans: Vec<J>) -> BoxFuture<'_, IvyResult<()>> {
        let this = self.clone();
        owned(async move { this.apply_owned(owner, desired, plans).await })
    }
    fn reload(&self) -> BoxFuture<'_, IvyResult<()>> {
        let this = self.clone();
        owned(async move { this.command(vec!["daemon-reload".into()]).await })
    }
    fn prepare(&self, next: J) -> Option<BoxFuture<'_, IvyResult<()>>> {
        let this = self.clone();
        Some(owned(async move { this.prepare_owned(next).await }))
    }
    fn pause(&self, snapshot: J, plans: Vec<J>) -> Option<BoxFuture<'_, IvyResult<()>>> {
        let this = self.clone();
        Some(owned(
            async move { this.pause_owned(snapshot, plans).await },
        ))
    }
    fn resume(&self, snapshot: J, plans: Vec<J>) -> Option<BoxFuture<'_, IvyResult<()>>> {
        let this = self.clone();
        Some(owned(
            async move { this.resume_owned(snapshot, plans).await },
        ))
    }
    fn can_pause_and_resume(&self) -> bool {
        true
    }
    fn launch_automatic(
        &self,
        plan: J,
        request: PathBuf,
        operation: String,
    ) -> Option<BoxFuture<'_, IvyResult<()>>> {
        let this = self.clone();
        Some(owned(async move {
            this.launch_owned(plan, request, operation).await
        }))
    }
}
