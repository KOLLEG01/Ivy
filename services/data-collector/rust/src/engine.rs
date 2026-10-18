//! Async owner for local execution, offline schedules and the ordered outbox.
use crate::{
    cache::ExecutionCache,
    model,
    publication::Publisher,
    runtime::{Context, Read, Runtime},
    schema,
    store_owner::StoreOwner,
};
use ivy_contracts::{IvyError, IvyJson, IvyResult};
use ivy_sdk::{
    clock::{Clock, TimeZone},
    rpc::{RequestOptions, RpcClient},
    service::{InvocationContext, ServiceFuture},
};
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicI64, Ordering},
    },
};
use tokio::{
    sync::{Mutex as AsyncMutex, Notify},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

pub type RegisterJob = Arc<dyn Fn(String) -> ServiceFuture<()> + Send + Sync>;
pub trait Runner: Send + Sync + 'static {
    /// Invoke registration and await its durable completion before either npm or
    /// the script can spawn. Return only after bounded process-tree retirement.
    fn run(
        &self,
        task: IvyJson,
        state: IvyJson,
        input: IvyJson,
        signal: CancellationToken,
        register: RegisterJob,
    ) -> ServiceFuture<IvyJson>;
    fn stopped(&self, task_id: String, job_name: String) -> ServiceFuture<bool>;
    /// Synchronous filesystem work is called on the blocking store owner.
    fn clear_authentication(&self, task_id: &str) -> IvyResult<()>;
}
struct Active {
    signal: CancellationToken,
    work: JoinHandle<()>,
}
#[derive(Default)]
struct Work {
    running: AtomicBool,
    done: Notify,
}
struct WorkGuard(Arc<Work>);
impl Drop for WorkGuard {
    fn drop(&mut self) {
        self.0.running.store(false, Ordering::Release);
        self.0.done.notify_waiters();
    }
}
impl Work {
    fn begin(self: &Arc<Self>) -> Option<WorkGuard> {
        self.running
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()
            .map(|_| WorkGuard(self.clone()))
    }
    async fn wait(&self) {
        loop {
            let notified = self.done.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if !self.running.load(Ordering::Acquire) {
                return;
            }
            notified.await;
        }
    }
}
struct Inner {
    store: StoreOwner,
    settings: IvyJson,
    runner: Arc<dyn Runner>,
    clock: Arc<dyn Clock>,
    client: Mutex<Option<Arc<dyn RpcClient>>>,
    root: Mutex<String>,
    publisher: AsyncMutex<Publisher>,
    active: Mutex<HashMap<String, Active>>,
    closing: CancellationToken,
    ticking: Arc<Work>,
    publication: Arc<Work>,
    maintenance: Arc<Work>,
    shutdown: Arc<Work>,
    next_recovery: AtomicI64,
    last_maintenance: AtomicI64,
    last_retention: AtomicI64,
}
#[derive(Clone)]
pub struct Engine(Arc<Inner>);
fn iso(ms: i64) -> IvyResult<String> {
    let p = TimeZone::local_parts(ms, "UTC")?;
    let year = if (0..=9999).contains(&p.year) {
        format!("{:04}", p.year)
    } else {
        format!("{:+07}", p.year)
    };
    Ok(format!(
        "{year}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        p.month,
        p.day,
        p.hour,
        p.minute,
        p.second,
        ms.rem_euclid(1000)
    ))
}
impl Engine {
    pub async fn new(
        store: StoreOwner,
        runner: Arc<dyn Runner>,
        settings: IvyJson,
        clock: Arc<dyn Clock>,
    ) -> IvyResult<Self> {
        let settings = schema::parse_schema("settingsSchema", &settings)?;
        let root = match model::optional_text(&settings, "rootObjectId") {
            Some(root) => root,
            None => store
                .call(|s| s.metadata("rootId"))
                .await?
                .unwrap_or_default(),
        };
        let publisher = Publisher::new(
            store.clone(),
            root.clone(),
            model::field(&settings, "retention")?.clone(),
        );
        Ok(Self(Arc::new(Inner {
            store,
            settings,
            runner,
            clock,
            client: Mutex::new(None),
            root: Mutex::new(root),
            publisher: AsyncMutex::new(publisher),
            active: Mutex::new(HashMap::new()),
            closing: CancellationToken::new(),
            ticking: Arc::default(),
            publication: Arc::default(),
            maintenance: Arc::default(),
            shutdown: Arc::default(),
            next_recovery: AtomicI64::new(0),
            last_maintenance: AtomicI64::new(0),
            last_retention: AtomicI64::new(0),
        })))
    }
    pub fn store(&self) -> StoreOwner {
        self.0.store.clone()
    }
    pub fn settings(&self) -> &IvyJson {
        &self.0.settings
    }
    pub fn root_id(&self) -> String {
        self.0.root.lock().unwrap().clone()
    }
    pub fn active(&self, id: &str) -> bool {
        self.0.active.lock().unwrap().contains_key(id)
    }
    pub fn client(&self) -> IvyResult<Arc<dyn RpcClient>> {
        self.0
            .client
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| IvyError::new("service_not_ready", "Hive connection is not ready."))
    }
    pub fn detach(&self) {
        *self.0.client.lock().unwrap() = None;
    }
    fn options(&self) -> RequestOptions {
        RequestOptions {
            cancellation: self.0.closing.clone(),
            ..Default::default()
        }
    }
    pub async fn attach(&self, client: Arc<dyn RpcClient>) -> IvyResult<()> {
        *self.0.client.lock().unwrap() = Some(client.clone());
        let mut publisher = self.0.publisher.lock().await;
        publisher
            .attach(
                client,
                model::optional_text(&self.0.settings, "rootObjectId"),
                &model::text(&self.0.settings, "rootName")?,
                self.options(),
            )
            .await?;
        *self.0.root.lock().unwrap() = publisher.root_id.clone();
        Ok(())
    }
    pub async fn read(&self, input: IvyJson) -> IvyResult<IvyJson> {
        let settings = self.0.settings.clone();
        let root = self.root_id();
        match self
            .0
            .store
            .call(move |store| {
                Runtime {
                    store,
                    settings: &settings,
                    root_id: &root,
                }
                .read(&input)
            })
            .await?
        {
            Read::Local(value) => Ok(value),
            Read::Remote { method, params } => {
                self.client()?.request(method, params, self.options()).await
            }
        }
    }
    pub async fn update(
        &self,
        input: IvyJson,
        context: Option<InvocationContext>,
    ) -> IvyResult<IvyJson> {
        let this = self.clone();
        let now = self.0.clock.utc_millis();
        let date = iso(now)?;
        let result = self
            .0
            .store
            .call(move |store| {
                let caller = context.as_ref().map(|c| Context {
                    caller: &c.caller_principal_id,
                    operation_id: c.operation_id.as_ref(),
                });
                Runtime {
                    store,
                    settings: &this.0.settings,
                    root_id: &this.root_id(),
                }
                .update(
                    &input,
                    caller,
                    now,
                    &date,
                    |id| this.active(id),
                    |id| this.0.runner.clear_authentication(id),
                )
            })
            .await?;
        if let Some(id) = result.disabled {
            if let Some(active) = self.0.active.lock().unwrap().get(&id) {
                active.signal.cancel();
            }
        }
        Ok(result.value)
    }
    pub async fn pending_publications(&self) -> IvyResult<usize> {
        self.0.store.call(|s| Ok(s.runs("publishing")?.len())).await
    }
    pub async fn flush(&self) {
        if self.0.closing.is_cancelled() {
            return;
        }
        if let (Ok(client), Some(guard)) = (self.client(), self.0.publication.begin()) {
            let this = self.clone();
            tokio::spawn(async move {
                let _guard = guard;
                let _ = this
                    .0
                    .publisher
                    .lock()
                    .await
                    .flush(client, this.options(), &this.0.closing)
                    .await;
            });
        }
        self.0.publication.wait().await;
    }
    pub async fn tick(&self, wait_for_publication: bool) -> IvyResult<()> {
        if self.0.closing.is_cancelled() {
            return Ok(());
        }
        let Some(_ticking) = self.0.ticking.begin() else {
            return Ok(());
        };
        self.recover_workers().await?;
        let now = self.0.clock.utc_millis();
        self.0
            .store
            .call(move |store| {
                let cache = ExecutionCache { store };
                for mut run in store.runs("publishing")? {
                    cache.commit_execution(&mut run, now as f64)?;
                }
                cache.schedule(now as f64, || uuid::Uuid::new_v4().to_string())?;
                Ok(())
            })
            .await?;
        let flush = self.clone();
        tokio::spawn(async move {
            flush.flush().await;
        });
        let queued = self.0.store.call(|store| store.runs("queued")).await?;
        for run in queued {
            let id = model::text(&run, "taskId")?;
            let mut active = self.0.active.lock().unwrap();
            if self.0.closing.is_cancelled()
                || active.len() as f64 >= model::number(&self.0.settings, "concurrency")?
            {
                break;
            }
            if active.contains_key(&id) {
                continue;
            }
            let signal = CancellationToken::new();
            let cancellation = signal.clone();
            let this = self.clone();
            let task_id = id.clone();
            let (start, started) = tokio::sync::oneshot::channel();
            let work = tokio::spawn(async move {
                let _ = started.await;
                let _ = this.execute(run, cancellation).await;
                this.0.active.lock().unwrap().remove(&task_id);
            });
            active.insert(id, Active { signal, work });
            let _ = start.send(());
        }
        if now - self.0.last_maintenance.load(Ordering::Acquire) > 60000 {
            self.0.last_maintenance.store(now, Ordering::Release);
            self.0.store.call(move |s| s.cleanup(now)).await?;
            if now - self.0.last_retention.load(Ordering::Acquire) >= 15 * 60000 {
                if let (Ok(client), Some(guard)) = (self.client(), self.0.maintenance.begin()) {
                    self.0.last_retention.store(now, Ordering::Release);
                    let this = self.clone();
                    tokio::spawn(async move {
                        let _guard = guard;
                        let _ = this.retention(client).await;
                    });
                }
            }
        }
        if wait_for_publication {
            self.flush().await;
            self.0.maintenance.wait().await;
        }
        Ok(())
    }
    async fn execute(&self, run: IvyJson, signal: CancellationToken) -> IvyResult<()> {
        let id = model::sqlite_key(model::field(&run, "id")?)?;
        let task_id = model::text(&run, "taskId")?;
        let date = iso(self.0.clock.utc_millis())?;
        let key = id.clone();
        let (running, state) = self
            .0
            .store
            .call(move |store| {
                let cache = ExecutionCache { store };
                let row = cache.task(&task_id)?;
                Ok((
                    cache.mark_running(&key, &date)?,
                    row.get("state").cloned().unwrap_or(IvyJson::Null),
                ))
            })
            .await?;
        let store = self.0.store.clone();
        let key = id.clone();
        let register: RegisterJob = Arc::new(move |name| {
            let store = store.clone();
            let key = key.clone();
            Box::pin(async move {
                store
                    .call(move |store| {
                        let mut run = store.run(&key)?.ok_or_else(IvyError::internal)?;
                        ExecutionCache { store }.register_job(&mut run, &name)
                    })
                    .await
            })
        });
        let outcome = self
            .0
            .runner
            .run(
                model::field(&running, "task")?.clone(),
                state,
                model::field(&running, "input")?.clone(),
                signal.clone(),
                register,
            )
            .await;
        let now = self.0.clock.utc_millis();
        let date = iso(now)?;
        let maximum = model::field(&self.0.settings, "retention")?.clone();
        self.0
            .store
            .call(move |store| {
                let cache = ExecutionCache { store };
                let mut run = store.run(&id)?.ok_or_else(IvyError::internal)?;
                let result = outcome.and_then(|output| {
                    if signal.is_cancelled() {
                        return Err(IvyError::new("cancelled", "Task was cancelled."));
                    }
                    let output = schema::parse_schema("outputSchema", &output)?;
                    run.insert("output", output.clone());
                    run.insert("finishedAt", date.clone().into());
                    crate::policy::validate_publication(&run, &maximum)?;
                    Ok(output)
                });
                match result {
                    Ok(output) => {
                        cache.commit_output(&mut run, output, &date, now as f64, &maximum)
                    }
                    Err(error) => cache.fail(&mut run, &error.code, signal.is_cancelled(), &date),
                }
            })
            .await
    }
    async fn recover_workers(&self) -> IvyResult<()> {
        if self.0.clock.utc_millis() < self.0.next_recovery.load(Ordering::Acquire) {
            return Ok(());
        }
        let rows = self
            .0
            .store
            .call(|store| {
                let cache = ExecutionCache { store };
                let mut found = Vec::new();
                for row in store.list(false)? {
                    if cache.unconfirmed(&row)? {
                        let id = model::optional_sqlite_key(&row, "lastRunId").unwrap();
                        if let Some(run) = store.run(&id)? {
                            if let Some(job) = model::optional_text(&run, "workerJobName")
                                .filter(|v| !v.is_empty())
                            {
                                found.push((model::text(&run, "taskId")?, id, job));
                            }
                        }
                    }
                }
                Ok(found)
            })
            .await?;
        for (task_id, run_id, job) in rows {
            if self.active(&task_id) {
                continue;
            }
            self.0
                .next_recovery
                .store(self.0.clock.utc_millis() + 2000, Ordering::Release);
            if self.0.runner.stopped(task_id.clone(), job.clone()).await != Ok(true) {
                continue;
            }
            let this = self.clone();
            let now = self.0.clock.utc_millis();
            self.0
                .store
                .call(move |store| {
                    if !this.active(&task_id) {
                        ExecutionCache { store }
                            .verified_stopped(&task_id, &run_id, &job, now as f64)?;
                    }
                    Ok(())
                })
                .await?;
        }
        Ok(())
    }
    async fn retention(&self, client: Arc<dyn RpcClient>) -> IvyResult<()> {
        let rows = self
            .0
            .store
            .call(|store| {
                let mut rows = Vec::new();
                for row in store.list(true)? {
                    if row.get("resultObjectId").is_some_and(|v| !v.is_null())
                        && !store.busy(&model::text(model::field(&row, "task")?, "id")?)?
                    {
                        rows.push(row);
                    }
                }
                Ok(rows)
            })
            .await?;
        for row in rows {
            if self.0.closing.is_cancelled() || self.client().is_err() {
                break;
            }
            let id = model::text(model::field(&row, "task")?, "id")?;
            self.0
                .publisher
                .lock()
                .await
                .retain(&id, client.clone(), self.options())
                .await?;
        }
        Ok(())
    }
    pub async fn close(&self) {
        let Some(_shutdown) = self.0.shutdown.begin() else {
            self.0.shutdown.wait().await;
            return;
        };
        self.0.closing.cancel();
        let active = std::mem::take(&mut *self.0.active.lock().unwrap());
        for task in active.values() {
            task.signal.cancel();
        }
        for (_, task) in active {
            let _ = task.work.await;
        }
        self.0.ticking.wait().await;
        self.0.publication.wait().await;
        self.0.maintenance.wait().await;
    }
}
