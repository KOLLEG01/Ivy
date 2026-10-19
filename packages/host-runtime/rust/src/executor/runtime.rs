use super::*;
use crate::{
    config::InstanceConfig,
    docker::{DockerAdapter, ManagedContainer},
    health, linux_resources, node_path,
    prepare::PreparationRuntime,
    process::{self, LaunchContext, ProcessResult},
    systemd::{SystemdAdapter, SystemdState},
};
use ivy_contracts::core_schema::validate_host;
use std::{collections::BTreeMap, ffi::OsString, path::PathBuf, sync::Mutex, time::Duration};
use tokio::sync::Mutex as AsyncMutex;

mod compatibility;
mod containers;
mod inspection;
mod units;
pub use inspection::NativeOfflineHiveInspector;

/// The Hive owner supplies its qualified offline entrypoint. The host never
/// substitutes a database reader or an older backend when it is unavailable.
pub struct OfflineInspection {
    pub configuration_path: PathBuf,
    pub artifact_root: PathBuf,
    pub contracts: IvyJson,
    pub host: IvyJson,
}
pub trait OfflineHiveInspector: Send + Sync {
    fn inspect(&self, request: OfflineInspection) -> ExecutorFuture<'_, IvyJson>;
}

struct Container {
    target: IvyJson,
    process: ManagedContainer,
    manifest: IvyJson,
    unready_since: f64,
    readiness_checked: bool,
}
struct Unit {
    target: IvyJson,
    unready_since: f64,
    readiness_checked: bool,
}
#[derive(Default)]
struct Handles {
    container: Option<Container>,
    unit: Option<Unit>,
}
#[derive(Default)]
struct Resources {
    loaded: bool,
    value: Option<IvyJson>,
}

/// Production host effects for durable activation. Each instance has its own
/// observation lock; an ordinary lane never borrows another lane's config.
pub struct NativeActivationRuntime {
    bootstrap_root: PathBuf,
    preparation: PreparationRuntime,
    job_launcher: PathBuf,
    platform: String,
    docker: DockerAdapter,
    systemd: SystemdAdapter,
    inspector: Arc<dyn OfflineHiveInspector>,
    handles: Mutex<BTreeMap<String, Arc<AsyncMutex<Handles>>>>,
    resources: AsyncMutex<Resources>,
    boot_id: String,
}
impl NativeActivationRuntime {
    pub fn new(
        bootstrap_root: PathBuf,
        preparation: PreparationRuntime,
        job_launcher: PathBuf,
        inspector: Arc<dyn OfflineHiveInspector>,
    ) -> IvyResult<Self> {
        let docker = DockerAdapter::new(bootstrap_root.clone())?;
        let systemd = SystemdAdapter::new(bootstrap_root.clone());
        Ok(Self::with_adapters(
            bootstrap_root,
            preparation,
            job_launcher,
            inspector,
            if cfg!(windows) {
                "win32"
            } else {
                std::env::consts::OS
            }
            .into(),
            docker,
            systemd,
        ))
    }
    /// Explicit OS adapters also support isolated platform-policy qualification.
    /// Native construction always supplies the actual host platform above.
    pub fn with_adapters(
        bootstrap_root: PathBuf,
        preparation: PreparationRuntime,
        job_launcher: PathBuf,
        inspector: Arc<dyn OfflineHiveInspector>,
        platform: String,
        docker: DockerAdapter,
        systemd: SystemdAdapter,
    ) -> Self {
        Self {
            bootstrap_root,
            preparation,
            job_launcher,
            inspector,
            platform,
            docker,
            systemd,
            handles: Mutex::new(BTreeMap::new()),
            resources: AsyncMutex::new(Resources::default()),
            boot_id: uuid::Uuid::new_v4().to_string(),
        }
    }
    fn handles(&self, id: &str) -> IvyResult<Arc<AsyncMutex<Handles>>> {
        Ok(self
            .handles
            .lock()
            .map_err(|_| IvyError::internal())?
            .entry(id.to_owned())
            .or_default()
            .clone())
    }
    async fn resource_scope(&self, c: &OperationContext) -> IvyResult<Option<IvyJson>> {
        let mut state = self.resources.lock().await;
        if state.loaded {
            return Ok(state.value.clone());
        }
        state.loaded = true;
        if self.platform != "linux" {
            return Ok(None);
        }
        let identity = linux_resources::installation_identity(&c.config)?;
        let path = config::path_value(c.config.required("runtimeRoot")?)?
            .join("bootstrap")
            .join(format!("{identity}.json"));
        let Some(value) = config::json_file_if_present(path, config::CONFIGURATION_BYTES).await?
        else {
            return Ok(None);
        };
        validate_host("BootstrapPlan", &value)?;
        require(
            value.get("installationId") == Some(&IvyJson::from(identity.clone()))
                && value.get("hostId") == c.config.get("hostId")
                && value.get("runtimeRoot") == c.config.get("runtimeRoot"),
            "target_conflict",
            "The retained Linux bootstrap plan belongs to another installation.",
        )?;
        state.value = selected(&value, "linuxResources").cloned();
        if let Some(scope) = &state.value {
            linux_resources::validate_linux_resource_scope(scope, Some(&identity))?;
        }
        Ok(state.value.clone())
    }
    async fn observation(
        &self,
        c: &OperationContext,
        target: Option<&IvyJson>,
        state: &str,
        code: Option<JsString>,
        message: JsString,
        health: Option<IvyJson>,
        build: Option<IvyJson>,
        systemd: Option<&SystemdState>,
    ) -> IvyResult<()> {
        let revision = target
            .and_then(|t| t.get("revision"))
            .cloned()
            .unwrap_or(IvyJson::Null);
        let owner_boot = if systemd.is_some() {
            health
                .as_ref()
                .and_then(|h| selected(h, "bootId"))
                .cloned()
                .or_else(|| (!revision.is_null()).then(|| revision.clone()))
                .unwrap_or_else(|| self.boot_id.clone().into())
        } else {
            self.boot_id.clone().into()
        };
        c.journal
            .record_observation(
                &c.instance_id,
                IvyJson::object([
                    ("schemaVersion", IvyJson::Number(1.0)),
                    ("instanceId", c.instance_id.clone().into()),
                    (
                        "ownerPid",
                        IvyJson::Number(
                            systemd
                                .and_then(|s| s.main_pid)
                                .unwrap_or(std::process::id() as u64)
                                as f64,
                        ),
                    ),
                    ("ownerBootId", owner_boot),
                    ("observedAt", stamp(c.clock.as_ref())?),
                    ("targetRevision", revision),
                    (
                        "candidateId",
                        target
                            .and_then(|t| t.get("candidateId"))
                            .cloned()
                            .unwrap_or(IvyJson::Null),
                    ),
                    ("buildId", build.unwrap_or(IvyJson::Null)),
                    ("state", state.into()),
                    ("health", health.unwrap_or(IvyJson::Null)),
                    ("restartCount", IvyJson::Number(0.0)),
                    ("nextRestartAt", IvyJson::Null),
                    ("code", code.map(IvyJson::String).unwrap_or(IvyJson::Null)),
                    (
                        "message",
                        IvyJson::String(JsString(message.0.into_iter().take(4096).collect())),
                    ),
                ]),
            )
            .await
    }
    async fn run_command(
        &self,
        c: &OperationContext,
        command: &IvyJson,
        root: PathBuf,
        environment: BTreeMap<OsString, OsString>,
    ) -> IvyResult<Arc<ProcessResult>> {
        process::run_command(
            &command_value(command)?,
            &root,
            &executables(&c.config)?,
            LaunchContext {
                environment,
                job_launcher: Some(self.job_launcher.clone()),
                ..Default::default()
            },
            None,
        )
        .await
    }
    async fn instance(&self, target: &IvyJson) -> IvyResult<InstanceConfig> {
        config::instance_config(
            config::path_value(target.required("configPath")?)?,
            &self.preparation.node,
        )
        .await
    }
}
impl ActivationRuntime for NativeActivationRuntime {
    fn prepare<'a>(
        &'a self,
        c: &'a OperationContext,
        snapshot: &'a IvyJson,
        component: &'a str,
    ) -> ExecutorFuture<'a, IvyJson> {
        Box::pin(async move {
            let plan = crate::source::source_build_plan(
                config::path_value(snapshot.required("sourceRoot")?)?,
                component,
            )
            .await?;
            self.service_storage(c, &plan).await?;
            crate::prepare::prepare_candidate(
                snapshot.clone(),
                component.into(),
                c.config.clone(),
                self.bootstrap_root.clone(),
                self.preparation.clone(),
            )
            .await
        })
    }
    fn compatibility<'a>(
        &'a self,
        c: &'a OperationContext,
        candidate: &'a str,
        settings: Option<IvyJson>,
    ) -> ExecutorFuture<'a, ()> {
        Box::pin(self.check_compatibility(c, candidate, settings))
    }
    fn reconcile<'a>(
        &'a self,
        c: &'a OperationContext,
        target: Option<IvyJson>,
        allow_start: bool,
    ) -> ExecutorFuture<'a, ()> {
        Box::pin(async move {
            let instance = c.journal.instance(&c.instance_id).await?;
            if instance.get("engine") == Some(&"docker".into()) {
                let slot = self.handles(&c.instance_id)?;
                let mut handles = slot.lock().await;
                self.reconcile_container(c, &instance, target.as_ref(), allow_start, &mut handles)
                    .await
            } else if self.platform == "linux"
                && instance.get("componentId") != Some(&"host-executor".into())
            {
                let slot = self.handles(&c.instance_id)?;
                let mut handles = slot.lock().await;
                self.reconcile_unit(c, &instance, target.as_ref(), allow_start, &mut handles)
                    .await
            } else {
                Ok(())
            }
        })
    }
    fn dispose(&self) -> ExecutorFuture<'_, ()> {
        Box::pin(async move {
            let slots = self
                .handles
                .lock()
                .map_err(|_| IvyError::internal())?
                .values()
                .cloned()
                .collect::<Vec<_>>();
            for slot in slots {
                let mut h = slot.lock().await;
                if let Some(container) = h.container.take() {
                    container.process.dispose()
                }
                h.unit = None;
            }
            Ok(())
        })
    }
}
fn executables(config: &IvyJson) -> IvyResult<BTreeMap<String, PathBuf>> {
    json::object_entries(config.required("executables")?)
        .into_iter()
        .map(|(key, value)| Ok((key.to_utf8()?, config::path_value(value)?)))
        .collect()
}
fn command_value(value: &IvyJson) -> IvyResult<process::Command> {
    let timeout = value
        .required("timeoutMs")?
        .as_f64()
        .ok_or_else(IvyError::internal)?;
    Ok(process::Command {
        executable: text(value, "executable")?,
        args: value
            .required("args")?
            .as_array()
            .ok_or_else(IvyError::internal)?
            .iter()
            .map(|v| {
                Ok(
                    node_path::to_native(v.as_js_string().ok_or_else(IvyError::internal)?)
                        .into_os_string(),
                )
            })
            .collect::<IvyResult<_>>()?,
        cwd: value.get("cwd").map(config::path_value).transpose()?,
        timeout_ms: if timeout.is_finite() && (1.0..2147483648.0).contains(&timeout) {
            timeout as u64
        } else {
            1
        },
    })
}
fn truthy(value: &IvyJson) -> bool {
    match value {
        IvyJson::Null => false,
        IvyJson::Bool(v) => *v,
        IvyJson::Number(v) => *v != 0.0 && !v.is_nan(),
        IvyJson::String(v) => !v.0.is_empty(),
        _ => true,
    }
}
fn timeout(manifest: &IvyJson, kind: &str, fallback: f64) -> f64 {
    field(manifest, kind)
        .get("timeoutMs")
        .and_then(IvyJson::as_f64)
        .unwrap_or(fallback)
}
fn env(
    config_path: &IvyJson,
    revision: Option<&IvyJson>,
    endpoint: Option<&str>,
) -> IvyResult<BTreeMap<OsString, OsString>> {
    let mut value = BTreeMap::from([(
        "IVY_INSTANCE_CONFIG".into(),
        node_path::to_native(config_path.as_js_string().ok_or_else(IvyError::internal)?)
            .into_os_string(),
    )]);
    if let Some(revision) = revision {
        value.insert(
            "IVY_LAUNCH_ID".into(),
            node_path::to_native(revision.as_js_string().ok_or_else(IvyError::internal)?)
                .into_os_string(),
        );
    }
    if let Some(endpoint) = endpoint {
        value.insert("IVY_HIVE_HEALTH_URL".into(), endpoint.into());
    }
    Ok(value)
}
