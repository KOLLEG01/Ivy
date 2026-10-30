//! Periodic health, retention, snapshots and credential-only configuration reload.
use super::{App, backup, failure, require, string};
use ivy_contracts::{IvyError, IvyJson, IvyResult, hash::digest, json::canonical};
use ivy_hive::{server_rpc::Storage, storage::RequestOptions, store::Credential};
use ivy_host_runtime::{
    config::{CONFIGURATION_BYTES, json_file},
    health::HealthFile,
};
use std::{
    collections::{BTreeMap, HashSet},
    future::Future,
    path::PathBuf,
    pin::Pin,
    sync::Arc,
    time::Duration,
};
use tokio::time::Instant;

pub(super) fn settings(config: &IvyJson) -> IvyResult<&IvyJson> {
    require(
        string(config, "componentId")? == "hive",
        "Hive requires its own instance configuration.",
    )?;
    let settings = config.required("settings")?;
    ivy_contracts::core_schema::validate_host("HiveSettings", settings)?;
    let directory = PathBuf::from(string(settings.required("backup")?, "directory")?);
    let artifact = PathBuf::from(string(config, "artifactRoot")?);
    require(
        directory.is_absolute()
            && !ivy_host_runtime::paths::inside(&artifact, &directory).map_err(failure)?,
        "Backup storage must be outside immutable artifacts.",
    )?;
    Ok(settings)
}
pub(super) fn credentials(settings: &IvyJson) -> IvyResult<Vec<Credential>> {
    let mut tokens = HashSet::new();
    let credentials = settings
        .required("credentials")?
        .as_array()
        .ok_or_else(IvyError::internal)?
        .iter()
        .map(|value| {
            let token = string(value, "token")?;
            require(
                !token.is_empty() && tokens.insert(token.clone()),
                "Hive needs distinct configured credentials.",
            )?;
            Ok(Credential {
                principal_id: value
                    .required("principalId")?
                    .as_js_string()
                    .cloned()
                    .ok_or_else(IvyError::internal)?,
                digest: digest(token).into(),
            })
        })
        .collect::<IvyResult<Vec<_>>>()?;
    require(
        !credentials.is_empty(),
        "Hive needs configured credentials.",
    )?;
    Ok(credentials)
}
fn fixed(config: &IvyJson) -> IvyResult<String> {
    let mut value = config.clone();
    let mut settings = value.required("settings")?.clone();
    settings.insert("credentials", IvyJson::Array(vec![]));
    value.insert("settings", settings);
    canonical(&value, CONFIGURATION_BYTES)
}
struct Backup {
    plan: backup::Plan,
    future: Pin<Box<dyn Future<Output = IvyResult<PathBuf>>>>,
}
pub(super) struct Maintenance {
    config_path: PathBuf,
    config: IvyJson,
    encoded: String,
    data: PathBuf,
    backup_settings: backup::Settings,
    next_backup: Instant,
    next_retention: Instant,
    backup: Option<Backup>,
    diagnostics: BTreeMap<&'static str, (bool, &'static str, Instant)>,
}
impl Maintenance {
    pub async fn new(config_path: PathBuf, config: IvyJson) -> IvyResult<Self> {
        let settings = settings(&config)?;
        let backup_settings = backup::Settings::new(settings.required("backup")?)?;
        let delay = backup_settings.prepare().await?;
        Ok(Self {
            data: PathBuf::from(string(&config, "dataRoot")?),
            encoded: canonical(&config, CONFIGURATION_BYTES)?,
            config_path,
            config,
            backup_settings,
            next_backup: Instant::now() + delay,
            next_retention: Instant::now(),
            backup: None,
            diagnostics: BTreeMap::new(),
        })
    }
    async fn diagnostic(
        &mut self,
        app: &App,
        code: &'static str,
        message: &'static str,
        resolved: bool,
    ) {
        if self
            .diagnostics
            .get(code)
            .is_some_and(|(old_resolved, old_message, sent)| {
                *old_resolved == resolved
                    && *old_message == message
                    && sent.elapsed() < Duration::from_secs(60)
            })
        {
            return;
        }
        let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let value = IvyJson::object([
            ("code", code.into()),
            ("message", message.into()),
            (
                "resource",
                IvyJson::object([(
                    "instanceId",
                    self.config
                        .get("instanceId")
                        .cloned()
                        .unwrap_or(IvyJson::Null),
                )]),
            ),
            ("source", "hive".into()),
            (
                "severity",
                if resolved {
                    "info"
                } else if code == "storage_pressure" {
                    "warning"
                } else {
                    "error"
                }
                .into(),
            ),
            (
                "status",
                if resolved { "resolved" } else { "current" }.into(),
            ),
            ("firstObservedAt", now.clone().into()),
            ("lastObservedAt", now.into()),
        ]);
        if Storage::request(
            app.server.worker.as_ref(),
            IvyJson::object([("action", "diagnostic".into()), ("value", value)]),
            RequestOptions {
                timeout_ms: 2000,
                terminate_on_timeout: false,
                ..Default::default()
            },
        )
        .await
        .is_ok()
        {
            self.diagnostics
                .insert(code, (resolved, message, Instant::now()));
        }
    }
    async fn reload(&mut self, app: &App) {
        let result = async {
            let next = json_file(self.config_path.clone(), CONFIGURATION_BYTES).await?;
            let encoded = canonical(&next, CONFIGURATION_BYTES)?;
            if encoded == self.encoded {
                return Ok(false);
            }
            ivy_contracts::core_schema::validate_host("InstanceConfig", &next)?;
            let settings = settings(&next)?;
            let credentials = credentials(settings)?;
            if fixed(&next)? != fixed(&self.config)? {
                return Err(IvyError::new(
                    "restart_required",
                    "Only Hive credentials reload without a deployment restart.",
                ));
            }
            app.server
                .network
                .lifecycle
                .set_credentials(credentials)
                .await?;
            self.config = next;
            self.encoded = encoded;
            Ok(true)
        }
        .await;
        match result {
            Ok(false) if self.diagnostics.get("configuration_reload_failed").is_some_and(|(resolved, _, _)| !resolved) => self.diagnostic(app, "configuration_reload_failed", "Configuration loaded successfully.", true).await,
            Ok(false) => {},
            Ok(true) => self.diagnostic(app, "configuration_reload_failed", "Configuration credentials reloaded.", true).await,
            Err(_) => self.diagnostic(app, "configuration_reload_failed", "Configuration reload was refused; the last valid configuration remains active.", false).await,
        }
    }
    async fn observe(&mut self, app: &Arc<App>, health: &HealthFile) -> IvyResult<()> {
        let ready = app.storage_ready(2000).await;
        health
            .write(
                ready && !app.reset_bootstrap,
                if ready { "" } else { "Storage is recovering." }.into(),
                None,
            )
            .await?;
        self.reload(app).await;
        let data = self.data.clone();
        let (available, total) = tokio::task::spawn_blocking(move || {
            ivy_host_runtime::storage_space::measure_bytes(&data)
        })
        .await
        .map_err(failure)?
        .map_err(failure)?;
        if available < 512 * 1024 * 1024 || u128::from(available) * 20 < u128::from(total) {
            self.diagnostic(
                app,
                "storage_pressure",
                "Available disk space is below the operating reserve; Hive continues processing requests.",
                false,
            )
            .await;
        } else {
            self.diagnostic(
                app,
                "storage_pressure",
                "Storage operating reserve is available.",
                true,
            )
            .await;
        }
        if ready
            && self.backup_settings.enabled
            && self.backup.is_none()
            && Instant::now() >= self.next_backup
        {
            let plan = backup::Plan::new(self.backup_settings.directory.clone());
            self.backup = Some(Backup {
                plan: plan.clone(),
                future: Box::pin(backup::snapshot(
                    app.server.worker.clone(),
                    self.data.clone(),
                    self.backup_settings.clone(),
                    plan,
                    string(&self.config, "buildId")?,
                )),
            });
        }
        if ready && Instant::now() >= self.next_retention {
            self.next_retention = Instant::now() + Duration::from_secs(15 * 60);
            let result = async {
                Storage::request(
                    app.server.worker.as_ref(),
                    IvyJson::object([("action", "retention.collect".into())]),
                    RequestOptions::default(),
                )
                .await?;
                app.server
                    .network
                    .lifecycle
                    .collect_package_artifacts()
                    .await?;
                if self.backup.is_none() {
                    backup::collect(
                        &self.backup_settings.directory,
                        self.backup_settings.enabled,
                    )
                    .await?;
                }
                Ok::<_, IvyError>(())
            }
            .await;
            if result.is_ok() {
                self.diagnostic(
                    app,
                    "retention_failed",
                    "Automatic retention collection completed.",
                    true,
                )
                .await;
            } else {
                self.diagnostic(
                    app,
                    "retention_failed",
                    "Automatic retention collection failed.",
                    false,
                )
                .await;
            }
        }
        Ok(())
    }
    pub async fn run(&mut self, app: Arc<App>, health: Arc<HealthFile>) {
        self.diagnostic(
            &app,
            "configuration_reload_failed",
            "Configuration loaded successfully.",
            true,
        )
        .await;
        let mut tick = tokio::time::interval(Duration::from_secs(2));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            let result = if let Some(backup) = self.backup.as_mut() {
                tokio::select! { result = &mut backup.future => Some(result), _ = tick.tick() => None }
            } else {
                tick.tick().await;
                None
            };
            if let Some(result) = result {
                self.backup = None;
                self.next_backup = Instant::now()
                    + if result.is_ok() {
                        self.backup_settings.interval
                    } else {
                        Duration::from_secs(300)
                    };
                if result.is_ok() {
                    self.diagnostic(
                        &app,
                        "backup_failed",
                        "Latest SQLite-safe backup completed.",
                        true,
                    )
                    .await;
                } else {
                    eprintln!("Hive SQLite-safe snapshot failed; previous snapshots are retained.");
                    self.diagnostic(
                        &app,
                        "backup_failed",
                        "SQLite-safe snapshot failed; inspect host storage and backup records.",
                        false,
                    )
                    .await;
                }
            } else if let Err(error) = self.observe(&app, &health).await {
                eprintln!("Hive health/configuration observation failed: {error:?}");
            }
        }
    }
    pub fn stop(&mut self) -> Option<backup::Plan> {
        self.backup.take().map(|backup| backup.plan)
    }
}
