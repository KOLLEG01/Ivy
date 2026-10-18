//! Provider connection, reconciliation, heartbeat and durable event consumption.
pub use crate::service_connection::{
    ConnectionOptions, InvocationCapacity, InvocationContext, NotificationObserver,
    ServiceConnection, ServiceFuture, ToolHandler,
};
use crate::{
    IvyError, IvyJson, IvyResult, JsString, Outcome,
    rpc::{RequestOptions, RpcClient, base_url},
};
use futures_util::FutureExt;
use ivy_contracts::wire_schema::validate_shared;
use std::{
    collections::HashMap,
    panic::AssertUnwindSafe,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

pub type Dependency<T> = Arc<dyn Fn() -> ServiceFuture<T> + Send + Sync>;
pub type ConnectionDependency<T> = Arc<dyn Fn(ServiceConnection) -> ServiceFuture<T> + Send + Sync>;
pub type StateObserver = Arc<dyn Fn(ServiceState) + Send + Sync>;
#[derive(Clone, Debug)]
pub struct ServiceState {
    pub status: &'static str,
    pub generation: Option<u64>,
    pub code: Option<JsString>,
    pub message: Option<JsString>,
    pub phase: Option<&'static str>,
}
pub struct ServiceOptions {
    pub public_base_url: String,
    pub credential: Dependency<String>,
    pub identity: IvyJson,
    pub registry: Dependency<IvyJson>,
    pub handlers: HashMap<JsString, ToolHandler>,
    pub json_handlers: HashMap<JsString, ToolHandler>,
    pub reconcile: ConnectionDependency<()>,
    pub readiness: Option<ConnectionDependency<IvyJson>>,
    pub on_state: Option<StateObserver>,
    pub notification_filters: Option<Dependency<IvyJson>>,
    pub on_notification: Option<NotificationObserver>,
    pub reconnect_min: Duration,
    pub reconnect_max: Duration,
    pub heartbeat: Duration,
}
impl ServiceOptions {
    pub fn new(
        public_base_url: String,
        credential: Dependency<String>,
        identity: IvyJson,
        registry: Dependency<IvyJson>,
        reconcile: ConnectionDependency<()>,
    ) -> Self {
        Self {
            public_base_url,
            credential,
            identity,
            registry,
            reconcile,
            handlers: HashMap::new(),
            json_handlers: HashMap::new(),
            readiness: None,
            on_state: None,
            notification_filters: None,
            on_notification: None,
            reconnect_min: Duration::from_millis(500),
            reconnect_max: Duration::from_secs(10),
            heartbeat: Duration::from_secs(5),
        }
    }
}
struct Current {
    connection: ServiceConnection,
    acknowledged: bool,
}
struct Inner {
    options: Arc<ServiceOptions>,
    signal: CancellationToken,
    task: Mutex<Option<JoinHandle<()>>>,
    current: Mutex<Option<Current>>,
    capacity: InvocationCapacity,
}
#[derive(Clone)]
pub struct ServiceClient(Arc<Inner>);
fn unknown(code: &str, message: &str) -> IvyError {
    IvyError::new(code, message).with_outcome(Outcome::Unknown)
}

// Dropping an observation must not abort an already accepted dependency future.
async fn dependency<T: Send + 'static, F>(start: F, signal: CancellationToken) -> IvyResult<T>
where
    F: FnOnce() -> ServiceFuture<T> + Send + 'static,
{
    if signal.is_cancelled() {
        return Err(unknown(
            "service_unavailable",
            "Dependency observation was cancelled; accepted work must be reconciled.",
        ));
    }
    let task = tokio::spawn(async move {
        AssertUnwindSafe(async move { start().await })
            .catch_unwind()
            .await
            .unwrap_or_else(|_| Err(IvyError::new("dependency_unavailable", "")))
    });
    tokio::select! {
        biased;
        result=task=>result.unwrap_or_else(|_|Err(IvyError::new("dependency_unavailable",""))),
        _=signal.cancelled()=>Err(unknown("service_unavailable","Dependency observation was cancelled; accepted work must be reconciled.")),
        _=tokio::time::sleep(Duration::from_secs(30))=>Err(unknown("deadline_exceeded","Service dependency did not finish before its deadline.")),
    }
}
fn jitter(minimum: f64, spread: f64) -> f64 {
    let bytes = *uuid::Uuid::new_v4().as_bytes();
    // The first six UUID bytes contain only random bits, with no version/variant bits.
    let mut random = [0; 8];
    random[..6].copy_from_slice(&bytes[..6]);
    minimum + spread * u64::from_le_bytes(random) as f64 / 281_474_976_710_656.0
}
impl ServiceClient {
    pub fn new(options: ServiceOptions) -> IvyResult<Self> {
        base_url(&options.public_base_url)?;
        validate_shared("ServiceConnect", &options.identity)?;
        Ok(Self(Arc::new(Inner {
            options: Arc::new(options),
            signal: CancellationToken::new(),
            task: Mutex::new(None),
            current: Mutex::new(None),
            capacity: InvocationCapacity::default(),
        })))
    }
    pub fn connection(&self) -> IvyResult<ServiceConnection> {
        self.0
            .current
            .lock()
            .unwrap()
            .as_ref()
            .filter(|c| c.acknowledged && c.connection.ready())
            .map(|c| c.connection.clone())
            .ok_or_else(|| {
                IvyError::new(
                    "service_not_ready",
                    "The service has not reconciled its current Hive generation.",
                )
            })
    }
    pub fn ready(&self) -> bool {
        self.connection().is_ok()
    }
    pub fn start(&self) {
        let mut task = self.0.task.lock().unwrap();
        if task.is_none() && !self.0.signal.is_cancelled() {
            let client = self.clone();
            *task = Some(tokio::spawn(async move { client.run().await }));
        }
    }
    pub async fn stop(&self) {
        self.0.signal.cancel();
        if let Some(current) = self.0.current.lock().unwrap().as_ref() {
            current.connection.close();
        }
        let task = self.0.task.lock().unwrap().take();
        if let Some(task) = task {
            let _ = task.await;
        }
    }
    pub async fn wait_ready(
        &self,
        options: Option<RequestOptions>,
    ) -> IvyResult<ServiceConnection> {
        let options = options.unwrap_or_else(|| RequestOptions {
            timeout: Duration::from_secs(30),
            ..Default::default()
        });
        let timer = tokio::time::sleep(options.timeout);
        tokio::pin!(timer);
        loop {
            if let Ok(connection) = self.connection() {
                return Ok(connection);
            }
            tokio::select! {biased;
                _=options.cancellation.cancelled()=>return Err(options.cancel_error),
                _=self.0.signal.cancelled()=>return Err(IvyError::new("cancelled","Service is stopping.")),
                _=&mut timer=>return Err(IvyError::new("deadline_exceeded","Service did not become ready before its deadline.")),
                _=tokio::time::sleep(Duration::from_millis(25))=>{},
            }
        }
    }
    fn state(&self, status: &'static str, error: Option<&IvyError>, phase: Option<&'static str>) {
        if let Some(observer) = &self.0.options.on_state {
            let generation = self
                .0
                .current
                .lock()
                .unwrap()
                .as_ref()
                .map(|c| c.connection.generation())
                .filter(|v| *v != 0);
            let state = ServiceState {
                status,
                generation,
                code: error.map(IvyError::code_js).filter(|s| !s.0.is_empty()),
                message: error
                    .map(|e| JsString(e.message_js().0.into_iter().take(256).collect()))
                    .filter(|s| !s.0.is_empty()),
                phase,
            };
            let _ = std::panic::catch_unwind(AssertUnwindSafe(|| observer(state)));
        }
    }
    async fn generation(
        &self,
        connection: &ServiceConnection,
        phase: &mut &'static str,
        became_ready: &mut bool,
    ) -> IvyResult<()> {
        let options = &self.0.options;
        *phase = "service.connect";
        let connected = connection
            .request(
                "service.connect",
                options.identity.clone(),
                RequestOptions::default(),
            )
            .await?;
        connection.set_generation(connected.required("generation")?.as_f64().unwrap_or(0.0) as u64);
        self.state("syncing", None, None);
        *phase = "registry.prepare";
        let callback = options.registry.clone();
        let registry = connection
            .prepare_registry(&dependency(move || callback(), connection.signal()).await?)?;
        *phase = "registry.sync";
        connection
            .request("registry.sync", registry, RequestOptions::default())
            .await?;
        if options.on_notification.is_some() {
            *phase = "notifications.subscribe";
            let callback = options.notification_filters.clone();
            let filters = dependency(
                move || {
                    Box::pin(async move {
                        match callback {
                            Some(callback) => callback().await,
                            None => Ok(IvyJson::Array(vec![])),
                        }
                    })
                },
                connection.signal(),
            )
            .await?;
            connection
                .request(
                    "notifications.subscribe",
                    IvyJson::object([("filters", filters)]),
                    RequestOptions::default(),
                )
                .await?;
        }
        *phase = "reconcile";
        let callback = options.reconcile.clone();
        let context = connection.clone();
        dependency(move || callback(context), connection.signal()).await?;
        while !connection.signal().is_cancelled() && !self.0.signal.is_cancelled() {
            *phase = "readiness";
            let callback = options.readiness.clone();
            let context = connection.clone();
            let observation = dependency(
                move || {
                    Box::pin(async move {
                        match callback {
                            Some(callback) => callback(context).await,
                            None => Ok(IvyJson::object([
                                ("ready", true.into()),
                                ("diagnostics", IvyJson::Array(vec![])),
                            ])),
                        }
                    })
                },
                connection.signal(),
            )
            .await?;
            connection.set_ready(
                observation
                    .get("ready")
                    .and_then(IvyJson::as_bool)
                    .unwrap_or(false),
            );
            *phase = "service.heartbeat";
            let accepted = connection
                .request("service.heartbeat", observation, RequestOptions::default())
                .await?;
            let ready = accepted
                .get("ready")
                .and_then(IvyJson::as_bool)
                .unwrap_or(false);
            connection.set_ready(ready);
            if let Some(current) = self.0.current.lock().unwrap().as_mut() {
                current.acknowledged = ready;
            }
            self.state(if ready { "ready" } else { "degraded" }, None, None);
            *became_ready |= ready;
            *phase = "heartbeat.wait";
            tokio::select! {biased;_=connection.closed()=>return Err(connection.close_reason().unwrap()),_=self.0.signal.cancelled()=>break,_=tokio::time::sleep(options.heartbeat)=>{}}
        }
        Ok(())
    }
    async fn run(&self) {
        let mut failures = 0u32;
        while !self.0.signal.is_cancelled() {
            let mut phase = "credential";
            let mut connection = None;
            let mut became_ready = false;
            self.state("connecting", None, None);
            let result: IvyResult<()> = async {
                let callback = self.0.options.credential.clone();
                let credential = dependency(move || callback(), self.0.signal.clone()).await?;
                if self.0.signal.is_cancelled() {
                    return Ok(());
                }
                phase = "socket.open";
                let options = &self.0.options;
                let established = ServiceConnection::connect(
                    &options.public_base_url,
                    &credential,
                    ConnectionOptions {
                        service_node_id: options
                            .identity
                            .required("serviceNodeId")?
                            .as_js_string()
                            .unwrap()
                            .clone(),
                        handlers: options.handlers.clone(),
                        json_handlers: options.json_handlers.clone(),
                        capacity: self.0.capacity.clone(),
                        on_notification: options.on_notification.clone(),
                    },
                    self.0.signal.clone(),
                )
                .await?;
                *self.0.current.lock().unwrap() = Some(Current {
                    connection: established.clone(),
                    acknowledged: false,
                });
                connection = Some(established.clone());
                // Stop closes the socket even when a dependency observation is pending.
                let signal = self.0.signal.clone();
                let context = established.clone();
                let stop = tokio::spawn(async move {
                    tokio::select! {_=signal.cancelled()=>context.close(),_=context.closed()=>{}}
                });
                let result = self
                    .generation(&established, &mut phase, &mut became_ready)
                    .await;
                stop.abort();
                result
            }
            .await;
            if let Err(error) = result {
                let error = connection
                    .as_ref()
                    .and_then(ServiceConnection::close_reason)
                    .unwrap_or(error);
                self.state("offline", Some(&error), Some(phase));
            }
            if let Some(connection) = connection {
                connection.close();
            }
            *self.0.current.lock().unwrap() = None;
            if self.0.signal.is_cancelled() {
                break;
            }
            if became_ready {
                failures = 0;
            }
            let minimum = self.0.options.reconnect_min.max(Duration::from_millis(25));
            let maximum = self.0.options.reconnect_max.max(minimum);
            let backoff = minimum
                .saturating_mul(1 << failures.min(8))
                .min(maximum)
                .mul_f64(jitter(0.8, 0.2));
            failures = failures.saturating_add(1);
            tokio::select! {_=self.0.signal.cancelled()=>break,_=tokio::time::sleep(backoff)=>{}}
        }
        self.state("stopped", None, None);
    }
}

pub type PersistBatch = Arc<dyn Fn(IvyJson, CancellationToken) -> ServiceFuture<()> + Send + Sync>;
pub type EventFailureObserver = Arc<dyn Fn(&IvyError, &IvyJson) + Send + Sync>;
pub struct ConsumeEventsOptions {
    pub poll: Duration,
    pub on_failure: Option<EventFailureObserver>,
}
impl Default for ConsumeEventsOptions {
    fn default() -> Self {
        Self {
            poll: Duration::from_secs(30),
            on_failure: None,
        }
    }
}
/// A batch is acknowledged only after its durable result commits successfully.
pub async fn consume_events(
    connection: &ServiceConnection,
    subscription: IvyJson,
    persist_batch: PersistBatch,
    options: ConsumeEventsOptions,
) -> IvyResult<()> {
    while !connection.signal().is_cancelled() {
        let batch = connection
            .request(
                "events.subscribe",
                subscription.clone(),
                RequestOptions::default(),
            )
            .await?;
        if let Err(error) = persist_batch(batch.clone(), connection.signal()).await {
            if let Some(observer) = &options.on_failure {
                observer(&error, &batch);
            }
            return Err(error);
        }
        connection.check_open()?;
        let mut ack = IvyJson::object([
            ("name", subscription.required("name")?.clone()),
            (
                "throughSequence",
                batch.required("throughSequence")?.clone(),
            ),
        ]);
        if let Some(gap) = batch.get("gap").filter(|gap| !gap.is_null()) {
            ack.insert(
                "gapThroughSequence",
                gap.required("prunedThroughSequence")?.clone(),
            );
        }
        connection
            .request("events.ack", ack, RequestOptions::default())
            .await?;
        if batch.get("hasMore").and_then(IvyJson::as_bool) != Some(true) {
            connection
                .wait_for_events(
                    batch.required("throughSequence")?.as_f64().unwrap_or(0.0) as u64,
                    options
                        .poll
                        .max(Duration::from_millis(25))
                        .mul_f64(jitter(0.9, 0.2)),
                )
                .await?;
        }
    }
    Ok(())
}
