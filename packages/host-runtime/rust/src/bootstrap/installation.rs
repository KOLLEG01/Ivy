//! Initial installation keeps reviewed plans, retained launch files and journal
//! state in the same order as the bootstrap command. Maintenance is a separate
//! stopped-owner operation, not a replacement for this initial installer.
use super::{
    bootstrap_components, configuration_bootstrap_plan, field,
    publish_bootstrap_instance_configurations, require, retain_bootstrap_plan,
    retained_bootstrap_plans,
};
use crate::{
    IvyError, IvyJson as J, IvyResult, Outcome,
    artifact::FileCompare,
    backup_files, config, exec_file,
    executor_lock::ExecutorLock,
    host_config,
    journal::HostJournal,
    linux_host::{LinuxHost, trimmed},
    linux_resources, linux_service_identity, native_path,
    node_runtime::NodeRuntime,
    process,
    process_output::Utf8Decoder,
    systemd::SystemdHost,
};
use ivy_contracts::{JsString, core_schema::validate_host, hash::hash_json, json};
use ivy_sdk::clock::Clock;
use std::{collections::BTreeMap, path::PathBuf, sync::Arc};

mod effects;
use effects::InitialInstallEffects;

#[derive(Clone)]
pub(crate) struct BootstrapInstallation {
    node: Option<NodeRuntime>,
    compare: FileCompare,
    distribution: PathBuf,
    job_launcher: PathBuf,
    clock: Arc<dyn Clock>,
    effects: InitialInstallEffects,
}

impl BootstrapInstallation {
    pub(crate) fn native(
        node: impl Into<Option<NodeRuntime>>,
        compare: FileCompare,
        distribution: PathBuf,
        job_launcher: PathBuf,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            effects: InitialInstallEffects::native(distribution.clone()),
            node: node.into(),
            compare,
            distribution,
            job_launcher,
            clock,
        }
    }

    pub async fn plan(
        &self,
        path: PathBuf,
        candidate_id: JsString,
        mut resources: Option<J>,
        preferred: &[J],
    ) -> IvyResult<J> {
        let path = std::env::current_dir()
            .map_err(|_| IvyError::internal())?
            .join(path);
        let path = crate::paths::absolute(&path).unwrap_or_else(|_| native_path::lexical(&path));
        let config = host_config::host_config(path.clone()).await?;
        let journal = HostJournal::open_with_clock(config.clone(), self.clock.clone()).await?;
        let result = async {
            let candidate = journal.candidate(&candidate_id.to_utf8()?).await?;
            let manifest = crate::artifact::verify_candidate(
                candidate.clone(),
                config::path_value(config.required("artifactRoot")?)?,
                self.node.as_ref(),
            )
            .await?;
            let selected = config::path_value(config.required("runtimeRoot")?)?
                .join("bootstrap")
                .join(format!(
                    "{}.json",
                    linux_resources::installation_identity(&config)?
                ));
            if resources.is_none()
                && cfg!(target_os = "linux")
                && backup_files::exists(&selected).await?
            {
                resources = retained_bootstrap_plans(config.clone())
                    .await?
                    .first()
                    .and_then(|plan| plan.get("linuxResources"))
                    .and_then(|scope| scope.get("budget"))
                    .cloned();
            }
            let mut releases = vec![J::object([
                ("candidate", candidate.clone()),
                ("manifest", manifest),
            ])];
            releases.extend_from_slice(preferred);
            let mut components =
                bootstrap_components(&config, &journal, &candidate, &releases, &self.compare)
                    .await?;
            for selected in &mut components {
                let selected_candidate = selected.required("candidate")?.clone();
                let verified = if selected_candidate == candidate {
                    releases[0].required("manifest")?.clone()
                } else {
                    crate::artifact::verify_candidate(
                        selected_candidate,
                        config::path_value(config.required("artifactRoot")?)?,
                        self.node.as_ref(),
                    )
                    .await?
                };
                selected.insert("manifest", verified);
            }
            configuration_bootstrap_plan(
                &config,
                &native_path::from_native(&path),
                &candidate,
                resources.as_ref().filter(|v| !v.is_null()),
                &components,
            )
        }
        .await;
        let closed = journal.close().await;
        match result {
            Err(error) => Err(error),
            Ok(value) => closed.map(|_| value),
        }
    }

    pub async fn install(&self, plan: J, replace_legacy: bool) -> IvyResult<J> {
        validate_host("BootstrapPlan", &plan)?;
        let config =
            host_config::host_config(config::path_value(plan.required("configPath")?)?).await?;
        let lock = ExecutorLock::acquire(
            config::path_value(config.required("runtimeRoot")?)?.join("bootstrap-maintenance-lock"),
            false,
        )
        .await?;
        let result = self.install_owned(&plan, &config, replace_legacy).await;
        let closed = lock.close().await;
        match result {
            Err(error) => Err(error),
            Ok(value) => closed.map(|_| value),
        }
    }

    async fn install_owned(&self, plan: &J, config: &J, replace_legacy: bool) -> IvyResult<J> {
        require(
            Some(&J::from(hash_json(config)?)) == plan.get("configHash"),
            "configuration_changed",
            "Host configuration changed after the reviewed installation plan.",
        )?;
        let path = config::path_value(plan.required("runtimeRoot")?)?
            .join("bootstrap")
            .join(format!(
                "{}.json",
                field(plan, "installationId")?.to_utf8()?
            ));
        let journal = HostJournal::open_with_clock(config.clone(), self.clock.clone()).await?;
        let result = async {
            let mut preferred = Vec::new();
            for process in processes(plan)?.iter().filter(|p| {
                p.get("componentId") != Some(&"host-executor".into())
                    && p.get("candidateId").and_then(J::as_js_string).is_some_and(|text| !text.0.is_empty())
            }) {
                let candidate = journal
                    .candidate(&process.required("candidateId")?.as_str()?)
                    .await?;
                let manifest = journal
                    .manifest(&candidate.required("candidateId")?.as_str()?)
                    .await?;
                preferred.push(J::object([
                    ("candidate", candidate),
                    ("manifest", manifest),
                ]));
            }
            let rebuilt = self
                .plan(
                    config::path_value(plan.required("configPath")?)?,
                    field(plan, "candidateId")?,
                    plan.get("linuxResources")
                        .and_then(|scope| scope.get("budget"))
                        .cloned(),
                    &preferred,
                )
                .await?;
            require(
                hash_json(plan)? == hash_json(&rebuilt)?,
                "target_conflict",
                "Installation plan differs from the verified candidate and host configuration.",
            )?;
            retain_bootstrap_plan(plan, self.job_launcher.clone()).await?;
            config::atomic_json(path.clone(), plan.clone()).await?;
            publish_bootstrap_instance_configurations(plan, config, &journal).await?;
            if cfg!(target_os = "linux") {
                for process in processes(plan)?.iter().filter(|p| {
                    p.get("user").and_then(J::as_js_string).is_some_and(|text| !text.0.is_empty()) && p.get("configPath").and_then(J::as_js_string).is_some_and(|text| !text.0.is_empty())
                }) {
                    let runtime = config::instance_config(
                        config::path_value(process.required("configPath")?)?,
                        self.node.as_ref(),
                    )
                    .await?;
                    linux_service_identity::ensure_linux_process_identity_with_host(
                        J::object([
                            ("componentId", process.required("componentId")?.clone()),
                            ("user", process.required("user")?.clone()),
                            ("configPath", process.required("configPath")?.clone()),
                            (
                                "artifactRoot",
                                process
                                    .get("artifactRoot")
                                    .filter(|v| !v.is_null())
                                    .unwrap_or(plan.required("artifactRoot")?)
                                    .clone(),
                            ),
                            ("nodeExecutable", J::String(super::process_executable(plan, process)?)),
                        ]),
                        runtime.json().clone(),
                        self.effects.linux.clone(),
                    )
                    .await?;
                }
            }
            Ok(())
        }
        .await;
        let closed = journal.close().await;
        result?;
        closed?;
        if cfg!(windows) {
            return self
                .finish_windows(plan, config, path, replace_legacy)
                .await;
        }
        self.finish_linux(plan, config, replace_legacy).await
    }

    async fn finish_windows(
        &self,
        plan: &J,
        config: &J,
        path: PathBuf,
        replace_legacy: bool,
    ) -> IvyResult<J> {
        let result = (self.effects.execute_windows)(
            windows_command(self.distribution.clone(), path, replace_legacy),
            self.distribution.clone(),
            exec_file::FileOptions {
                job_launcher: Some(self.job_launcher.clone()),
                environment: Some(process::runtime_environment(&BTreeMap::new())),
                ..Default::default()
            },
        )
        .await?;
        if result.failure.is_some() {
            return Err(IvyError::internal());
        }
        let output =
            json::parse(&result.stdout.to_utf8()?, usize::MAX).map_err(|_| IvyError::internal())?;
        validate_host("CliOutput", &output)?;
        self.sync(config, plan, true).await?;
        Ok(output)
    }

    async fn finish_linux(&self, plan: &J, config: &J, replace_legacy: bool) -> IvyResult<J> {
        let linux = self.effects.linux.as_ref();
        require(
            linux.platform() == "linux" && linux.user_id() == Some(0),
            "unsupported_runtime",
            "Linux bootstrap installation needs the host systemd administrator.",
        )?;
        self.replace_systemd_owners(plan, replace_legacy, linux)
            .await?;
        if let Some(scope) = plan.get("linuxResources").filter(|v| v.as_object().is_some()) {
            linux_resources::ensure_linux_slice_with_host(
                scope.clone(),
                self.effects.linux.clone(),
            )
            .await?;
        }
        let mut names = Vec::new();
        let files = self.effects.units.as_ref();
        for process in processes(plan)? {
            let name = field(process, "name")?;
            require(
                name.0
                    .starts_with(&super::concat(&[field(plan, "installationId")?, "-".into()]).0)
                    && valid_name(&name),
                "target_conflict",
                "Unit name is outside this installation.",
            )?;
            let name = super::concat(&[name, ".service".into()]);
            let path = unit_path(&name);
            let content = super::linux_unit(plan, process, None)?;
            let old = read_unit(path.clone(), files).await?;
            require(
                old.as_ref().is_none_or(|v| v == &content),
                "target_conflict",
                "Existing systemd unit differs from this immutable bootstrap.",
            )?;
            if old.is_none() {
                create_unit(path, content, files).await?;
            }
            names.push(J::String(name));
        }
        self.sync(config, plan, false).await?;
        command(linux, vec!["daemon-reload".into()]).await?;
        if let Some(scope) = plan.get("linuxResources").filter(|v| v.as_object().is_some()) {
            linux_resources::inspect_linux_slice_with_host(
                scope.clone(),
                self.effects.linux.clone(),
            )
            .await?;
        }
        for name in &names {
            command(
                linux,
                vec![
                    "enable".into(),
                    "--now".into(),
                    name.as_js_string()
                        .cloned()
                        .ok_or_else(IvyError::internal)?,
                ],
            )
            .await?;
        }
        Ok(J::object([
            ("schemaVersion", J::Number(1.0)),
            ("ok", true.into()),
            ("code", "bootstrap_installed".into()),
            ("data", J::object([("units", J::Array(names))])),
        ]))
    }

    async fn sync(&self, config: &J, plan: &J, windows: bool) -> IvyResult<()> {
        let journal = HostJournal::open_with_clock(config.clone(), self.clock.clone()).await?;
        let result = journal.sync_bootstrap(plan.clone(), windows).await;
        let closed = journal.close().await;
        result?;
        closed
    }

    async fn replace_systemd_owners(
        &self,
        plan: &J,
        allow_legacy: bool,
        linux: &dyn LinuxHost,
    ) -> IvyResult<()> {
        let identity = field(plan, "installationId")?;
        let entries = (self.effects.unit_entries)()
            .await?
            .into_iter()
            .filter(|entry| entry.is_file)
            .filter_map(|entry| {
                let name = entry.name;
                let prefix = super::concat(&[identity.clone(), "-".into()]);
                let suffix = &name.0[prefix.0.len().min(name.0.len())..];
                let matches = name.0.starts_with(&prefix.0)
                    && suffix.len() == 20
                    && suffix[..12].iter().all(|u| matches!(*u, 48..=57|97..=102))
                    && suffix[12..] == JsString::from(".service").0;
                matches.then_some(name)
            })
            .collect::<Vec<_>>();
        let desired = processes(plan)?
            .iter()
            .map(|process| {
                Ok((
                    super::concat(&[field(process, "name")?, ".service".into()]),
                    super::linux_unit(plan, process, None)?,
                ))
            })
            .collect::<IvyResult<Vec<_>>>()?;
        let mut current = Vec::new();
        let mut replacement = false;
        for name in entries {
            let path = unit_path(&name);
            let content = read_unit(path.clone(), self.effects.units.as_ref())
                .await?
                .ok_or_else(IvyError::internal)?;
            if !desired
                .iter()
                .any(|(n, value)| n == &name && value == &content)
            {
                require(
                    super::replaceable_systemd_owner(&content, allow_legacy),
                    "target_conflict",
                    "Existing systemd owner is not an explicitly recognized Ivy unit.",
                )?;
                replacement = true;
            }
            current.push((name, path));
        }
        if !replacement {
            return Ok(());
        }
        for (name, path) in current {
            command(linux, vec!["disable".into(), "--now".into(), name.clone()]).await?;
            let deadline = self.clock.utc_millis() + 30_000;
            loop {
                let state = command(
                    linux,
                    vec![
                        "show".into(),
                        "--no-pager".into(),
                        "--property=ActiveState,SubState".into(),
                        name.clone(),
                    ],
                )
                .await?;
                if stopped(&state) {
                    break;
                }
                if self.clock.utc_millis() >= deadline {
                    return Err(IvyError::new(
                        "outcome_unknown",
                        "Existing systemd owner did not establish a stopped outcome.",
                    )
                    .with_outcome(Outcome::Unknown));
                }
                self.effects.units.sleep(100).await;
            }
            self.effects
                .units
                .unlink(native_path::from_native(&path))
                .await
                .map_err(|_| IvyError::internal())?;
        }
        Ok(())
    }
}

fn processes(plan: &J) -> IvyResult<&[J]> {
    plan.required("processes")?
        .as_array()
        .ok_or_else(IvyError::internal)
}
fn windows_command(
    distribution: PathBuf,
    path: PathBuf,
    replace_legacy: bool,
) -> exec_file::FileCommand {
    let mut args = vec![
        "-NoProfile".into(),
        "-NonInteractive".into(),
        "-ExecutionPolicy".into(),
        "Bypass".into(),
        "-File".into(),
        distribution
            .join("dist/packages/host-runtime/assets/install-windows.ps1")
            .into_os_string(),
        "-PlanPath".into(),
        path.into_os_string(),
    ];
    if replace_legacy {
        args.push("-ReplaceLegacy".into());
    }
    exec_file::FileCommand {
        executable: PathBuf::from(
            std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into()),
        )
        .join("System32/WindowsPowerShell/v1.0/powershell.exe")
        .into_os_string(),
        args,
        cwd: None,
        timeout_ms: 60_000,
        maximum_buffer_bytes: 65_536,
    }
}
fn valid_name(name: &JsString) -> bool {
    let s = &name.0;
    s.len() == 34
        && s.starts_with(&JsString::from("ivy-next-").0)
        && s[21] == 45
        && s[9..21]
            .iter()
            .chain(&s[22..])
            .all(|u| matches!(*u, 48..=57|97..=102))
}
fn unit_path(name: &JsString) -> PathBuf {
    PathBuf::from("/etc/systemd/system").join(native_path::to_native(name))
}
async fn read_unit(path: PathBuf, files: &dyn SystemdHost) -> IvyResult<Option<JsString>> {
    match files.read(native_path::from_native(&path)).await {
        Ok(bytes) => Ok(Some(JsString(
            Utf8Decoder::preserving_bom().decode(&bytes, true),
        ))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(IvyError::internal()),
    }
}
async fn create_unit(path: PathBuf, content: JsString, files: &dyn SystemdHost) -> IvyResult<()> {
    let mut file = files
        .open(native_path::from_native(&path), true)
        .await
        .map_err(|_| IvyError::internal())?;
    let written = file
        .write(String::from_utf16_lossy(&content.0).into_bytes())
        .await;
    let closed = file.close().await;
    written.map_err(|_| IvyError::internal())?;
    closed.map_err(|_| IvyError::internal())
}
async fn command(linux: &dyn LinuxHost, args: Vec<JsString>) -> IvyResult<JsString> {
    linux
        .command("/usr/bin/systemctl", args, 30_000, 65_536)
        .await
}
fn stopped(output: &JsString) -> bool {
    let values = trimmed(output)
        .0
        .split(|u| *u == 10)
        .map(|line| {
            let (key, value) = match line.iter().position(|u| *u == 61) {
                Some(i) => (&line[..i], &line[i + 1..]),
                None => (&line[..line.len().saturating_sub(1)], line),
            };
            (JsString(key.to_vec()), JsString(value.to_vec()))
        })
        .collect::<BTreeMap<_, _>>();
    ["inactive", "failed"]
        .iter()
        .any(|s| values.get(&JsString::from("ActiveState")) == Some(&JsString::from(*s)))
        && ["dead", "failed"]
            .iter()
            .any(|s| values.get(&JsString::from("SubState")) == Some(&JsString::from(*s)))
}

#[cfg(test)]
#[path = "../../../../../tests/rust/host-bootstrap-installation-cases.rs"]
mod tests;
