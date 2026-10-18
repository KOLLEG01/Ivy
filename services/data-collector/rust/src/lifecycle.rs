//! Service lifetime around the durable execution and definition owners.
use crate::{
    definition_journal::CompareNames,
    definitions::Definitions,
    engine::{self, Engine},
    ingress::Ingress,
    model,
    native_mcp::NativeMcpFactory,
    provider::{self, CollectorBackend},
    runner::TaskRunner,
    schema,
    store::{CollectorStore, Limits},
    store_owner::StoreOwner,
};
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString};
use ivy_host_runtime::{
    config::{self, InstanceConfig},
    health::HealthFile,
    node_runtime::NodeRuntime,
};
use ivy_sdk::{
    clock::{Clock, SystemClock},
    rpc::RpcClient,
    service::{InvocationContext, ServiceClient, ServiceFuture, ServiceState},
};
use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{
    sync::{mpsc, oneshot, watch},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

pub struct StartOptions {
    /// Required shared String.localeCompare semantics. No production ordinal fallback.
    pub compare_names: CompareNames,
    pub node_executable: Option<PathBuf>,
    pub job_launcher: Option<PathBuf>,
}

enum HealthCommand {
    Write(bool, JsString, Option<u64>, oneshot::Sender<IvyResult<()>>),
    Offline(Option<JsString>),
    Finish(oneshot::Sender<IvyResult<()>>),
}
#[derive(Clone)]
struct HealthQueue(mpsc::UnboundedSender<HealthCommand>);
fn unavailable() -> IvyError {
    IvyError::new("service_unavailable", "Collector is stopping.")
}
impl HealthQueue {
    fn new(health: Arc<HealthFile>, store: StoreOwner) -> Self {
        let (sender, mut commands) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            while let Some(command) = commands.recv().await {
                match command {
                    HealthCommand::Write(ready, detail, generation, reply) => {
                        let _ = reply.send(health.write(ready, detail, generation).await);
                    }
                    HealthCommand::Offline(code) => {
                        let ready = store.call(|s| Ok(!s.list(false)?.is_empty())).await;
                        if let Ok(ready) = ready {
                            let mut detail = JsString::from("Hive unavailable").0;
                            if let Some(code) = code.filter(|c| !c.0.is_empty()) {
                                detail.extend(" (".encode_utf16());
                                detail.extend(code.0);
                                detail.push(')' as u16);
                            }
                            detail.extend(
                                "; accepted jobs continue from the durable cache.".encode_utf16(),
                            );
                            let _ = health.write(ready, JsString(detail), None).await;
                        }
                    }
                    HealthCommand::Finish(reply) => {
                        // Every earlier observation is drained before the final stopped file.
                        // A failed write must never skip retiring the SQLite connection.
                        let retired = store.close().await;
                        let written = health
                            .write(false, "DataCollector stopped.".into(), None)
                            .await;
                        let _ = reply.send(retired.and(written));
                        break;
                    }
                }
            }
        });
        Self(sender)
    }
    async fn write(&self, ready: bool, detail: &str, generation: Option<u64>) -> IvyResult<()> {
        let (reply, result) = oneshot::channel();
        self.0
            .send(HealthCommand::Write(
                ready,
                detail.into(),
                generation,
                reply,
            ))
            .map_err(|_| unavailable())?;
        result.await.map_err(|_| unavailable())?
    }
    async fn finish(&self) -> IvyResult<()> {
        let (reply, result) = oneshot::channel();
        self.0
            .send(HealthCommand::Finish(reply))
            .map_err(|_| unavailable())?;
        result.await.map_err(|_| unavailable())?
    }
}
type ClientSlot = Arc<Mutex<Option<Arc<dyn RpcClient>>>>;
struct Backend {
    engine: Engine,
    definitions: Definitions,
    health: HealthQueue,
    client: ClientSlot,
}
impl CollectorBackend for Backend {
    fn read(&self, input: IvyJson) -> ServiceFuture<IvyJson> {
        let definitions = self.definitions.clone();
        Box::pin(async move { definitions.read(input).await })
    }
    fn update(&self, input: IvyJson, context: InvocationContext) -> ServiceFuture<IvyJson> {
        let definitions = self.definitions.clone();
        Box::pin(async move { definitions.update(input, Some(context)).await })
    }
    fn reconcile(&self, client: Arc<dyn RpcClient>) -> ServiceFuture<()> {
        *self.client.lock().unwrap() = Some(client.clone());
        let engine = self.engine.clone();
        let definitions = self.definitions.clone();
        Box::pin(async move {
            engine.attach(client).await?;
            definitions.sync().await
        })
    }
    fn detach(&self) {
        *self.client.lock().unwrap() = None;
        self.engine.detach();
    }
    fn state(&self, state: ServiceState) {
        if matches!(state.status, "connecting" | "offline" | "stopped") {
            let _ = self.health.0.send(HealthCommand::Offline(state.code));
        }
    }
    fn ready(&self, generation: u64) -> ServiceFuture<()> {
        let health = self.health.clone();
        Box::pin(async move {
            health
                .write(
                    true,
                    "Collector storage, scripts and result routing are ready.",
                    Some(generation),
                )
                .await
        })
    }
}

struct Inner {
    engine: Engine,
    definitions: Definitions,
    service: ServiceClient,
    ingress: Option<Ingress>,
    health: HealthQueue,
    closing: CancellationToken,
    started_close: AtomicBool,
    done: watch::Sender<Option<IvyResult<()>>>,
    tasks: Mutex<Vec<JoinHandle<()>>>,
}
#[derive(Clone)]
pub struct Collector(Arc<Inner>);
impl Collector {
    pub fn engine(&self) -> &Engine {
        &self.0.engine
    }
    pub fn definitions(&self) -> &Definitions {
        &self.0.definitions
    }
    pub fn service(&self) -> &ServiceClient {
        &self.0.service
    }
    pub fn ingress_address(&self) -> Option<std::net::SocketAddr> {
        self.0.ingress.as_ref().map(Ingress::address)
    }
    fn begin_close(&self) {
        if self.0.started_close.swap(true, Ordering::AcqRel) {
            return;
        }
        self.0.closing.cancel();
        if let Some(ingress) = &self.0.ingress {
            ingress.begin_close();
        }
        self.0.engine.begin_close();
        let this = self.clone();
        // Caller cancellation cannot strand accepted cleanup halfway through.
        tokio::spawn(async move {
            if let Some(ingress) = &this.0.ingress {
                ingress.close().await;
            }
            let stopped = this.0.engine.clone();
            let engine_work = tokio::spawn(async move { stopped.close().await });
            this.0.service.stop().await;
            let tasks = std::mem::take(&mut *this.0.tasks.lock().unwrap());
            for task in tasks {
                let _ = task.await;
            }
            let engine_result = engine_work.await.map_err(|_| IvyError::internal());
            let health_result = this.0.health.finish().await;
            this.0
                .done
                .send_replace(Some(engine_result.and(health_result)));
        });
    }
    pub async fn close(&self) -> IvyResult<()> {
        self.begin_close();
        self.wait_closed().await
    }
    pub async fn wait_closed(&self) -> IvyResult<()> {
        let mut done = self.0.done.subscribe();
        loop {
            if let Some(result) = done.borrow_and_update().clone() {
                return result;
            }
            done.changed().await.map_err(|_| unavailable())?;
        }
    }
}

fn path_value(value: &IvyJson) -> IvyResult<PathBuf> {
    let text = value.as_js_string().ok_or_else(IvyError::internal)?;
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStringExt;
        Ok(std::ffi::OsString::from_wide(&text.0).into())
    }
    #[cfg(not(windows))]
    {
        Ok(String::from_utf16_lossy(&text.0).into())
    }
}
fn identity(config: &InstanceConfig) -> IvyResult<IvyJson> {
    let source = config.json();
    Ok(IvyJson::object([
        ("serviceNodeId", source.required("serviceNodeId")?.clone()),
        ("serviceName", "data-collector".into()),
        ("instanceMode", "multiple".into()),
        ("hostId", source.required("hostId")?.clone()),
        ("version", source.required("version")?.clone()),
        ("buildId", source.required("buildId")?.clone()),
        ("hiveProtocol", IvyJson::Number(1.0)),
    ]))
}

pub async fn start(path: PathBuf, options: StartOptions) -> IvyResult<Collector> {
    let node = NodeRuntime::discover(options.node_executable.as_deref()).await?;
    let config = config::instance_config(path, &node).await?;
    let settings = schema::parse_schema("settingsSchema", config.json().required("settings")?)?;
    if config.json().get("componentId") != Some(&"data-collector".into())
        || !config
            .json()
            .get("credential")
            .and_then(IvyJson::as_js_string)
            .is_some_and(|v| !v.0.is_empty())
    {
        return Err(IvyError::new(
            "invalid_arguments",
            "DataCollector needs its own component identity and credential.",
        ));
    }
    let credential = model::text(config.json(), "credential")?;
    let work_root = config
        .json()
        .get("workRoot")
        .map(path_value)
        .transpose()?
        .unwrap_or_else(|| config.data_root().to_owned())
        .join("tasks");
    let client: ClientSlot = Arc::new(Mutex::new(None));
    let current = client.clone();
    let mcp = Arc::new(NativeMcpFactory::new(Arc::new(move || {
        current.lock().unwrap().clone().ok_or_else(|| {
            IvyError::new(
                "provider_unavailable",
                "Hive connection is not ready for MCP calls.",
            )
        })
    })));
    let runner = Arc::new(TaskRunner::with_node_runtime(
        work_root,
        config.artifact_root().to_owned(),
        node,
        settings.clone(),
        options.job_launcher,
        Some(mcp),
    )?);
    let database = config.data_root().join("data-collector.sqlite");
    let now = engine::iso(SystemClock.utc_millis())?;
    let store = StoreOwner::new(
        tokio::task::spawn_blocking(move || {
            CollectorStore::open(&database, Limits::default(), &now)
        })
        .await
        .map_err(|_| IvyError::internal())??,
    );
    let engine = Engine::new(
        store.clone(),
        runner.clone(),
        settings,
        Arc::new(SystemClock),
    )
    .await?;
    let comparator = options.compare_names.clone();
    let definitions = Definitions::new(
        model::text(config.json(), "serviceNodeId")?,
        engine.clone(),
        Arc::new(move |id| runner.authentication_fingerprint(id, &comparator)),
        options.compare_names,
    );
    let health_file = Arc::new(HealthFile::new(config.clone()));
    let health = HealthQueue::new(health_file.clone(), store.clone());
    let backend = Arc::new(Backend {
        engine: engine.clone(),
        definitions: definitions.clone(),
        health: health.clone(),
        client,
    });
    let prepared = async {
        let service = provider::service(
            model::text(config.json(), "publicBaseUrl")?,
            Arc::new(move || {
                let value = credential.clone();
                Box::pin(async move { Ok(value) })
            }),
            identity(&config)?,
            backend,
        )?;
        let ingress = Ingress::open(engine.clone()).await?;
        Ok::<_, IvyError>((service, ingress))
    }
    .await;
    let (service, ingress) = match prepared {
        Ok(prepared) => prepared,
        Err(error) => {
            engine.close().await;
            let _ = health.finish().await;
            return Err(error);
        }
    };
    let collector = Collector(Arc::new(Inner {
        engine,
        definitions,
        service,
        ingress,
        health,
        closing: CancellationToken::new(),
        started_close: AtomicBool::new(false),
        done: watch::channel(None).0,
        tasks: Mutex::new(Vec::new()),
    }));
    if let Err(error) = collector
        .0
        .health
        .write(false, "Connecting DataCollector.", None)
        .await
    {
        let _ = collector.close().await;
        return Err(error);
    }
    let runtime = collector.clone();
    let timer = tokio::spawn(async move {
        let mut interval = tokio::time::interval_at(
            tokio::time::Instant::now() + Duration::from_millis(500),
            Duration::from_millis(500),
        );
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut tick: Option<JoinHandle<()>> = None;
        let mut sync: Option<JoinHandle<()>> = None;
        let mut next_sync = tokio::time::Instant::now();
        loop {
            tokio::select! { biased; _ = runtime.0.closing.cancelled() => break, _ = interval.tick() => {} }
            if runtime.0.service.ready()
                && sync.as_ref().is_none_or(JoinHandle::is_finished)
                && tokio::time::Instant::now() >= next_sync
            {
                next_sync = tokio::time::Instant::now() + Duration::from_secs(5);
                let definitions = runtime.0.definitions.clone();
                sync = Some(tokio::spawn(async move {
                    let _ = definitions.sync().await;
                }));
            }
            if tick.as_ref().is_none_or(JoinHandle::is_finished) {
                let engine = runtime.0.engine.clone();
                tick = Some(tokio::spawn(async move {
                    let _ = engine.tick(false).await;
                }));
            }
        }
        if let Some(tick) = tick {
            let _ = tick.await;
        }
        if let Some(sync) = sync {
            let _ = sync.await;
        }
    });
    let runtime = collector.clone();
    let control = tokio::spawn(async move {
        let mut interval = tokio::time::interval_at(
            tokio::time::Instant::now() + Duration::from_millis(500),
            Duration::from_millis(500),
        );
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! { biased; _ = runtime.0.closing.cancelled() => break, _ = interval.tick() => {} }
            if health_file.control().await.ok().flatten().is_some() {
                runtime.begin_close();
                break;
            }
        }
    });
    collector.0.tasks.lock().unwrap().extend([timer, control]);
    collector.0.service.start();
    Ok(collector)
}
