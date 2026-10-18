//! Service-owned generation replacement. One local task owns each journal and
//! joins its accepted work before releasing the journal or opening its successor.
use crate::{
    assignment_driver::AssignmentDriver,
    assignment_execution::AssignmentExecution,
    assignment_native::AssignmentNative,
    assignment_scheduler::AssignmentScheduler,
    engine::{SecretaryEngine, random_uuid, validate_settings},
    generation::{GenerationExit, ReconciledGeneration, ServiceEvent},
    inbox_evidence::InboxEvidence,
    inbox_recovery::RecoveryScan,
    journal::{Journal, Limits},
    service_handlers::{Invocation, SecretaryHandlers, invoke},
    store::Store,
};
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString, checked_native_contract::CheckedNativeContracts,
};
use ivy_sdk::{
    clock::{Clock, SystemClock},
    rpc::{RequestOptions, RpcClient},
    service::ServiceConnection,
    tools::ToolClients,
};
use std::{
    cell::RefCell,
    future::Future,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::mpsc;

#[derive(Clone, Default)]
pub struct Readiness {
    pub generation: Option<u64>,
    pub recovering: bool,
    pub failure: Option<JsString>,
    pub issue: Option<JsString>,
}
pub struct Observation {
    pub ready: bool,
    pub detail: &'static str,
    pub heartbeat: IvyJson,
}
impl Readiness {
    /// Health and heartbeat use the same observation. Record warnings leave the
    /// reconciled inbox available, as in the original readiness callback.
    pub fn observe(
        &self,
        generation: u64,
        service_node_id: &JsString,
        at: &JsString,
    ) -> Observation {
        let issue = self
            .failure
            .as_ref()
            .or(self.issue.as_ref())
            .filter(|code| !code.0.is_empty());
        let ready = self.generation == Some(generation)
            && self.failure.as_ref().is_none_or(|code| code.0.is_empty())
            && !self.recovering;
        let detail = if issue.is_some() {
            "Secretary recovery needs attention; independent inbox actions remain available."
        } else if ready {
            "Secretary sources and original operations are reconciled."
        } else {
            "Secretary reconciliation is pending."
        };
        let diagnostics = issue
            .map(|code| {
                IvyJson::object([
                    ("code", IvyJson::String(code.clone())),
                    (
                        "message",
                        "Secretary recovery requires attention; other source work continues."
                            .into(),
                    ),
                    ("severity", "warning".into()),
                    ("resource", IvyJson::Null),
                    ("source", IvyJson::String(service_node_id.clone())),
                    ("firstObservedAt", IvyJson::String(at.clone())),
                    ("lastObservedAt", IvyJson::String(at.clone())),
                    ("status", "current".into()),
                ])
            })
            .into_iter()
            .collect();
        Observation {
            ready,
            detail,
            heartbeat: IvyJson::object([
                ("ready", ready.into()),
                ("diagnostics", IvyJson::Array(diagnostics)),
            ]),
        }
    }
}

/// Cross-thread callbacks observe values only. Journal references never cross
/// the SDK's Send callback boundary.
#[derive(Clone, Default)]
pub struct ReadinessState(Arc<Mutex<Readiness>>);
impl ReadinessState {
    pub fn current(&self) -> Readiness {
        self.0.lock().unwrap().clone()
    }
    fn begin(&self) {
        let mut state = self.0.lock().unwrap();
        state.generation = None;
        state.issue = None;
        state.recovering = true;
    }
    fn settled(&self, generation: u64) {
        let mut state = self.0.lock().unwrap();
        state.generation = Some(generation);
        state.failure = None;
        state.recovering = false;
    }
    fn issue(&self, issue: Option<JsString>) {
        self.0.lock().unwrap().issue = issue;
    }
    fn release(&self) {
        let mut state = self.0.lock().unwrap();
        state.generation = None;
        state.issue = None;
        state.recovering = false;
    }
    fn failed(&self, error: &IvyError) {
        self.0.lock().unwrap().failure = Some(error.code_js());
    }
}

/// Values prepared by service startup. Generic configuration, health and
/// process mechanics remain with HostExecutor; dormant workers are not started.
pub struct ReconcileConfig {
    pub settings: IvyJson,
    pub service_node_id: JsString,
    pub host_id: JsString,
    pub data_root: PathBuf,
    pub distribution_root: PathBuf,
    pub job_launcher: Option<PathBuf>,
    pub minimum_message_age_minutes: f64,
    pub notification_policy: JsString,
}
pub struct Reconciliation {
    pub config: ReconcileConfig,
    pub readiness: ReadinessState,
}

/// Attempts are zero-based: the original permits six attempts, with a bounded
/// backoff only for limit_exceeded and only while that connection is current.
pub fn retry_delay(error: &IvyError, attempt: usize, cancelled: bool) -> Option<Duration> {
    (error.code == "limit_exceeded" && !cancelled && attempt < 5)
        .then(|| Duration::from_millis(250 * (attempt as u64 + 1)))
}

async fn reject(request: Invocation) {
    let result = invoke(
        None,
        request.method,
        &request.arguments,
        &request.context,
        || Ok(()),
        || {},
    )
    .await;
    let _ = request.reply.send(result);
}

/// Do not drop initialization when a replacement arrives. Existing RPC and local
/// storage work still belongs to the old connection and must settle first.
async fn initializing(
    work: impl Future<Output = IvyResult<()>>,
    connection: &ServiceConnection,
    invocations: &mut mpsc::UnboundedReceiver<Invocation>,
    events: &mut mpsc::UnboundedReceiver<ServiceEvent>,
    next: &mut Option<ServiceEvent>,
) -> IvyResult<()> {
    tokio::pin!(work);
    let mut invocation_open = true;
    loop {
        tokio::select! {
            biased;
            event = events.recv(), if next.is_none() => match event {
                Some(event @ (ServiceEvent::Reconcile(_) | ServiceEvent::Stop)) => {
                    connection.close(); *next = Some(event);
                },
                Some(ServiceEvent::State(_)) => {},
                None => { connection.close(); *next = Some(ServiceEvent::Stop); },
            },
            result = &mut work => return result,
            request = invocations.recv(), if invocation_open => match request {
                Some(request) => reject(request).await,
                None => {
                    invocation_open = false;
                    connection.close();
                    if next.is_none() { *next = Some(ServiceEvent::Stop); }
                },
            },
        }
    }
}

impl Reconciliation {
    /// Run on the service's local task, not in a Send SDK callback. Returning
    /// means initialization, handlers, background workers and preflights joined.
    pub async fn run(
        &self,
        invocations: &mut mpsc::UnboundedReceiver<Invocation>,
        events: &mut mpsc::UnboundedReceiver<ServiceEvent>,
    ) {
        self.run_with_clock(invocations, events, &SystemClock).await;
    }
    pub async fn run_with_clock(
        &self,
        invocations: &mut mpsc::UnboundedReceiver<Invocation>,
        events: &mut mpsc::UnboundedReceiver<ServiceEvent>,
        clock: &dyn Clock,
    ) {
        let retained = CheckedNativeContracts::new(&self.config.distribution_root);
        let mut next = None;
        loop {
            let event = match next.take() {
                Some(event) => event,
                None => tokio::select! {
                    biased;
                    event = events.recv() => match event { Some(event) => event, None => break },
                    request = invocations.recv() => {
                        match request { Some(request) => reject(request).await, None => break }
                        continue;
                    },
                },
            };
            match event {
                ServiceEvent::Stop => break,
                ServiceEvent::State(_) => {}
                ServiceEvent::Reconcile(connection) => {
                    self.readiness.begin();
                    // Reuse one client identity across retry attempts, including
                    // the shared SDK runtime-epoch and owner caches.
                    let client: Arc<dyn RpcClient> = Arc::new(connection.clone());
                    for attempt in 0.. {
                        let result = self
                            .attempt(
                                &connection,
                                client.clone(),
                                &retained,
                                clock,
                                invocations,
                                events,
                                &mut next,
                            )
                            .await;
                        match result {
                            Ok(exit) => {
                                next = exit.next.or(next);
                                break;
                            }
                            Err(error) => {
                                if let Some(delay) =
                                    retry_delay(&error, attempt, connection.signal().is_cancelled())
                                {
                                    let signal = connection.signal();
                                    let wait = async {
                                        tokio::select! {
                                            biased;
                                            _ = signal.cancelled() => connection.check_open(),
                                            _ = tokio::time::sleep(delay) => Ok(()),
                                        }
                                    };
                                    if initializing(
                                        wait,
                                        &connection,
                                        invocations,
                                        events,
                                        &mut next,
                                    )
                                    .await
                                    .is_ok()
                                    {
                                        continue;
                                    }
                                }
                                if !connection.signal().is_cancelled() {
                                    self.readiness.failed(&error);
                                    eprintln!(
                                        "{}",
                                        serde_json::json!({"code":error.code,"phase":"reconcile"})
                                    );
                                    connection.close_with(error);
                                }
                                break;
                            }
                        }
                    }
                    self.readiness.release();
                }
            }
        }
        self.readiness.release();
    }

    async fn attempt(
        &self,
        connection: &ServiceConnection,
        client: Arc<dyn RpcClient>,
        retained: &CheckedNativeContracts,
        clock: &dyn Clock,
        invocations: &mut mpsc::UnboundedReceiver<Invocation>,
        events: &mut mpsc::UnboundedReceiver<ServiceEvent>,
        next: &mut Option<ServiceEvent>,
    ) -> IvyResult<GenerationExit> {
        let config = &self.config;
        validate_settings(&config.settings)?;
        let journal = Journal::open(
            &config.data_root.join("secretary.sqlite"),
            Limits::default(),
        )?;
        let store = Store {
            client: client.clone(),
            root: config
                .settings
                .required("identity")?
                .required("scope")?
                .required("rootObjectId")?
                .as_js_string()
                .cloned()
                .ok_or_else(IvyError::internal)?,
            journal: &journal,
        };
        let evidence = InboxEvidence {
            store: &store,
            settings: &config.settings,
            service_node_id: &config.service_node_id,
            cancellation: RequestOptions {
                cancellation: connection.signal(),
                ..Default::default()
            },
        };
        let tools = Arc::new(ToolClients::new(client));
        let issues = RefCell::new(RecoveryScan::default());
        let engine = SecretaryEngine::new(
            &evidence,
            clock,
            &SystemClock,
            random_uuid,
            tools.as_ref(),
            &issues,
        );
        let scheduler = AssignmentScheduler {
            evidence: &evidence,
            tools: tools.clone(),
            clock,
            fallback_execution: None,
        };
        initializing(
            async {
                engine.initialize().await?;
                engine.tick().await?;
                connection.check_open()?;
                scheduler.initialize().await?;
                connection.check_open()
            },
            connection,
            invocations,
            events,
            next,
        )
        .await?;
        if next.is_some() {
            return Ok(GenerationExit { next: next.take() });
        }
        let execution = AssignmentExecution {
            native: AssignmentNative {
                evidence: &evidence,
                tools: tools.clone(),
            },
            clock,
            retained,
            notification_policy: &config.notification_policy,
        };
        let (runner, driver) = AssignmentDriver::new(
            &execution,
            config.minimum_message_age_minutes,
            &issues,
            config.distribution_root.clone(),
            config.job_launcher.clone(),
        );
        let generation = ReconciledGeneration {
            connection: connection.clone(),
            handlers: SecretaryHandlers {
                engine: &engine,
                generation: connection.generation(),
                service_node_id: &config.service_node_id,
                host_id: &config.host_id,
                tools: tools.clone(),
                retained,
            },
            scheduler,
            runner,
            minimum_message_age_minutes: config.minimum_message_age_minutes,
        };
        self.readiness.settled(connection.generation());
        Ok(generation
            .run(driver, invocations, events, |issue| {
                self.readiness.issue(issue)
            })
            .await)
    }
}
