use super::*;
use crate::{executor_lock::ExecutorLock, scheduler};
use futures_util::FutureExt;
use ivy_contracts::core_schema::validate_host;
use std::sync::Mutex;
use tokio::{sync::watch, task::JoinHandle};

struct Coordinator {
    base: HostJournal,
    lock: ExecutorLock,
    runtime: Arc<dyn ActivationRuntime>,
    options: ExecutorOptions,
    cancel: CancellationToken,
    clock: Arc<dyn Clock>,
    boot_id: String,
}
struct Control {
    pending: Mutex<Option<Coordinator>>,
    cancel: CancellationToken,
    result: watch::Receiver<Option<IvyResult<()>>>,
    result_sender: watch::Sender<Option<IvyResult<()>>>,
}
impl Drop for Control {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

/// The activation coordinator holds the host lock, keeps two disjoint lanes,
/// and drains accepted work before releasing ownership. Polling/configuration/
/// retention services compose around this core; they do not share lane context.
#[derive(Clone)]
pub struct HostExecutor(Arc<Control>);
impl HostExecutor {
    pub async fn open(
        config: IvyJson,
        options: ExecutorOptions,
        runtime: Arc<dyn ActivationRuntime>,
    ) -> IvyResult<Self> {
        Self::open_with_clock(config, options, runtime, Arc::new(SystemClock)).await
    }
    pub async fn open_with_clock(
        config: IvyJson,
        options: ExecutorOptions,
        runtime: Arc<dyn ActivationRuntime>,
        clock: Arc<dyn Clock>,
    ) -> IvyResult<Self> {
        let lock = ExecutorLock::acquire(
            config::path_value(config.required("runtimeRoot")?)?.join("executor"),
            false,
        )
        .await?;
        let base = match HostJournal::open_with_clock(config, clock.clone()).await {
            Ok(base) => base,
            Err(error) => {
                let _ = lock.close().await;
                return Err(error);
            }
        };
        let cancel = CancellationToken::new();
        let (sender, result) = watch::channel(None);
        Ok(Self(Arc::new(Control {
            pending: Mutex::new(Some(Coordinator {
                base,
                lock,
                runtime,
                options,
                cancel: cancel.clone(),
                clock,
                boot_id: uuid::Uuid::new_v4().to_string(),
            })),
            cancel,
            result,
            result_sender: sender,
        })))
    }
    pub fn start(&self) -> IvyResult<()> {
        let Some(owner) = self
            .0
            .pending
            .lock()
            .map_err(|_| IvyError::internal())?
            .take()
        else {
            return Ok(());
        };
        let sender = self.0.result_sender.clone();
        tokio::spawn(async move {
            let result = tokio::spawn(owner.run())
                .await
                .map_err(|_| IvyError::internal())
                .and_then(|result| result);
            let _ = sender.send(Some(result));
        });
        Ok(())
    }
    pub async fn completion(&self) -> IvyResult<()> {
        let mut result = self.0.result.clone();
        loop {
            if let Some(value) = result.borrow_and_update().clone() {
                return value;
            }
            if result.changed().await.is_err() {
                return Err(IvyError::internal());
            }
        }
    }
    /// Start even an unopened worker so it releases its owned resources. All
    /// close waiters observe the same completion; dropping one cannot abort it.
    pub async fn close(&self) -> IvyResult<()> {
        self.0.cancel.cancel();
        self.start()?;
        self.completion().await
    }
}
struct Active {
    entry: IvyJson,
    task: JoinHandle<IvyResult<()>>,
}

impl Coordinator {
    async fn status(&self, state: &str, active: Option<String>) -> IvyResult<()> {
        let host = self.base.config().await?;
        let code = if state == "ready" {
            self.base
                .storage_retention_status()
                .await?
                .filter(|v| {
                    field(v, "state") != &"running".into()
                        && field(v, "state") != &"succeeded".into()
                })
                .map(|_| IvyJson::from("storage_retention_attention"))
                .unwrap_or(IvyJson::Null)
        } else {
            IvyJson::Null
        };
        let value = IvyJson::object([
            ("schemaVersion", IvyJson::Number(1.0)),
            ("hostId", host.required("hostId")?.clone()),
            ("pid", IvyJson::Number(std::process::id() as f64)),
            ("bootId", self.boot_id.clone().into()),
            ("observedAt", stamp(self.clock.as_ref())?),
            ("state", state.into()),
            (
                "activeDeploymentId",
                active.map(IvyJson::from).unwrap_or(IvyJson::Null),
            ),
            ("code", code),
            (
                "buildId",
                self.options
                    .build_id
                    .clone()
                    .map(IvyJson::String)
                    .unwrap_or(IvyJson::Null),
            ),
        ]);
        validate_host("ExecutorStatus", &value)?;
        config::atomic_json(
            config::path_value(host.required("runtimeRoot")?)?.join("executor.json"),
            value,
        )
        .await
    }
    async fn exclusive(&self, entry: &IvyJson) -> IvyResult<bool> {
        let record = entry.required("record")?;
        // Keep short-circuit/error order: source/HostExecutor need no manifest.
        if field(record, "phase") == &"preparing".into()
            || field(record, "componentId") == &"host-executor".into()
        {
            return Ok(true);
        }
        match selected(entry, "candidateId") {
            Some(candidate) => Ok(
                field(&self.base.manifest(&candidate.as_str()?).await?, "kind") == &"hive".into(),
            ),
            None => Ok(field(record, "componentId") == &"hive".into()),
        }
    }
    async fn selection(&self, queue: &[IvyJson], active: &[Active]) -> IvyResult<Vec<usize>> {
        let mut running = Vec::new();
        for lane in active {
            let entry = &lane.entry;
            if self.exclusive(entry).await? {
                return Ok(Vec::new());
            }
            running.push((
                text(entry.required("record")?, "deploymentId")?,
                text(entry.required("record")?, "instanceId")?,
                false,
            ));
        }
        fn view(values: &[(String, String, bool)]) -> Vec<scheduler::Deployment<'_>> {
            values
                .iter()
                .map(|(id, instance, exclusive)| scheduler::Deployment {
                    deployment_id: id,
                    instance_id: instance,
                    exclusive: *exclusive,
                })
                .collect::<Vec<_>>()
        }
        let active_view = view(&running);
        let mut admission = scheduler::Admission::new(&active_view, 2);
        let headers = queue
            .iter()
            .map(|entry| {
                Ok((
                    text(entry.required("record")?, "deploymentId")?,
                    text(entry.required("record")?, "instanceId")?,
                ))
            })
            .collect::<IvyResult<Vec<_>>>()?;
        let mut selected = Vec::new();
        for (index, (id, instance)) in headers.iter().enumerate() {
            if admission.ignores(id) {
                continue;
            }
            let exclusive = self.exclusive(&queue[index]).await?;
            if admission.consider(scheduler::Deployment {
                deployment_id: id,
                instance_id: instance,
                exclusive,
            }) {
                selected.push(index);
            }
            if admission.stopped() {
                break;
            }
        }
        Ok(selected)
    }
    async fn run(self) -> IvyResult<()> {
        let mut active: Vec<Active> = Vec::new();
        let mut failure = None;
        let run=std::panic::AssertUnwindSafe(async {
            self.status("reconciling",None).await?;
            let mut next_status=self.clock.monotonic();
            while !self.cancel.is_cancelled(){
                let mut index=0;
                while index<active.len(){
                    if active[index].task.is_finished(){
                        let lane=active.remove(index);
                        lane.task.await.map_err(|_|IvyError::internal())??;
                    }else{index+=1;}
                }
                let queued=self.base.unfinished().await?;
                for index in self.selection(&queued,&active).await? {
                    let entry=queued[index].clone();let base=self.base.clone();let runtime=self.runtime.clone();let options=self.options.clone();let cancel=self.cancel.clone();let clock=self.clock.clone();let input=entry.clone();
                    let task=tokio::spawn(async move{execute_with_clock(base,input,runtime,options,cancel,clock).await});
                    active.push(Active{entry,task});
                }
                if self.clock.monotonic()>=next_status{
                    self.status("ready",active.first().map(|a|text(field(&a.entry,"record"),"deploymentId")).transpose()?).await?;
                    next_status=self.clock.monotonic()+std::time::Duration::from_secs(1);
                }
                tokio::select!{_=self.cancel.cancelled()=>break,_=tokio::time::sleep(std::time::Duration::from_millis(250))=>{}}
            }
            Ok::<(),IvyError>(())
        }).catch_unwind().await.map_err(|_|IvyError::internal()).and_then(|result|result);
        if let Err(error) = run {
            failure = Some(error);
            self.cancel.cancel();
        }
        for lane in active {
            let result = lane
                .task
                .await
                .map_err(|_| IvyError::internal())
                .and_then(|result| result);
            if failure.is_none() {
                failure = result.err();
            }
        }
        for result in [
            self.runtime.dispose().await,
            self.status("stopping", None).await,
            self.base.close().await,
            self.lock.close().await,
        ] {
            if failure.is_none() {
                failure = result.err();
            }
        }
        failure.map_or(Ok(()), Err)
    }
}
