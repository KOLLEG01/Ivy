//! Identity-bound service health and control files.
use crate::{IvyError, IvyJson, IvyResult, config::{InstanceConfig, FileError, CONFIGURATION_BYTES, read_json, js_environment}};
use ivy_contracts::{JsString, core_schema::validate_host};
use std::sync::Arc;
use tokio::sync::Mutex;
use ivy_sdk::{clock::{Clock, SystemClock, parse_date}, rpc::{ClientOptions, HiveClient, RequestOptions, RpcClient}};
use std::time::Duration;

fn require(condition: bool, code: &str, message: &str) -> IvyResult<()> {
    if condition { Ok(()) } else { Err(IvyError::new(code, message)) }
}

/// Use a local Hive listener without weakening the SDK's remote HTTPS rule.
pub fn local_hive_endpoint(config: &IvyJson, docker: Option<&IvyJson>) -> IvyResult<url::Url> {
    let settings = config.required("settings")?;
    validate_host("HiveSettings", settings)?;
    let mappings = docker.map(|docker| -> IvyResult<Vec<&IvyJson>> {
        let ports = docker.required("ports")?.as_array().ok_or_else(IvyError::internal)?;
        Ok(ports.iter().filter(|port| {
            port.get("containerPort") == settings.get("listenPort") &&
                port.get("host").and_then(|v| v.as_str().ok()).is_some_and(|v| ["127.0.0.1", "::1", "0.0.0.0", "::"].contains(&v.as_str()))
        }).collect())
    }).transpose()?;
    require(mappings.as_ref().is_none_or(|ports| ports.len() == 1), "invalid_arguments", "Hive health needs exactly one loopback-accessible host port mapping for its listener.")?;
    let binding = mappings.as_ref().and_then(|ports| ports.first()).copied();
    let host = binding.and_then(|p| p.get("host")).unwrap_or(settings.required("listenHost")?).as_str()?;
    let hostname = match host.as_str() {
        "0.0.0.0" => "127.0.0.1".into(), "::" => "[::1]".into(),
        _ if host.contains(':') => format!("[{host}]"), _ => host,
    };
    let mut endpoint = url::Url::parse(&config.required("publicBaseUrl")?.as_str()?).map_err(|_| IvyError::internal())?;
    // URL setters silently ignore invalid assignments, as in the original JS.
    let _ = endpoint.set_scheme("http");
    let _ = endpoint.set_host(Some(&hostname));
    let port = binding.and_then(|p| p.get("port")).unwrap_or(settings.required("listenPort")?);
    let port = port.as_f64().ok_or_else(IvyError::internal)? as u16;
    let _ = endpoint.set_port(Some(port));
    Ok(endpoint)
}

pub async fn check_health(config: &InstanceConfig, hive_endpoint: Option<&str>, verify_registration: bool) -> IvyResult<IvyJson> {
    check_health_with_clock(config, hive_endpoint, verify_registration, &SystemClock).await
}

/// The injected clock changes only time observation, never validation or I/O.
pub async fn check_health_with_clock(config: &InstanceConfig, hive_endpoint: Option<&str>, verify_registration: bool, clock: &dyn Clock) -> IvyResult<IvyJson> {
    let value = crate::config::json_file(config.data_root().join("health.json"), CONFIGURATION_BYTES).await?;
    validate_host("Health", &value)?;
    let source = config.json();
    let observed = parse_date(value.required("observedAt")?.as_js_string().ok_or_else(IvyError::internal)?);
    require(value.get("ready") == Some(&IvyJson::Bool(true)) && value.get("instanceId") == source.get("instanceId") &&
        value.get("buildId") == source.get("buildId") && clock.utc_millis() as f64 - observed < 15_000.0 && observed <= clock.utc_millis() as f64 + 1000.0,
        "service_not_ready", "Instance health is stale, mismatched or not ready.")?;
    let options = || RequestOptions { timeout: Duration::from_millis(5000), ..Default::default() };
    if source.get("componentId") == Some(&IvyJson::from("hive")) {
        let settings = source.required("settings")?;
        validate_host("HiveSettings", settings)?;
        let endpoint = match hive_endpoint.filter(|v| !v.is_empty()) {
            Some(value) => url::Url::parse(value).map_err(|_| IvyError::internal())?,
            None => local_hive_endpoint(source, None)?,
        };
        let token = settings.required("credentials")?.as_array().and_then(|v| v.first()).ok_or_else(IvyError::internal)?.required("token")?.as_str()?;
        let client = HiveClient::new(endpoint.as_str(), ClientOptions { credential: Some(token), ..Default::default() })?;
        let status = client.request("system.status", IvyJson::object(Vec::<(String, IvyJson)>::new()), options()).await?;
        require(status.get("ready") == Some(&IvyJson::Bool(true)) && status.get("buildId") == source.get("buildId"),
            "service_not_ready", "Hive API/storage identity is not ready.")?;
    } else if let Some(credential) = source.get("credential").filter(|v| verify_registration && v.as_js_string().is_some_and(|s| !s.0.is_empty())) {
        let client = HiveClient::new(&source.required("publicBaseUrl")?.as_str()?, ClientOptions { credential: Some(credential.as_str()?), ..Default::default() })?;
        let node = client.request("serviceNodes.get", IvyJson::object([("serviceNodeId", source.required("serviceNodeId")?.clone())]), options()).await?;
        require(["connected", "synced", "ready"].iter().all(|key| node.get(key) == Some(&IvyJson::Bool(true))) && node.get("buildId") == source.get("buildId"),
            "service_not_ready", "The current Hive service registration is not synchronized and ready.")?;
    }
    Ok(value)
}

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
        crate::memory::start_reclaiming();
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
