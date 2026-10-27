//! Service lifetime over the existing journal, native, call and Voice owners.
use crate::{
    admission::PhoneAdmission,
    call_commands::PhoneCallCommands,
    call_logs::PhoneCallLogs,
    diagnostics::PhoneDiagnostics,
    journal::{PhoneJournal, PhoneJournalLimits, PhoneJournalOptions, PhoneJournalOwner},
    native_client::validate_phone,
    service_bridge::PhoneBridge,
    service_flow::{PhoneFlow, PhoneFlowVoice},
    service_process::{PhoneProcess, PhoneProcessOptions, start_phone_process},
    service_registry::{PHONE_TOOLS, phone_registry},
    service_settings::{phone_credentials, phone_settings},
    voice_rpc::PhoneCodexVoice,
};
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString};
use ivy_host_runtime::{
    codex_home::HomeContext, config::InstanceConfig, health::HealthFile, node_path,
};
use ivy_sdk::service::{InvocationContext, ServiceClient, ServiceOptions, ToolHandler};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::{
    sync::{oneshot, watch},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

fn field<'a>(v: &'a IvyJson, name: &str) -> IvyResult<&'a IvyJson> {
    v.get(name).ok_or_else(IvyError::internal)
}
fn text(v: &IvyJson, name: &str) -> IvyResult<JsString> {
    field(v, name)?
        .as_js_string()
        .cloned()
        .ok_or_else(IvyError::internal)
}
fn optional_text(v: &IvyJson, name: &str) -> Option<JsString> {
    v.get(name).and_then(IvyJson::as_js_string).cloned()
}
fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}
fn unavailable() -> IvyError {
    IvyError::new(
        "service_not_ready",
        "Phone's original native owner or Hive generation is unavailable.",
    )
}
fn event(name: &str, code: Option<&str>) {
    let mut value = serde_json::json!({"event": name});
    if let Some(code) = code {
        value["code"] = code.into();
    }
    eprintln!("{value}");
}

/// Retain the executing account across rotations; never substitute service data
/// for an account home. No Codex process is started by this snapshot.
struct Account {
    paths: node_path::Context,
    environment: BTreeMap<JsString, JsString>,
    home: JsString,
}
impl Account {
    fn current() -> IvyResult<Self> {
        let home = std::env::home_dir()
            .filter(|p| p.is_absolute())
            .ok_or_else(|| {
                IvyError::new(
                    "invalid_arguments",
                    "The executing account home is unavailable.",
                )
            })?;
        Ok(Self {
            paths: node_path::Context::current().map_err(|_| IvyError::internal())?,
            environment: std::env::vars_os()
                .map(|(key, value)| {
                    (
                        node_path::from_native(Path::new(&key)),
                        node_path::from_native(Path::new(&value)),
                    )
                })
                .collect(),
            home: node_path::from_native(&home),
        })
    }
    fn context(&self) -> HomeContext<'_> {
        HomeContext {
            paths: &self.paths,
            environment: &self.environment,
            user_home: Some(&self.home),
            cwd: &self.paths.cwd,
            account_home: &self.home,
        }
    }
}

/// The offline operator tools use the same configuration admission as startup.
/// No service, native companion, account connection or audio device is opened.
pub fn validate_current_settings(value: &IvyJson) -> IvyResult<IvyJson> {
    let account = Account::current()?;
    phone_settings(value, &account.context())
}
#[derive(Clone)]
struct Runtime {
    process: PhoneProcess,
    flow: PhoneFlow,
    bridge: PhoneBridge,
}
struct Inner {
    config: InstanceConfig,
    settings: IvyJson,
    account: Account,
    health: HealthFile,
    health_gate: tokio::sync::Mutex<()>,
    journal: PhoneJournal,
    logs: PhoneCallLogs,
    diagnostics: Mutex<PhoneDiagnostics>,
    runtime: Mutex<Option<Runtime>>,
    initialize_gate: tokio::sync::Mutex<()>,
    service: Mutex<Option<ServiceClient>>,
    stopped: AtomicBool,
    rotating: AtomicBool,
    stop: CancellationToken,
    poll: Mutex<Option<JoinHandle<()>>>,
    voice: Mutex<Option<JoinHandle<()>>>,
    last_voice_check: Mutex<Option<Instant>>,
    closing: Mutex<Option<PhoneServiceClose>>,
    failure: Mutex<Option<IvyError>>,
}
struct Lease(Arc<Inner>);
impl Drop for Lease {
    fn drop(&mut self) {
        self.0.close();
    }
}
/// The final handle owns stop authority. Requests and task observers do not.
#[derive(Clone)]
pub struct PhoneService(Arc<Lease>);
#[derive(Clone)]
pub struct PhoneServiceClose(watch::Receiver<Option<IvyResult<()>>>);
impl PhoneServiceClose {
    pub async fn wait(&self) -> IvyResult<()> {
        let mut receiver = self.0.clone();
        loop {
            if let Some(result) = receiver.borrow_and_update().clone() {
                return result;
            }
            receiver.changed().await.map_err(|_| IvyError::internal())?;
        }
    }
}
impl PhoneService {
    /// Startup remains owned if its observer disappears. A late delivered handle
    /// is dropped and performs the same retained stop as an ordinary owner.
    pub async fn start(config: InstanceConfig) -> IvyResult<Self> {
        crate::tls::initialize();
        let (send, receive) = oneshot::channel();
        tokio::spawn(async move {
            let _ = send.send(Self::start_owned(config).await);
        });
        receive.await.map_err(|_| IvyError::internal())?
    }
    async fn start_owned(config: InstanceConfig) -> IvyResult<Self> {
        let source = config.json();
        if source.get("componentId") != Some(&"phone-bridge".into())
            || optional_text(source, "credential").is_none_or(|s| s.0.is_empty())
        {
            return Err(IvyError::new(
                "invalid_arguments",
                "PhoneBridge requires its own identity and Hive credential.",
            ));
        }
        let account = Account::current()?;
        let settings = phone_settings(field(source, "settings")?, &account.context())?;
        let journal = PhoneJournal::open(PhoneJournalOptions {
            data_root: config.data_root().into(),
            owner: PhoneJournalOwner {
                host_id: text(source, "hostId")?,
                service_node_id: text(source, "serviceNodeId")?,
            },
            limits: PhoneJournalLimits::default(),
            max_concurrent_calls: settings
                .get("binding")
                .and_then(|v| v.get("maxConcurrentCalls"))
                .and_then(IvyJson::as_f64)
                .unwrap_or(1.0) as usize,
        })
        .await?;
        let logs = match PhoneCallLogs::open(config.data_root()) {
            Ok(logs) => logs,
            Err(error) => {
                journal.close().wait().await?;
                return Err(error);
            }
        };
        let inner = Arc::new(Inner {
            health: HealthFile::new(config.clone()),
            diagnostics: Mutex::new(PhoneDiagnostics::new(
                field(source, "serviceNodeId")?.as_str()?,
            )),
            config,
            settings,
            account,
            journal,
            logs,
            health_gate: tokio::sync::Mutex::new(()),
            initialize_gate: tokio::sync::Mutex::new(()),
            runtime: Mutex::new(None),
            service: Mutex::new(None),
            stopped: AtomicBool::new(false),
            rotating: AtomicBool::new(false),
            stop: CancellationToken::new(),
            poll: Mutex::new(None),
            voice: Mutex::new(None),
            last_voice_check: Mutex::new(None),
            closing: Mutex::new(None),
            failure: Mutex::new(None),
        });
        let owner = Self(Arc::new(Lease(inner.clone())));
        let started = async {
            inner
                .write_health(false, None, "Starting the exclusive native Phone owner.")
                .await?;
            inner.initialize().await?;
            let service = inner.service_client()?;
            *inner.service.lock().unwrap() = Some(service.clone());
            service.start();
            let poll = inner.clone();
            *inner.poll.lock().unwrap() = Some(tokio::spawn(async move {
                poll.poll().await;
            }));
            inner.check_voice();
            Ok::<_, IvyError>(())
        }
        .await;
        if let Err(error) = started {
            owner.close().wait().await?;
            return Err(error);
        }
        Ok(owner)
    }
    pub fn close(&self) -> PhoneServiceClose {
        self.0.0.close()
    }
    pub fn journal(&self) -> &PhoneJournal {
        &self.0.0.journal
    }
    pub fn service(&self) -> Option<ServiceClient> {
        self.0.0.service.lock().unwrap().clone()
    }
    pub async fn wait_closed(&self) -> IvyResult<()> {
        self.0.0.stop.cancelled().await;
        self.close().wait().await
    }
}
impl Inner {
    fn current(&self) -> IvyResult<Runtime> {
        self.runtime.lock().unwrap().clone().ok_or_else(unavailable)
    }
    async fn local(&self) -> IvyResult<Runtime> {
        let current = self.current()?;
        if self.stopped.load(Ordering::Acquire)
            || self.rotating.load(Ordering::Acquire)
            || current.process.client.is_closed()
            || self.journal.epoch().await?.as_ref() != Some(&current.process.epoch)
        {
            return Err(unavailable());
        }
        Ok(current)
    }
    async fn write_health(
        &self,
        ready: bool,
        generation: Option<u64>,
        details: &str,
    ) -> IvyResult<()> {
        let _gate = self.health_gate.lock().await;
        if self.stopped.load(Ordering::Acquire) {
            return Ok(());
        }
        let archive = self.journal.archive_status().await?;
        let usage = self.journal.status().await?;
        let warning = usage
            .get("operations")
            .and_then(IvyJson::as_f64)
            .unwrap_or(0.0)
            >= self.journal.limits().max_operations as f64 * 0.8
            || usage
                .get("reservedBytes")
                .and_then(IvyJson::as_f64)
                .unwrap_or(0.0)
                >= self.journal.limits().max_bytes as f64 * 0.8;
        let detail = format!(
            "{details} Receipt archive: {} records, {} bytes.{}",
            field(&archive, "records")?.as_f64().unwrap_or(0.0),
            field(&archive, "bytes")?.as_f64().unwrap_or(0.0),
            if warning {
                " Active journal near capacity: investigate unknown calls; never delete replay evidence."
            } else {
                ""
            }
        );
        self.health.write(ready, detail.into(), generation).await
    }
    async fn initialize(self: &Arc<Self>) -> IvyResult<()> {
        let _gate = self.initialize_gate.lock().await;
        if self.stopped.load(Ordering::Acquire) {
            return Err(unavailable());
        }
        let native_settings = field(&self.settings, "native")?;
        let process = start_phone_process(PhoneProcessOptions {
            artifact_root: self.config.artifact_root().into(),
            executable: field(native_settings, "executable")?.as_str()?.into(),
            executable_hash: field(native_settings, "executableHash")?.as_str()?,
            journal: self.journal.clone(),
        })
        .await?;
        let initialized = async {
            let credentials =
                phone_credentials(optional_text(&self.settings, "credentialsPath")).await?;
            let policy = field(&self.settings, "policy")?;
            let peers: BTreeSet<_> = ["incoming", "challengedIncoming"]
                .iter()
                .flat_map(|key| policy.get(key).and_then(IvyJson::as_array).unwrap_or(&[]))
                .filter_map(|item| optional_text(item, "peerAddress"))
                .collect();
            let registration = field(&self.settings, "registration")?.clone();
            let configuration = IvyJson::object([
                ("binding", field(&self.settings, "binding")?.clone()),
                ("codecs", field(&self.settings, "codecs")?.clone()),
                (
                    "incomingPeers",
                    IvyJson::Array(peers.into_iter().map(Into::into).collect()),
                ),
                (
                    "password",
                    if registration.is_null() {
                        IvyJson::Null
                    } else {
                        field(&credentials, "password")?.clone()
                    },
                ),
                ("registration", registration),
            ]);
            validate_phone("Configure", &configuration)?;
            let reply = process
                .client
                .request(
                    "configure",
                    configuration,
                    Some(uuid::Uuid::new_v4().to_string().into()),
                    15000,
                )?
                .wait()
                .await?;
            if reply.get("ok") != Some(&true.into())
                || reply.get("result").and_then(|v| v.get("configured")) != Some(&true.into())
            {
                return Err(IvyError::new(
                    "phone_configuration_failed",
                    "Original native configuration did not complete.",
                ));
            }
            let weak = Arc::downgrade(self);
            let voice: Option<Arc<dyn PhoneFlowVoice>> = self
                .settings
                .get("codexVoice")
                .filter(|value| !value.is_null())
                .map(|native| {
                    PhoneCodexVoice::configured(
                        native.clone(),
                        node_path::from_native(self.config.artifact_root()),
                        node_path::from_native(self.config.data_root()),
                        optional_text(self.config.json(), "credential"),
                        &self.account.context(),
                        self.journal.clone(),
                        Arc::new(move |code| {
                            if let Some(owner) = weak.upgrade() {
                                owner.diagnostics.lock().unwrap().voice_failed(code, &now());
                            }
                        }),
                    )
                    .and_then(|voice| {
                        let owner = Arc::downgrade(self);
                        let notices = crate::voice_notices::PhoneVoiceNotices::new(
                            Arc::new(move || {
                                let owner = owner.upgrade().ok_or_else(unavailable)?;
                                let service = owner
                                    .service
                                    .lock()
                                    .unwrap()
                                    .clone()
                                    .ok_or_else(unavailable)?;
                                Ok(Arc::new(service.connection()?)
                                    as Arc<dyn ivy_sdk::rpc::RpcClient>)
                            }),
                            self.journal.clone(),
                            text(self.config.json(), "serviceNodeId")?,
                        );
                        voice.set_input_notice(Arc::new(move |work, input, current| {
                            let notices = notices.clone();
                            Box::pin(async move { notices.notify(work, input, current).await })
                        }));
                        let owner = Arc::downgrade(self);
                        voice.set_work_observer(Arc::new(move |id, code| {
                            if let Some(owner) = owner.upgrade() {
                                owner.diagnostics.lock().unwrap().work_observed(
                                    &String::from_utf16_lossy(&id.0),
                                    code,
                                    &now(),
                                );
                            }
                        }));
                        Ok(Arc::new(voice) as Arc<dyn PhoneFlowVoice>)
                    })
                })
                .transpose()?;
            let failed = Arc::downgrade(self);
            let connected = Arc::downgrade(self);
            let flow = PhoneFlow::new(
                PhoneCallCommands::new(
                    PhoneAdmission::new(policy.clone())?,
                    self.journal.clone(),
                    process.client.clone(),
                ),
                process.client.clone(),
                self.settings.clone(),
                voice,
                Some(self.logs.clone()),
                Arc::new(move |id, code| {
                    if let Some(owner) = failed.upgrade() {
                        owner.diagnostics.lock().unwrap().call_failed(
                            &String::from_utf16_lossy(&id.0),
                            code,
                            &now(),
                        );
                        event("phone_call_failed", Some(code));
                    }
                }),
                Arc::new(move |_| {
                    if let Some(owner) = connected.upgrade() {
                        owner.diagnostics.lock().unwrap().call_succeeded();
                    }
                }),
            )?;
            let owner = Arc::downgrade(self);
            flow.set_cleanup_observer(Arc::new(move |id, code| {
                if let Some(owner) = owner.upgrade() {
                    owner.diagnostics.lock().unwrap().cleanup_observed(
                        &String::from_utf16_lossy(&id.0),
                        code,
                        &now(),
                    );
                }
            }));
            Ok::<_, IvyError>(Runtime {
                bridge: PhoneBridge::new(flow.clone(), process.client.clone()),
                flow,
                process: process.clone(),
            })
        }
        .await;
        let runtime = match initialized {
            Ok(runtime) if !self.stopped.load(Ordering::Acquire) => runtime,
            result => {
                process.close().wait().await?;
                return Err(result.err().unwrap_or_else(unavailable));
            }
        };
        *self.runtime.lock().unwrap() = Some(runtime);
        let weak = Arc::downgrade(self);
        let epoch = process.epoch.clone();
        tokio::spawn(async move {
            let code = process.client.closed().await;
            if let Some(owner) = weak.upgrade() {
                if !owner.stopped.load(Ordering::Acquire)
                    && !owner.rotating.load(Ordering::Acquire)
                    && owner
                        .current()
                        .is_ok_and(|current| current.process.epoch == epoch)
                {
                    event("phone_native_closed", Some(&code));
                    *owner.failure.lock().unwrap() = Some(IvyError::new(
                        "phone_unavailable",
                        "The original native Phone process closed.",
                    ));
                    owner.close();
                }
            }
        });
        event("phone_native_started", None);
        Ok(())
    }
    fn service_client(self: &Arc<Self>) -> IvyResult<ServiceClient> {
        let source = self.config.json();
        let credential = field(source, "credential")?.as_str()?;
        let mut identity = IvyJson::object([
            ("serviceName", "phone-bridge".into()),
            ("hiveProtocol", 1.0.into()),
            ("instanceMode", "singleton".into()),
        ]);
        for key in ["serviceNodeId", "hostId", "version", "buildId"] {
            identity.insert(key, field(source, key)?.clone());
        }
        let weak = Arc::downgrade(self);
        let mut options = ServiceOptions::new(
            field(source, "publicBaseUrl")?.as_str()?,
            Arc::new(move || {
                let value = credential.clone();
                Box::pin(async move { Ok(value) })
            }),
            identity,
            Arc::new(|| Box::pin(async { phone_registry() })),
            Arc::new(move |_| {
                let owner = weak.upgrade();
                Box::pin(async move { owner.ok_or_else(unavailable)?.local().await.map(|_| ()) })
            }),
        );
        for tool in PHONE_TOOLS {
            let weak = Arc::downgrade(self);
            let name = tool.name;
            let handler: ToolHandler = Arc::new(move |args, context| {
                let owner = weak.upgrade();
                Box::pin(async move {
                    owner
                        .ok_or_else(unavailable)?
                        .invoke(name, args, context)
                        .await
                })
            });
            options
                .handlers
                .insert(format!("phone.{name}").into(), handler);
        }
        let weak = Arc::downgrade(self);
        options.readiness = Some(Arc::new(move |connection| {
            let owner = weak.upgrade();
            Box::pin(async move {
                let owner = owner.ok_or_else(unavailable)?;
                owner.local().await?;
                let diagnostics = owner.diagnostics.lock().unwrap().snapshot();
                let voice = diagnostics.iter().any(|v| {
                    v.get("resource").and_then(|v| v.get("scope")) == Some(&"codex_voice".into())
                });
                owner
                    .write_health(
                        true,
                        Some(connection.generation()),
                        if voice {
                            "Codex Voice unavailable; SIP remains active."
                        } else if !diagnostics.is_empty() {
                            "Original Phone results remain available; Phone needs attention."
                        } else {
                            "Phone calls and original outcome queries are available."
                        },
                    )
                    .await?;
                Ok(IvyJson::object([
                    ("ready", true.into()),
                    ("diagnostics", IvyJson::Array(diagnostics)),
                ]))
            })
        }));
        let weak = Arc::downgrade(self);
        options.on_state = Some(Arc::new(move |state| {
            if !matches!(state.status, "offline" | "stopped" | "connecting") {
                return;
            }
            let Some(owner) = weak
                .upgrade()
                .filter(|o| !o.stopped.load(Ordering::Acquire))
            else {
                return;
            };
            tokio::spawn(async move {
                let ready = owner.local().await.is_ok();
                let _ = owner.write_health(ready,state.generation,"Local Phone owner remains active during Hive reconnect; Hive tools are unavailable.").await;
            });
        }));
        options.heartbeat = Duration::from_secs(2);
        ServiceClient::new(options)
    }
    async fn invoke(
        &self,
        name: &str,
        args: IvyJson,
        context: InvocationContext,
    ) -> IvyResult<IvyJson> {
        let current = self.local().await?;
        let service = self
            .service
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(unavailable)?;
        if service.connection()?.generation() != context.generation {
            return Err(unavailable());
        }
        current
            .bridge
            .invoke(name, args, context.caller_principal_id, &context.signal)
            .await
    }
    fn check_voice(self: &Arc<Self>) {
        if self.stopped.load(Ordering::Acquire)
            || self.settings.get("codexVoice").is_none_or(IvyJson::is_null)
            || !self
                .service
                .lock()
                .unwrap()
                .as_ref()
                .is_some_and(ServiceClient::ready)
        {
            return;
        }
        let Some(principal) = optional_text(&self.settings, "incomingPrincipalId") else {
            return;
        };
        let Ok(current) = self.current() else {
            return;
        };
        let mut pending = self.voice.lock().unwrap();
        if pending.as_ref().is_some_and(|handle| !handle.is_finished()) {
            return;
        }
        *self.last_voice_check.lock().unwrap() = Some(Instant::now());
        let owner = self.clone();
        *pending = Some(tokio::spawn(async move {
            let result = current.flow.prewarm_voice(principal).await;
            if owner.stopped.load(Ordering::Acquire)
                || !owner
                    .current()
                    .is_ok_and(|v| v.process.epoch == current.process.epoch)
            {
                return;
            }
            let mut diagnostics = owner.diagnostics.lock().unwrap();
            match result {
                Ok(()) => {
                    if diagnostics.voice_succeeded() {
                        event("phone_codex_voice_recovered", None);
                    }
                }
                Err(error) => {
                    if diagnostics.voice_failed(error.code.as_str(), &now()) {
                        event("phone_codex_voice_unavailable", Some(error.code.as_str()));
                    }
                }
            }
        }));
    }
    async fn tick(self: &Arc<Self>) -> IvyResult<()> {
        if self.health.control().await?.is_some() {
            self.close();
            return Ok(());
        }
        let current = self.local().await?;
        let observed = current.process.client.observe(None).await?;
        {
            let mut diagnostics = self.diagnostics.lock().unwrap();
            diagnostics.registration_observed(
                self.settings
                    .get("registration")
                    .is_some_and(|v| !v.is_null()),
                observed.get("registration"),
                &now(),
            );
            diagnostics.poll_succeeded();
        }
        if let Some(principal) = optional_text(&self.settings, "incomingPrincipalId") {
            let ids = observed
                .get("callIds")
                .and_then(IvyJson::as_array)
                .map(|v| v.to_vec())
                .unwrap_or_else(|| {
                    observed
                        .get("call")
                        .and_then(|v| v.get("id"))
                        .cloned()
                        .into_iter()
                        .collect()
                });
            let maximum = self
                .settings
                .get("binding")
                .and_then(|v| v.get("maxConcurrentCalls"))
                .and_then(IvyJson::as_f64)
                .unwrap_or(1.0) as usize;
            for id in ids {
                let Some(id) = id.as_js_string().cloned() else {
                    continue;
                };
                if self.stopped.load(Ordering::Acquire)
                    || self.journal.call(id.clone()).await?.is_some()
                    || self.journal.current_calls().await?.len() >= maximum
                {
                    continue;
                }
                let candidate =
                    if observed.get("call").and_then(|v| v.get("id")) == Some(&id.clone().into()) {
                        observed.clone()
                    } else {
                        current.process.client.observe(Some(id.clone())).await?
                    };
                if candidate.get("call").and_then(|v| v.get("direction"))
                    != Some(&"incoming".into())
                    || candidate.get("call").and_then(|v| v.get("state")) != Some(&"ringing".into())
                {
                    continue;
                }
                if let Err(error) = current
                    .flow
                    .accept(principal.clone(), id.clone(), id)
                    .wait()
                    .await
                {
                    if !matches!(
                        error.code.as_str(),
                        "phone_caller_refused" | "phone_call_busy"
                    ) {
                        return Err(error);
                    }
                }
            }
        }
        current.flow.observe(Some(observed.clone())).await?;
        let idle = self.journal.current_calls().await?.is_empty();
        if idle
            && self
                .last_voice_check
                .lock()
                .unwrap()
                .is_none_or(|at| at.elapsed() >= Duration::from_secs(60))
        {
            self.check_voice();
        }
        if idle
            && !current.flow.has_voice_work()
            && observed.get("call").is_none_or(IvyJson::is_null)
            && observed
                .get("callIds")
                .and_then(IvyJson::as_array)
                .is_none_or(|v| v.is_empty())
            && self.journal.epoch_operations().await? >= 4000.0
        {
            self.rotating.store(true, Ordering::Release);
            let rotated = async {
                current.flow.close().wait().await?;
                current.process.close().wait().await?;
                if !self.stopped.load(Ordering::Acquire) {
                    self.initialize().await?;
                    self.check_voice();
                }
                Ok::<_, IvyError>(())
            }
            .await;
            self.rotating.store(false, Ordering::Release);
            if let Err(error) = rotated {
                // Stop can arrive between the idle check and initialization.
                // Initialization has already drained any late process. This
                // expected refusal must not turn cooperative stop into failure.
                if self.stopped.load(Ordering::Acquire) && error.code == "service_not_ready" {
                    return Ok(());
                }
                *self.failure.lock().unwrap() = Some(error.clone());
                self.close();
                return Err(error);
            }
        }
        Ok(())
    }
    async fn poll(self: Arc<Self>) {
        let interval = Duration::from_millis(
            self.settings
                .get("pollMs")
                .and_then(IvyJson::as_f64)
                .unwrap_or(500.0) as u64,
        );
        while !self.stop.is_cancelled() {
            if let Err(error) = self.tick().await {
                self.diagnostics
                    .lock()
                    .unwrap()
                    .poll_failed(error.code.as_str(), &now());
            }
            tokio::select! { biased; _=self.stop.cancelled()=>break, _=tokio::time::sleep(interval)=>{} }
        }
    }
    fn close(self: &Arc<Self>) -> PhoneServiceClose {
        let mut closing = self.closing.lock().unwrap();
        if let Some(work) = &*closing {
            return work.clone();
        }
        self.stopped.store(true, Ordering::Release);
        self.stop.cancel();
        let (send, receive) = watch::channel(None);
        let work = PhoneServiceClose(receive);
        *closing = Some(work.clone());
        let owner = self.clone();
        tokio::spawn(async move {
            let task = tokio::spawn(async move {
                owner.close_owned().await;
                owner.failure.lock().unwrap().clone().map_or(Ok(()), Err)
            });
            send.send_replace(Some(
                task.await.unwrap_or_else(|_| Err(IvyError::internal())),
            ));
        });
        work
    }
    async fn close_owned(self: &Arc<Self>) {
        let service = self.service.lock().unwrap().clone();
        if let Some(service) = service {
            service.stop().await;
        }
        let current = {
            let _gate = self.initialize_gate.lock().await;
            self.current().ok()
        };
        if let Some(current) = current {
            let flow = current.flow.close().wait().await;
            let process = current.process.close().wait().await;
            for result in [flow.map(|_| ()), process] {
                if let Err(error) = result {
                    self.failure.lock().unwrap().get_or_insert(error);
                }
            }
        }
        let poll = self.poll.lock().unwrap().take();
        if let Some(poll) = poll {
            if poll.await.is_err() {
                self.failure
                    .lock()
                    .unwrap()
                    .get_or_insert_with(IvyError::internal);
            }
        }
        let voice = self.voice.lock().unwrap().take();
        if let Some(voice) = voice {
            if voice.await.is_err() {
                self.failure
                    .lock()
                    .unwrap()
                    .get_or_insert_with(IvyError::internal);
            }
        }
        let _health = self.health_gate.lock().await;
        let logs = self.logs.close();
        let journal = self.journal.close().wait().await;
        let health = self
            .health
            .write(false, "PhoneBridge stopped.".into(), None)
            .await;
        for result in [logs, journal, health] {
            if let Err(error) = result {
                self.failure.lock().unwrap().get_or_insert(error);
            }
        }
    }
}
