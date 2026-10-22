use super::*;
use crate::{
    bootstrap_restart::BootstrapConfigurationRestarts,
    bootstrap_automatic::{AutomaticBootstrapRuntime, AutomaticBootstrapUpdater},
    configuration_updater::{self, ConfigurationUpdater},
    executor_lock::ExecutorLock,
    package_updater::{PackageClient, PackageUpdater},
    scheduler,
};
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
    status: StatusWriter,
    configuration: Option<ConfigurationUpdater>,
    bootstrap_restarts: Option<BootstrapConfigurationRestarts>,
    packages: Option<(PackageUpdater, AutomaticBootstrapUpdater)>,
}
/// Explicit service bindings for the independent maintenance owner, locale
/// comparison, Node qualification and the selected HTTP transport profile.
pub struct PackageUpdateRuntime {
    pub node: crate::node_runtime::NodeRuntime,
    pub client: Arc<dyn PackageClient>,
    pub automatic: Arc<dyn AutomaticBootstrapRuntime>,
    pub compare: crate::artifact::FileCompare,
    pub distribution: std::path::PathBuf,
    pub job_launcher: std::path::PathBuf,
}
impl PackageUpdateRuntime {
    /// Compose the real independent maintenance owner while retaining the
    /// caller's explicitly selected package transport and locale bindings.
    pub fn native(
        node: crate::node_runtime::NodeRuntime,
        client: Arc<dyn PackageClient>,
        compare: crate::artifact::FileCompare,
        distribution: std::path::PathBuf,
        job_launcher: std::path::PathBuf,
    ) -> IvyResult<Self> {
        let os = crate::bootstrap_os::bootstrap_os(
            distribution.clone(),
            job_launcher.clone(),
            node.clone(),
        )?;
        let maintenance = crate::bootstrap_maintenance::BootstrapMaintenance::new(
            node.clone(),
            compare.clone(),
            os,
            job_launcher.clone(),
        );
        Ok(Self {
            node,
            client,
            compare,
            distribution,
            job_launcher,
            automatic: Arc::new(crate::bootstrap_automatic::NativeAutomaticBootstrapRuntime(
                maintenance,
            )),
        })
    }
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
        Self::open_inner(config, options, runtime, clock, None, None, None).await
    }
    pub async fn open_with_configuration(
        config: IvyJson,
        config_path: std::path::PathBuf,
        credential: Option<JsString>,
        options: ExecutorOptions,
        runtime: Arc<dyn ActivationRuntime>,
    ) -> IvyResult<Self> {
        Self::open_with_configuration_and_clock(
            config,
            config_path,
            credential,
            options,
            runtime,
            Arc::new(SystemClock),
        )
        .await
    }
    pub async fn open_with_configuration_and_clock(
        config: IvyJson,
        config_path: std::path::PathBuf,
        credential: Option<JsString>,
        options: ExecutorOptions,
        runtime: Arc<dyn ActivationRuntime>,
        clock: Arc<dyn Clock>,
    ) -> IvyResult<Self> {
        Self::open_configuration(config, config_path, credential, options, runtime, clock, None, None).await
    }
    /// Service startup supplies its actual process start time and the immutable
    /// scheduler helper through the reconciler. Existing core constructors do
    /// not manufacture a process generation or an OS-owner fallback.
    pub async fn open_with_configuration_and_restarts(
        config: IvyJson,
        config_path: std::path::PathBuf,
        credential: Option<JsString>,
        options: ExecutorOptions,
        runtime: Arc<dyn ActivationRuntime>,
        clock: Arc<dyn Clock>,
        bootstrap_restarts: BootstrapConfigurationRestarts,
    ) -> IvyResult<Self> {
        Self::open_configuration(config, config_path, credential, options, runtime, clock, Some(bootstrap_restarts), None).await
    }
    pub async fn open_with_updates(
        config: IvyJson,
        config_path: std::path::PathBuf,
        credential: Option<JsString>,
        options: ExecutorOptions,
        runtime: Arc<dyn ActivationRuntime>,
        clock: Arc<dyn Clock>,
        bootstrap_restarts: Option<BootstrapConfigurationRestarts>,
        packages: PackageUpdateRuntime,
    ) -> IvyResult<Self> {
        Self::open_configuration(config, config_path, credential, options, runtime, clock, bootstrap_restarts, Some(packages)).await
    }
    async fn open_configuration(
        config: IvyJson,
        config_path: std::path::PathBuf,
        credential: Option<JsString>,
        options: ExecutorOptions,
        runtime: Arc<dyn ActivationRuntime>,
        clock: Arc<dyn Clock>,
        bootstrap_restarts: Option<BootstrapConfigurationRestarts>,
        packages: Option<PackageUpdateRuntime>,
    ) -> IvyResult<Self> {
        let credential = configuration_updater::update_credential(
            &config,
            options.own_instance_id.as_deref(),
            credential.as_ref(),
        )?;
        let credential = credential.map(|value| value.to_utf8()).transpose()?;
        Self::open_inner(
            config,
            options,
            runtime,
            clock,
            Some((config_path, credential)),
            bootstrap_restarts,
            packages,
        )
        .await
    }
    async fn open_inner(
        config: IvyJson,
        options: ExecutorOptions,
        runtime: Arc<dyn ActivationRuntime>,
        clock: Arc<dyn Clock>,
        configuration: Option<(std::path::PathBuf, Option<String>)>,
        bootstrap_restarts: Option<BootstrapConfigurationRestarts>,
        packages: Option<PackageUpdateRuntime>,
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
        let packages = match (&configuration, packages) {
            (Some((path, Some(credential))), Some(bindings)) if !credential.is_empty()
                && base.config().await?.get("packageUpdates").is_some_and(truthy) => Some((
                PackageUpdater::new(base.clone(), credential.clone(), bindings.node, bindings.client),
                AutomaticBootstrapUpdater::with_clock(path.clone(), bindings.distribution, base.clone(),
                    bindings.automatic, bindings.compare, bindings.job_launcher, clock.clone()),
            )),
            _ => None,
        };
        let configuration = match configuration {
            Some((path, Some(credential))) if !credential.is_empty() => {
                match ConfigurationUpdater::new(path, base.clone(), credential).await {
                    Ok(updater) => Some(updater),
                    Err(error) => {
                        let _ = base.close().await;
                        let _ = lock.close().await;
                        return Err(error);
                    }
                }
            }
            _ => None,
        };
        let status = StatusWriter {
            base: base.clone(),
            clock: clock.clone(),
            build_id: options.build_id.clone(),
            boot_id: uuid::Uuid::new_v4().to_string(),
            value: Arc::new(Mutex::new(Status::default())),
            write: Arc::new(tokio::sync::Mutex::new(())),
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
                status,
                configuration,
                bootstrap_restarts,
                packages,
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

#[derive(Clone, Default)]
struct Status {
    active: Vec<String>,
    configuration_code: Option<JsString>,
    package_code: Option<JsString>,
    revision: Option<IvyJson>,
    hash: Option<IvyJson>,
}
#[derive(Clone)]
struct StatusWriter {
    base: HostJournal,
    clock: Arc<dyn Clock>,
    build_id: Option<JsString>,
    boot_id: String,
    value: Arc<Mutex<Status>>,
    write: Arc<tokio::sync::Mutex<()>>,
}

impl StatusWriter {
    async fn write(&self, state: &str) -> IvyResult<()> {
        let _write = self.write.lock().await;
        let snapshot = self.value.lock().map_err(|_| IvyError::internal())?.clone();
        let host = self.base.config().await?;
        let mut code = snapshot
            .configuration_code
            .or(snapshot.package_code)
            .map(IvyJson::String)
            .unwrap_or(IvyJson::Null);
        if state == "ready" && (code.is_null() || code == "storage_retention_attention".into()) {
            if let Some(retention) = self
                .base
                .storage_retention_status()
                .await?
                .filter(|v| field(v, "state") != &"running".into())
            {
                code = if field(&retention, "state") == &"succeeded".into() {
                    IvyJson::Null
                } else {
                    "storage_retention_attention".into()
                };
            }
        }
        if state != "ready" {
            code = IvyJson::Null;
        }
        let mut value = IvyJson::object([
            ("schemaVersion", IvyJson::Number(1.0)),
            ("hostId", host.required("hostId")?.clone()),
            ("pid", IvyJson::Number(std::process::id() as f64)),
            ("bootId", self.boot_id.clone().into()),
            ("observedAt", stamp(self.clock.as_ref())?),
            ("state", state.into()),
            (
                "activeDeploymentId",
                snapshot
                    .active
                    .first()
                    .cloned()
                    .map(IvyJson::from)
                    .unwrap_or(IvyJson::Null),
            ),
            ("code", code),
            (
                "buildId",
                self.build_id
                    .clone()
                    .map(IvyJson::String)
                    .unwrap_or(IvyJson::Null),
            ),
        ]);
        if let Some((revision, hash)) = snapshot.revision.zip(snapshot.hash) {
            value.insert("configurationRevision", revision);
            value.insert("configurationHash", hash);
        }
        validate_host("ExecutorStatus", &value)?;
        config::atomic_json(
            config::path_value(host.required("runtimeRoot")?)?.join("executor.json"),
            value,
        )
        .await
    }
}
impl Coordinator {
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
        let pulse_cancel = CancellationToken::new();
        let mut pulse = None;
        let run=std::panic::AssertUnwindSafe(async {
            self.status.write("reconciling").await?;
            let status=self.status.clone();let stopped=pulse_cancel.clone();
            pulse=Some(tokio::spawn(async move{
                let mut ticks=tokio::time::interval_at(tokio::time::Instant::now()+std::time::Duration::from_secs(1),std::time::Duration::from_secs(1));
                ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                loop{tokio::select!{biased;_=stopped.cancelled()=>break,_=ticks.tick()=>{let _=status.write("ready").await;}}}
            }));
            let mut next_configuration=0.0;let mut next_bootstrap_restart=0.0;let mut next_package=0.0;let mut last_hint=String::new();
            while !self.cancel.is_cancelled(){
                if self.configuration.is_some() || self.packages.is_some() {
                    let config=self.base.config().await?;
                    let hint=config::json_file(config::path_value(config.required("runtimeRoot")?)?.join("desired-state-hint.json"),config::CONFIGURATION_BYTES).await.ok();
                    if let Some(hint)=hint.filter(|v|v.get("schemaVersion")==Some(&IvyJson::Number(1.0))){
                        let identity=json::stringify(&hint,usize::MAX)?;
                        if identity!=last_hint {last_hint=identity;if truthy(field(&hint,"configuration")){next_configuration=0.0;}if truthy(field(&hint,"packages")){next_package=0.0;}}
                    }
                }
                if let Some(updater)=&self.configuration {
                    if self.clock.utc_millis() as f64>=next_configuration {
                        let result=updater.sync().await;
                        {
                            let mut status=self.status.value.lock().map_err(|_|IvyError::internal())?;
                            match result {
                                Ok(value)=>{if !value.revision.is_null()&&!value.content_hash.is_null(){status.revision=Some(value.revision);status.hash=Some(value.content_hash);}status.configuration_code=None;}
                                Err(error)=>status.configuration_code=Some(error.code_js()),
                            }
                        }
                        let config=self.base.config().await?;
                        let interval=config.get("configurationUpdates").and_then(|v|v.get("intervalSeconds")).filter(|v|!v.is_null()).and_then(IvyJson::as_f64).unwrap_or(60.0);
                        next_configuration=self.clock.utc_millis() as f64+interval*1000.0;
                    }
                    if let Some(restarts)=&self.bootstrap_restarts
                        && self.clock.utc_millis() as f64>=next_bootstrap_restart {
                        next_bootstrap_restart=self.clock.utc_millis() as f64+5000.0;
                        match restarts.reconcile(self.base.config().await?).await {
                            Ok(true)=>{self.cancel.cancel();break;}
                            Ok(false)=>{},
                            Err(error)=>self.status.value.lock().map_err(|_|IvyError::internal())?.configuration_code=Some(error.code_js()),
                        }
                    }
                }
                if let Some((updater,automatic))=&self.packages && self.clock.utc_millis() as f64>=next_package {
                    let config=self.base.config().await?;
                    next_package=self.clock.utc_millis() as f64+config.required("packageUpdates")?.required("intervalSeconds")?.as_f64().ok_or_else(IvyError::internal)?*1000.0;
                    let result=async {
                        let result=updater.sync().await?;
                        if !result.bootstrap.is_empty(){automatic.reconcile(result.bootstrap).await?;}
                        Ok::<(),IvyError>(())
                    }.await;
                    self.status.value.lock().map_err(|_|IvyError::internal())?.package_code=result.err().map(|error|error.code_js());
                }
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
                    let id=text(field(&entry,"record"),"deploymentId")?;
                    self.status.value.lock().map_err(|_|IvyError::internal())?.active.push(id.clone());
                    let status=ActiveStatus{value:self.status.value.clone(),id};
                    let task=tokio::spawn(async move{let _status=status;execute_with_clock(base,input,runtime,options,cancel,clock).await});
                    active.push(Active{entry,task});
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
        pulse_cancel.cancel();
        if let Some(pulse) = pulse {
            let _ = pulse.await;
        }
        if let Ok(mut status) = self.status.value.lock() {
            status.active.clear();
        }
        for result in [
            self.runtime.dispose().await,
            self.status.write("stopping").await,
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
struct ActiveStatus {
    value: Arc<Mutex<Status>>,
    id: String,
}
impl Drop for ActiveStatus {
    fn drop(&mut self) {
        if let Ok(mut status) = self.value.lock() {
            status.active.retain(|id| id != &self.id);
        }
    }
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
