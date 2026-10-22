//! Request-scoped native host actions for the CLI and other local callers.
//! Argument admission and JSON/exit formatting belong to the calling shell.
mod backup;
mod bootstrap;
mod preparation;
mod reset;
use crate::{
    IvyError, IvyJson as J, IvyResult,
    actions::{LocalAction, accept_local_action},
    artifact::FileCompare,
    bootstrap::BootstrapInstallation,
    bootstrap_maintenance::BootstrapMaintenance,
    bootstrap_os::bootstrap_os,
    config, host_config,
    journal::{HostJournal, fresh_executor_status},
    node_runtime::NodeRuntime,
    preparation_progress::preparation_progress_with_clock,
};
pub use backup::RestoreRuntime;
use ivy_contracts::{JsString, core_schema::validate_host, json};
use ivy_sdk::clock::{Clock, SystemClock, parse_date};
use std::{path::PathBuf, sync::Arc, time::Duration};

pub type ProgressSink = Arc<dyn Fn(&JsString) + Send + Sync>;

/// A later wait/display read can fail after durable acceptance. The shell must
/// retain this identity when it formats the error, and must not resubmit work.
pub struct LocalActionResult {
    pub entry: IvyResult<J>,
    pub deployment_id: Option<JsString>,
}

#[derive(Clone)]
pub struct HostActions {
    distribution: PathBuf,
    job_launcher: PathBuf,
    maintenance: BootstrapMaintenance,
    installation: BootstrapInstallation,
    clock: Arc<dyn Clock>,
}
impl HostActions {
    /// Node and the locale comparator have already passed caller qualification.
    /// No parser, HTTP profile or mutable configuration is captured here.
    pub fn native(
        node: NodeRuntime,
        compare: FileCompare,
        distribution: PathBuf,
        job_launcher: PathBuf,
    ) -> IvyResult<Self> {
        Self::with_clock(
            node,
            compare,
            distribution,
            job_launcher,
            Arc::new(SystemClock),
        )
    }

    pub fn with_clock(
        node: NodeRuntime,
        compare: FileCompare,
        distribution: PathBuf,
        job_launcher: PathBuf,
        clock: Arc<dyn Clock>,
    ) -> IvyResult<Self> {
        let os = bootstrap_os(distribution.clone(), job_launcher.clone(), node.clone())?;
        Ok(Self {
            installation: BootstrapInstallation::native(
                node.clone(),
                compare.clone(),
                distribution.clone(),
                job_launcher.clone(),
                clock.clone(),
            ),
            distribution,
            job_launcher: job_launcher.clone(),
            maintenance: BootstrapMaintenance::with_clock(
                node,
                compare,
                os,
                job_launcher,
                clock.clone(),
            ),
            clock,
        })
    }

    pub fn bootstrap(&self) -> BootstrapMaintenance {
        self.maintenance.clone()
    }

    /// Each invocation owns a fresh configuration and journal through close.
    /// Dropping the response observer never cancels admitted capture/accept/wait.
    pub async fn lifecycle(
        &self,
        config_path: PathBuf,
        action: LocalAction,
        wait_ms: f64,
        progress: Option<ProgressSink>,
    ) -> LocalActionResult {
        let this = self.clone();
        tokio::spawn(async move {
            this.lifecycle_owned(config_path, action, wait_ms, progress)
                .await
        })
        .await
        .unwrap_or_else(|_| LocalActionResult {
            entry: Err(IvyError::internal()),
            deployment_id: None,
        })
    }

    async fn lifecycle_owned(
        &self,
        config_path: PathBuf,
        action: LocalAction,
        wait_ms: f64,
        progress: Option<ProgressSink>,
    ) -> LocalActionResult {
        let mut deployment_id = None;
        let open = async {
            if !wait_ms.is_finite()
                || wait_ms.fract() != 0.0
                || !(0.0..=600_000.0).contains(&wait_ms)
            {
                return Err(IvyError::new(
                    "invalid_arguments",
                    "--wait-ms must be a finite integer from 0 to 600000.",
                ));
            }
            let config = host_config::host_config(config_path).await?;
            let journal = HostJournal::open_with_clock(config.clone(), self.clock.clone()).await?;
            Ok((config, journal))
        }
        .await;
        let entry = match open {
            Err(error) => Err(error),
            Ok((config, journal)) => {
                let result = async {
                    let entry =
                        accept_local_action(journal.clone(), action, self.distribution.clone())
                            .await?;
                    deployment_id = Some(
                        entry
                            .required("record")?
                            .required("deploymentId")?
                            .as_js_string()
                            .cloned()
                            .ok_or_else(IvyError::internal)?,
                    );
                    let entry = self
                        .wait(&config, &journal, entry, wait_ms, progress.as_ref())
                        .await?;
                    self.deployment_data(&config, entry).await
                }
                .await;
                close(journal, result).await
            }
        };
        LocalActionResult {
            entry,
            deployment_id,
        }
    }

    pub async fn deployment(&self, config_path: PathBuf, deployment_id: JsString) -> IvyResult<J> {
        let this = self.clone();
        tokio::spawn(async move {
            let config = host_config::host_config(config_path).await?;
            let journal = HostJournal::open_with_clock(config.clone(), this.clock.clone()).await?;
            let result = async {
                let entry = journal.get(&deployment_id.to_utf8()?).await?;
                this.deployment_data(&config, entry).await
            }
            .await;
            close(journal, result).await
        })
        .await
        .map_err(|_| IvyError::internal())?
    }

    /// The CLI's host_status data is distinct from the manager's HostStatus.
    pub async fn status(&self, config_path: PathBuf) -> IvyResult<J> {
        let this = self.clone();
        tokio::spawn(async move {
            let config = host_config::host_config(config_path).await?;
            let journal = HostJournal::open_with_clock(config.clone(), this.clock.clone()).await?;
            let result = async {
                let executor = config::json_file(
                    config::path_value(config.required("runtimeRoot")?)?.join("executor.json"),
                    config::CONFIGURATION_BYTES,
                )
                .await
                .ok()
                .filter(|value| {
                    validate_host("ExecutorStatus", value).is_ok()
                        && value.get("hostId") == config.get("hostId")
                        && fresh_executor_status(value, this.clock.as_ref())
                });
                let mut instances = Vec::new();
                for instance in config
                    .required("instances")?
                    .as_array()
                    .ok_or_else(IvyError::internal)?
                {
                    let installed = journal
                        .installed(&instance.required("instanceId")?.as_str()?)
                        .await?;
                    let enabled = installed
                        .as_ref()
                        .and_then(|value| value.get("enabled"))
                        .unwrap_or(instance.required("enabled")?)
                        .clone();
                    instances.push(J::object([
                        ("instanceId", instance.required("instanceId")?.clone()),
                        ("componentId", instance.required("componentId")?.clone()),
                        ("enabled", enabled),
                        ("installed", installed.unwrap_or(J::Null)),
                    ]));
                }
                let unfinished = journal
                    .unfinished()
                    .await?
                    .into_iter()
                    .map(|entry| entry.required("record").cloned())
                    .collect::<IvyResult<Vec<_>>>()?;
                let mut data = J::object([
                    ("hostId", config.required("hostId")?.clone()),
                    ("executor", executor.clone().unwrap_or(J::Null)),
                    ("instances", J::Array(instances)),
                    (
                        "observations",
                        journal.current_observations(executor).await?,
                    ),
                    ("unfinished", J::Array(unfinished)),
                    (
                        "restoredFrom",
                        config.get("restoredFrom").cloned().unwrap_or(J::Null),
                    ),
                ]);
                if let Some(status) = journal.storage_retention_status().await? {
                    data.insert("storageRetention", status);
                }
                Ok(data)
            }
            .await;
            close(journal, result).await
        })
        .await
        .map_err(|_| IvyError::internal())?
    }

    async fn deployment_data(&self, config: &J, mut entry: J) -> IvyResult<J> {
        if let Some(progress) =
            preparation_progress_with_clock(config, &entry, self.clock.as_ref()).await?
        {
            entry.insert("progress", progress);
        }
        Ok(entry)
    }

    async fn wait(
        &self,
        config: &J,
        journal: &HostJournal,
        mut entry: J,
        wait_ms: f64,
        progress: Option<&ProgressSink>,
    ) -> IvyResult<J> {
        let end = self.clock.utc_millis() as f64 + wait_ms;
        let (mut next_progress, mut last_reported) = (0.0, 0.0);
        let (mut last_message, mut last_failure) = (J::from(""), J::from(""));
        while !terminal(entry.required("record")?)? && (self.clock.utc_millis() as f64) < end {
            let record = entry.required("record")?;
            let id = record.required("deploymentId")?;
            if let Some(sink) = progress {
                if let Some(error) = record.get("errorCode").filter(|value| {
                    value
                        .as_js_string()
                        .is_some_and(|value| !value.0.is_empty())
                }) && error != &last_failure
                {
                    last_failure = error.clone();
                    emit(
                        sink,
                        parts(&[
                            id.clone(),
                            ": FAILED ".into(),
                            error.clone(),
                            " | ".into(),
                            record.required("readiness")?.required("message")?.clone(),
                            " | ".into(),
                            record.required("phase")?.clone(),
                        ])?,
                    );
                }
                let now = self.clock.utc_millis() as f64;
                if now >= next_progress {
                    let value =
                        preparation_progress_with_clock(config, &entry, self.clock.as_ref())
                            .await?;
                    let step = value
                        .as_ref()
                        .and_then(|value| value.get("step"))
                        .unwrap_or(record.required("phase")?);
                    if step != &last_message || now - last_reported >= 15_000.0 {
                        let elapsed = value
                            .as_ref()
                            .and_then(|v| v.get("stepElapsedMs"))
                            .and_then(J::as_f64)
                            .unwrap_or(
                                now - parse_date(
                                    record
                                        .required("updatedAt")?
                                        .as_js_string()
                                        .ok_or_else(IvyError::internal)?,
                                ),
                            );
                        let mut message = vec![
                            id.clone(),
                            ": ".into(),
                            step.clone(),
                            " (".into(),
                            J::Number((elapsed / 1000.0 + 0.5).floor()),
                            "s)".into(),
                        ];
                        if let Some(value) = &value {
                            for (key, before, after) in [
                                ("command", " | ", ""),
                                ("fileBytes", " | ", " bytes"),
                                ("lastOutputAt", " | last output ", ""),
                            ] {
                                if let Some(v) = value.get(key).filter(|v| !v.is_null()) {
                                    if key == "fileBytes"
                                        || v.as_js_string().is_some_and(|v| !v.0.is_empty())
                                    {
                                        message.extend([before.into(), v.clone(), after.into()]);
                                    }
                                }
                            }
                            if let Some(age) = value
                                .get("heartbeatAgeMs")
                                .and_then(J::as_f64)
                                .filter(|age| *age > 15_000.0)
                            {
                                message.extend([
                                    " | heartbeat ".into(),
                                    J::Number((age / 1000.0 + 0.5).floor()),
                                    "s old".into(),
                                ]);
                            }
                        }
                        emit(sink, parts(&message)?);
                        last_message = step.clone();
                        last_reported = now;
                    }
                    next_progress = now + 5000.0;
                }
            }
            tokio::time::sleep(Duration::from_secs_f64(
                ((end - self.clock.utc_millis() as f64).clamp(0.0, 250.0)) / 1000.0,
            ))
            .await;
            entry = journal.get(&id.as_str()?).await?;
        }
        Ok(entry)
    }
}

async fn close<T>(journal: HostJournal, result: IvyResult<T>) -> IvyResult<T> {
    let closed = journal.close().await;
    match result {
        Err(error) => Err(error),
        Ok(value) => closed.map(|_| value),
    }
}
fn terminal(record: &J) -> IvyResult<bool> {
    Ok(matches!(
        record.required("phase")?.as_str()?.as_str(),
        "succeeded" | "rolled_back" | "failed" | "needs_attention"
    ))
}
fn parts(values: &[J]) -> IvyResult<JsString> {
    let mut result = Vec::new();
    for value in values {
        result.extend(json::try_js_string(value)?.0);
    }
    Ok(JsString(result))
}
fn emit(sink: &ProgressSink, message: JsString) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| sink(&message)));
}
