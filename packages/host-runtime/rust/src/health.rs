//! Identity-bound service health and control files.
use crate::{IvyError, IvyJson, IvyResult, config::{InstanceConfig, FileError, CONFIGURATION_BYTES, read_json, js_environment}};
use ivy_contracts::{JsString, core_schema::validate_host};
use std::sync::Arc;
use tokio::sync::Mutex;

pub struct HealthFile {
    config: InstanceConfig,
    started_at: String,
    boot_id: String,
    launch_id: Option<JsString>,
    writes: Arc<Mutex<()>>,
}
fn now() -> String { chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true) }
impl HealthFile {
    pub fn new(config: InstanceConfig) -> Self {
        Self::with_launch_id(config, js_environment("IVY_LAUNCH_ID"))
    }
    pub fn with_launch_id(config: InstanceConfig, launch_id: Option<JsString>) -> Self {
        Self { config, started_at: now(), boot_id: uuid::Uuid::new_v4().to_string(), launch_id, writes: Arc::new(Mutex::new(())) }
    }
    pub fn boot_id(&self) -> &str { &self.boot_id }
    pub fn launch_id(&self) -> Option<&JsString> { self.launch_id.as_ref() }
    pub async fn write(&self, ready: bool, details: JsString, generation: Option<u64>) -> IvyResult<()> {
        let guard = self.writes.clone().lock_owned().await;
        let source = self.config.json();
        let mut value = IvyJson::object([
            ("schemaVersion", IvyJson::Number(1.0)),
            ("instanceId", source.required("instanceId")?.clone()),
            ("componentId", source.required("componentId")?.clone()),
            ("buildId", source.required("buildId")?.clone()),
            ("pid", IvyJson::Number(std::process::id() as f64)),
            ("bootId", self.boot_id.clone().into()),
            ("startedAt", self.started_at.clone().into()),
            ("observedAt", now().into()),
            ("ready", IvyJson::Bool(ready)),
            ("generation", generation.map(|v| IvyJson::Number(v as f64)).unwrap_or(IvyJson::Null)),
            ("details", IvyJson::String(JsString(details.0.into_iter().take(4096).collect()))),
        ]);
        if let Some(launch_id) = &self.launch_id { if !launch_id.0.is_empty() { value.insert("launchId", IvyJson::String(launch_id.clone())); } }
        validate_host("Health", &value)?;
        let path = self.config.data_root().join("health.json");
        // Keep serialization ownership in the worker even if its awaiting future is cancelled.
        tokio::task::spawn_blocking(move || {
            let _guard = guard;
            crate::config::write_json(&path, &value)
        }).await.map_err(|_| IvyError::internal())?
    }
    pub async fn control(&self) -> IvyResult<Option<IvyJson>> {
        let path = self.config.data_root().join("control.json");
        let instance = self.config.json().required("instanceId")?.clone();
        let boot = IvyJson::from(self.boot_id.clone());
        let launch = self.launch_id.clone().map(IvyJson::String);
        tokio::task::spawn_blocking(move || {
            let value = match read_json(&path, CONFIGURATION_BYTES) {
                Ok(value) => value,
                Err(FileError::Missing) => return Ok(None),
                Err(error) => return Err(error.error()),
            };
            validate_host("RuntimeControl", &value)?;
            Ok((value.get("instanceId") == Some(&instance) && value.get("bootId") == Some(&boot) && value.get("launchId") == launch.as_ref()).then_some(value))
        }).await.map_err(|_| IvyError::internal())?
    }
}
