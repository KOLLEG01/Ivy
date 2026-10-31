//! Native admission, writer shutdown and existing SSH phase dispatch. Hive clear
//! and authenticated UI bootstrap/publication remain with their owning modules.
use super::*;
use crate::{
    config,
    docker::DockerAdapter,
    exec_file, layout, native_path, node_path,
    systemd::{SystemdAdapter, systemd_unit_name},
};
use ivy_contracts::{core_schema::validate_host, json};
use ivy_sdk::clock::Clock;
use std::{path::PathBuf, sync::Arc, time::Duration};
mod dispatch;
mod phases;
pub use dispatch::{
    CoordinatedReset, ResetHostPhase, ResetPhaseArguments, decode_scope_roots, encode_scope_roots,
    reset_phase_arguments,
};
#[cfg(test)]
#[path = "../../../../../tests/rust/host-reset-release-cases.rs"]
mod release_cases;

fn internal<T, E>(value: Result<T, E>) -> IvyResult<T> {
    value.map_err(|_| IvyError::internal())
}
fn field(value: &J, key: &str) -> IvyResult<JsString> {
    value
        .required(key)?
        .as_js_string()
        .cloned()
        .ok_or_else(IvyError::internal)
}
fn instances(config: &J) -> IvyResult<&[J]> {
    config
        .required("instances")?
        .as_array()
        .ok_or_else(IvyError::internal)
}
fn number(value: &J) -> IvyResult<f64> {
    value.as_f64().ok_or_else(IvyError::internal)
}
fn require(ok: bool, code: &str, message: &str) -> IvyResult<()> {
    if ok {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}

struct Inner {
    distribution: PathBuf,
    job_launcher: PathBuf,
    clock: Arc<dyn Clock>,
    docker: DockerAdapter,
    systemd: SystemdAdapter,
}
#[derive(Clone)]
pub struct ResetHost(Arc<Inner>);
impl ResetHost {
    pub fn new(
        distribution: PathBuf,
        job_launcher: PathBuf,
        clock: Arc<dyn Clock>,
    ) -> IvyResult<Self> {
        Ok(Self(Arc::new(Inner {
            docker: DockerAdapter::new(distribution.clone())?,
            systemd: SystemdAdapter::new(distribution.clone()),
            distribution,
            job_launcher,
            clock,
        })))
    }
    pub async fn require_administrator(&self) -> IvyResult<()> {
        let inner = self.0.clone();
        internal(tokio::spawn(async move { inner.require_administrator().await }).await)?
    }
    /// Complete prepare only after real administrator admission, durable hold,
    /// writer shutdown and every ownership proof. A lost waiter cannot cancel it.
    pub async fn prepare(
        &self,
        config: J,
        plan: J,
        reset_id: JsString,
        runtime_epoch: JsString,
        publisher_digest: JsString,
    ) -> IvyResult<J> {
        let inner = self.0.clone();
        internal(
            tokio::spawn(async move {
                local::require_publisher_digest(&publisher_digest)?;
                inner.require_administrator().await?;
                let record = prepare_reset_hold(
                    config.clone(),
                    plan,
                    reset_id,
                    runtime_epoch,
                    publisher_digest,
                    inner.clock.clone(),
                )
                .await?;
                inner.stop(config).await?;
                Ok(record)
            })
            .await,
        )?
    }
    pub async fn complete(
        &self,
        config: J,
        reset_id: JsString,
        runtime_epoch: JsString,
    ) -> IvyResult<J> {
        let inner = self.0.clone();
        internal(
            tokio::spawn(async move {
                inner.require_administrator().await?;
                complete_reset_record(config, reset_id, runtime_epoch, inner.clock.clone()).await
            })
            .await,
        )?
    }
    pub async fn confirm(
        &self,
        config: J,
        reset_id: JsString,
        runtime_epoch: JsString,
    ) -> IvyResult<J> {
        let inner = self.0.clone();
        internal(
            tokio::spawn(async move {
                inner.require_administrator().await?;
                confirm_reset_record(config, reset_id, runtime_epoch, inner.clock.clone()).await
            })
            .await,
        )?
    }
    /// Admitted shutdown stage, also used when a release must recover its hold.
    pub async fn stop(&self, config: J) -> IvyResult<()> {
        let inner = self.0.clone();
        internal(tokio::spawn(async move { inner.stop(config).await }).await)?
    }
    pub async fn send_stop(&self, config: J, instance: J) -> IvyResult<Option<f64>> {
        let inner = self.0.clone();
        internal(tokio::spawn(async move { inner.send_stop(&config, &instance).await }).await)?
    }
    /// Use the existing runtime-reset-host CLI argv and response envelope. No
    /// network protocol, retry ID or fallback transport is introduced here.
    pub async fn remote(&self, config: J, host: J, args: Vec<JsString>) -> IvyResult<Option<J>> {
        let inner = self.0.clone();
        internal(tokio::spawn(async move { inner.remote(config, host, args).await }).await)?
    }
}
impl Inner {
    fn options(&self) -> exec_file::FileOptions {
        exec_file::FileOptions {
            job_launcher: Some(self.job_launcher.clone()),
            ..Default::default()
        }
    }
    async fn require_administrator(&self) -> IvyResult<()> {
        #[cfg(not(windows))]
        {
            #[cfg(unix)]
            let administrator = unsafe { libc::getuid() } == 0;
            #[cfg(not(unix))]
            let administrator = false;
            require(
                administrator,
                "access_denied",
                "Runtime reset requires the local OS administrator.",
            )
        }
        #[cfg(windows)]
        {
            let command=exec_file::FileCommand{
                executable:"powershell.exe".into(),
                args:["-NoProfile","-NonInteractive","-Command","[Security.Principal.WindowsPrincipal]::new([Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)"].into_iter().map(Into::into).collect(),
                cwd:None,timeout_ms:0,maximum_buffer_bytes:65536,
            };
            let result = exec_file::exec_file_without_timeout(
                command,
                self.distribution.clone(),
                self.options(),
            )
            .await;
            let administrator = result
                .ok()
                .filter(|v| v.failure.is_none())
                .is_some_and(|v| {
                    node_path::lowercase(&crate::linux_host::trimmed(&v.stdout))
                        == JsString::from("true")
                });
            require(
                administrator,
                "access_denied",
                "Runtime reset requires an elevated Windows administrator token.",
            )
        }
    }
    async fn send_stop(&self, config: &J, instance: &J) -> IvyResult<Option<f64>> {
        let data = config::path_value(layout::service_paths(config, instance)?.required("data")?)?;
        let result: IvyResult<Option<f64>> = async {
            let path = native_path::lexical(&data.join("health.json"));
            let health = config::json_file(path, config::CONFIGURATION_BYTES).await?;
            validate_host("Health", &health)?;
            if health.get("instanceId") != instance.get("instanceId") {
                return Ok(None);
            }
            let pid = number(health.required("pid")?)?;
            if health
                .get("launchId")
                .and_then(J::as_js_string)
                .is_some_and(|text| !text.0.is_empty())
            {
                let control = J::object([
                    ("schemaVersion", J::Number(1.0)),
                    ("instanceId", instance.required("instanceId")?.clone()),
                    ("launchId", health.required("launchId")?.clone()),
                    ("bootId", health.required("bootId")?.clone()),
                    ("action", "shutdown".into()),
                    ("requestedAt", local::now(self.clock.as_ref())?),
                ]);
                let path = native_path::lexical(&data.join("control.json"));
                config::atomic_json(path, control).await?;
            } else if !signal_pid(pid, true) {
                return Ok(None);
            }
            Ok(Some(pid))
        }
        .await;
        Ok(result.unwrap_or(None))
    }
    async fn stop(self: Arc<Self>, config: J) -> IvyResult<()> {
        if cfg!(target_os = "linux") {
            for instance in instances(&config)?.iter().rev() {
                let id = field(instance, "instanceId")?;
                if instance.get("engine") == Some(&"docker".into()) {
                    self.docker
                        .stop_container(config.clone(), id, 30000.0, None)
                        .await?;
                } else {
                    self.systemd
                        .stop_unit(
                            config.clone(),
                            systemd_unit_name(&config, &id)?.into(),
                            crate::runtime_owner::stop::StopPolicy::Deadline(Duration::from_secs(
                                30,
                            )),
                        )
                        .await?;
                }
            }
        } else {
            let mut ordered = instances(&config)?.to_vec();
            ordered.sort_by_key(|instance| {
                ["service-manager", "host-executor"]
                    .iter()
                    .any(|c| instance.get("componentId") == Some(&J::from(*c)))
            });
            // Admit every call before awaiting Promise.all's first rejection.
            // Dropped join handles retain the independently owned writes.
            let pending = ordered
                .into_iter()
                .map(|instance| {
                    let inner = self.clone();
                    let config = config.clone();
                    tokio::spawn(async move { inner.send_stop(&config, &instance).await })
                })
                .collect::<Vec<_>>();
            let pids = futures_util::future::try_join_all(
                pending
                    .into_iter()
                    .map(|work| async move { internal(work.await)? }),
            )
            .await?
            .into_iter()
            .flatten()
            .collect();
            wait_reset_processes_stopped(pids, self.clock.clone()).await?;
        }
        prove_reset_locks_released(config, self.clock.clone()).await
    }
    async fn remote(&self, config: J, host: J, args: Vec<JsString>) -> IvyResult<Option<J>> {
        let ssh = config
            .required("executables")?
            .get("ssh")
            .filter(|v| !v.is_null())
            .map(|v| v.as_js_string().cloned().ok_or_else(IvyError::internal))
            .transpose()?
            .unwrap_or_else(|| "ssh".into());
        let mut argv = vec![
            field(&host, "ssh")?,
            field(&host, "cliPath")?,
            "runtime-reset-host".into(),
            "--config".into(),
            field(&host, "configPath")?,
        ];
        argv.extend(args);
        argv.push("--json".into());
        let result = exec_file::exec_file(
            exec_file::FileCommand {
                executable: node_path::to_native(&ssh).into_os_string(),
                args: argv
                    .iter()
                    .map(|v| node_path::to_native(v).into_os_string())
                    .collect(),
                cwd: None,
                timeout_ms: 180000,
                maximum_buffer_bytes: 4 * 1024 * 1024,
            },
            self.distribution.clone(),
            self.options(),
        )
        .await
        .map_err(|_| IvyError::internal())?;
        if result.failure.is_some() {
            return Err(IvyError::internal());
        }
        let value = internal(json::parse(&result.stdout.to_utf8()?, usize::MAX))?;
        if value.get("ok") != Some(&J::Bool(true)) {
            let code = value
                .get("code")
                .filter(|v| !v.is_null())
                .map(|v| v.as_js_string().cloned().ok_or_else(IvyError::internal))
                .transpose()?
                .unwrap_or_else(|| "remote_reset_failed".into());
            return Err(IvyError::new_js(
                code,
                "Remote runtime reset phase failed.".into(),
            ));
        }
        Ok(value.get("data").cloned())
    }
}

fn signal_pid(pid: f64, terminate: bool) -> bool {
    if !pid.is_finite() || pid.fract() != 0.0 || pid < i32::MIN as f64 || pid > i32::MAX as f64 {
        return false;
    }
    let pid = pid as i32;
    #[cfg(unix)]
    {
        unsafe { libc::kill(pid, if terminate { libc::SIGTERM } else { 0 }) == 0 }
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::{
            Foundation::{CloseHandle, STILL_ACTIVE, WAIT_TIMEOUT},
            System::Threading::{
                GetCurrentProcess, GetExitCodeProcess, OpenProcess, PROCESS_QUERY_INFORMATION,
                PROCESS_SYNCHRONIZE, PROCESS_TERMINATE, TerminateProcess, WaitForSingleObject,
            },
        };
        let handle = if pid == 0 {
            unsafe { GetCurrentProcess() }
        } else {
            unsafe {
                OpenProcess(
                    PROCESS_TERMINATE | PROCESS_QUERY_INFORMATION | PROCESS_SYNCHRONIZE,
                    0,
                    pid as u32,
                )
            }
        };
        if handle.is_null() {
            return false;
        }
        let ok = if terminate {
            unsafe { TerminateProcess(handle, 1) != 0 }
        } else {
            let mut status = 0;
            unsafe {
                GetExitCodeProcess(handle, &mut status) != 0
                    && status == STILL_ACTIVE as u32
                    && WaitForSingleObject(handle, 0) == WAIT_TIMEOUT
            }
        };
        if pid != 0 {
            unsafe {
                CloseHandle(handle);
            }
        }
        ok
    }
    #[cfg(not(any(windows, unix)))]
    {
        let _ = (pid, terminate);
        false
    }
}
/// Match the original process.kill(pid,0) probe, including its caught errors.
/// This probe alone is never the final reset ownership proof.
pub fn reset_process_alive(pid: f64) -> bool {
    signal_pid(pid, false)
}
pub async fn wait_reset_processes_stopped(pids: Vec<f64>, clock: Arc<dyn Clock>) -> IvyResult<()> {
    internal(
        tokio::spawn(async move {
            let end = clock.utc_millis() + 60000;
            while pids.iter().copied().any(reset_process_alive) && clock.utc_millis() < end {
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
            require(
                !pids.iter().copied().any(reset_process_alive),
                "outcome_unknown",
                "Not every reset writer established a stopped process.",
            )
        })
        .await,
    )?
}
