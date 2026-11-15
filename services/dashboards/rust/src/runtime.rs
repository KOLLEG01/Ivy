//! Provider lifecycle: generation-bound store, renderer readiness, serialized
//! health publication and identity-bound control-file shutdown.
use crate::{
    browser::{BrowserOptions, BrowserOwner},
    cdp::RenderInput,
    handlers::Handlers,
    store::DashboardStore,
};
use ivy_contracts::{IvyError, IvyJson as J, IvyResult, JsString};
use ivy_host_runtime::{config::InstanceConfig, health::HealthFile};
use ivy_sdk::{
    rpc::{RequestOptions, RpcClient},
    service::{ServiceClient, ServiceOptions},
};
use std::{
    ffi::OsString,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::{Notify, oneshot};
use tokio_util::sync::CancellationToken;

#[derive(Default)]
struct Gate {
    epoch: u64,
    closed: bool,
}
struct HealthWrite {
    ready: bool,
    detail: JsString,
    generation: Option<u64>,
    reply: Option<oneshot::Sender<IvyResult<()>>>,
    stop: bool,
}
#[derive(Default)]
struct HealthPending {
    write: Mutex<Option<HealthWrite>>,
    notify: Notify,
    closed: std::sync::atomic::AtomicBool,
}
#[derive(Clone)]
struct HealthQueue(Arc<HealthPending>);
impl HealthQueue {
    fn new(health: Arc<HealthFile>) -> Self {
        let pending = Arc::new(HealthPending::default());
        let worker = pending.clone();
        tokio::spawn(async move {
            loop {
                worker.notify.notified().await;
                loop {
                    let write = worker.write.lock().unwrap().take();
                    let Some(write) = write else {
                        break;
                    };
                    let result = health
                        .write(write.ready, write.detail, write.generation)
                        .await;
                    if let Some(reply) = write.reply {
                        let _ = reply.send(result);
                    }
                    if write.stop {
                        return;
                    }
                }
            }
        });
        Self(pending)
    }
    fn enqueue(
        &self,
        ready: bool,
        detail: JsString,
        generation: Option<u64>,
    ) -> IvyResult<oneshot::Receiver<IvyResult<()>>> {
        self.submit(ready, detail, generation, false)
    }
    fn submit(
        &self,
        ready: bool,
        detail: JsString,
        generation: Option<u64>,
        stop: bool,
    ) -> IvyResult<oneshot::Receiver<IvyResult<()>>> {
        use std::sync::atomic::Ordering;
        let mut pending = self.0.write.lock().unwrap();
        if self.0.closed.load(Ordering::Acquire) {
            return Err(IvyError::internal());
        }
        if stop {
            self.0.closed.store(true, Ordering::Release);
        }
        let (tx, rx) = oneshot::channel();
        let write = HealthWrite {
            ready,
            detail,
            generation,
            reply: Some(tx),
            stop,
        };
        // A bounded latest-state slot replaces obsolete writes. Publication stays
        // sequential; awaited shutdown is acknowledged only after its disk write.
        if let Some(previous) = pending.replace(write) {
            if let Some(reply) = previous.reply {
                let _ = reply.send(Err(IvyError::internal()));
            }
        }
        self.0.notify.notify_one();
        Ok(rx)
    }
    async fn write(&self, ready: bool, detail: &str, generation: Option<u64>) -> IvyResult<()> {
        self.enqueue(ready, detail.into(), generation)?
            .await
            .map_err(|_| IvyError::internal())?
    }
    async fn stop(&self) -> IvyResult<()> {
        self.submit(false, "Dashboards stopped.".into(), None, true)?
            .await
            .map_err(|_| IvyError::internal())?
    }
}
struct Inner {
    service: ServiceClient,
    browser: BrowserOwner,
    handlers: Handlers,
    health: HealthQueue,
    gate: Arc<Mutex<Gate>>,
    close: tokio::sync::Mutex<Option<IvyResult<()>>>,
    stopped: CancellationToken,
}
#[derive(Clone)]
pub struct DashboardRuntime(Arc<Inner>);
impl DashboardRuntime {
    pub fn service(&self) -> &ServiceClient {
        &self.0.service
    }
    pub async fn stopped(&self) {
        self.0.stopped.cancelled().await;
    }
    pub async fn close(&self) -> IvyResult<()> {
        let runtime = self.clone();
        tokio::spawn(async move {
            let mut result = runtime.0.close.lock().await;
            if let Some(result) = result.as_ref() {
                return result.clone();
            }
            {
                let mut gate = runtime.0.gate.lock().unwrap();
                gate.closed = true;
                gate.epoch += 1;
                runtime.0.handlers.set_store(None);
            }
            runtime.0.service.stop().await;
            // Each cleanup runs even if a previous step failed.
            let browser = runtime.0.browser.close().await;
            if browser.is_ok() {
                runtime.0.handlers.confirm_retirement();
                runtime.0.handlers.idle().await;
            }
            let health = runtime.0.health.stop().await;
            let value = browser.and(health);
            *result = Some(value.clone());
            runtime.0.stopped.cancel();
            value
        })
        .await
        .map_err(|_| IvyError::internal())?
    }
}

fn path(text: &JsString) -> PathBuf {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStringExt;
        OsString::from_wide(&text.0).into()
    }
    #[cfg(not(windows))]
    {
        String::from_utf16_lossy(&text.0).into()
    }
}
fn settings(value: &J) -> IvyResult<(Option<JsString>, Option<PathBuf>)> {
    let object = value.as_object().ok_or_else(IvyError::internal)?;
    if object.iter().any(|(key, _)| {
        key != &JsString::from("rootObjectId") && key != &JsString::from("chromiumExecutable")
    }) {
        return Err(IvyError::internal());
    }
    let root = match value.get("rootObjectId") {
        None | Some(J::Null) => None,
        Some(value) => Some(value.as_js_string().ok_or_else(IvyError::internal)?.clone()),
    };
    let executable = value
        .get("chromiumExecutable")
        .map(|value| {
            let text = value
                .as_js_string()
                .filter(|s| !s.0.is_empty())
                .ok_or_else(IvyError::internal)?;
            Ok(path(text))
        })
        .transpose()?;
    Ok((root, executable))
}

pub async fn start(config: InstanceConfig) -> IvyResult<DashboardRuntime> {
    let value = config.json();
    if value.get("componentId") != Some(&J::from("dashboards"))
        || value
            .get("credential")
            .and_then(J::as_js_string)
            .is_none_or(|v| v.0.is_empty())
    {
        return Err(IvyError::new(
            "invalid_arguments",
            "Dashboards requires its own identity and Hive credential.",
        ));
    }
    let (root, executable) = settings(value.required("settings")?)?;
    let browser = BrowserOwner::new(BrowserOptions {
        executable: executable.unwrap_or_default(),
        artifact_root: config.artifact_root().into(),
        temporary_root: std::env::temp_dir(),
        job_launcher: None,
    });
    let renderer = browser.clone();
    let handlers = Handlers::new(Arc::new(move |html, data, output, cancel| {
        let renderer = renderer.clone();
        Box::pin(async move {
            renderer
                .render(RenderInput::owned(html, data, output)?, cancel)
                .await
        })
    }));
    let health_file = Arc::new(HealthFile::new(config.clone()));
    let health = HealthQueue::new(health_file.clone());
    let gate = Arc::new(Mutex::new(Gate::default()));
    let base = value.required("publicBaseUrl")?.as_str()?;
    let node = value
        .required("serviceNodeId")?
        .as_js_string()
        .unwrap()
        .clone();
    let credential = value.required("credential")?.as_str()?;
    let identity = J::object([
        ("serviceNodeId", J::String(node.clone())),
        ("hostId", value.required("hostId")?.clone()),
        ("serviceName", "dashboards".into()),
        ("version", value.required("version")?.clone()),
        ("buildId", value.required("buildId")?.clone()),
        ("hiveProtocol", J::Number(1.0)),
    ]);
    let (reconcile_handlers, reconcile_gate, reconcile_base) =
        (handlers.clone(), gate.clone(), base.clone());
    let mut options = ServiceOptions::new(
        base,
        Arc::new(move || {
            let credential = credential.clone();
            Box::pin(async move { Ok(credential) })
        }),
        identity,
        Arc::new(|| Box::pin(async { Ok(crate::registry()) })),
        Arc::new(move |connection| {
            let (handlers, gate, root, base, node) = (
                reconcile_handlers.clone(),
                reconcile_gate.clone(),
                root.clone(),
                reconcile_base.clone(),
                node.clone(),
            );
            Box::pin(async move {
                let epoch = gate.lock().unwrap().epoch;
                if let Some(root) = root.as_ref().filter(|r| !r.0.is_empty()) {
                    connection
                        .request(
                            "objects.stat",
                            J::object([("objectId", J::String(root.clone()))]),
                            RequestOptions::default(),
                        )
                        .await?;
                }
                let current = gate.lock().unwrap();
                if current.closed || current.epoch != epoch || connection.signal().is_cancelled() {
                    return Err(IvyError::internal());
                }
                handlers.set_store(Some(DashboardStore::new(
                    Arc::new(connection),
                    root,
                    &base,
                    node,
                )?));
                Ok(())
            })
        }),
    );
    options.handlers = handlers.map();
    options.heartbeat = Duration::from_secs(2);
    let (ready_health, ready_gate) = (health.clone(), gate.clone());
    options.readiness = Some(Arc::new(move |connection| {
        let (health, gate) = (ready_health.clone(), ready_gate.clone());
        Box::pin(async move {
            let reply = {
                let current = gate.lock().unwrap();
                if current.closed || connection.signal().is_cancelled() {
                    return Err(IvyError::internal());
                }
                health.enqueue(
                    true,
                    "Dashboard storage ready.".into(),
                    Some(connection.generation()),
                )?
            };
            reply.await.map_err(|_| IvyError::internal())??;
            Ok(J::object([
                ("ready", true.into()),
                ("diagnostics", J::Array(vec![])),
            ]))
        })
    }));
    let (state_handlers, state_health, state_gate) =
        (handlers.clone(), health.clone(), gate.clone());
    options.on_state = Some(Arc::new(move |state| {
        if ["offline", "stopped", "connecting"].contains(&state.status) {
            let mut gate = state_gate.lock().unwrap();
            gate.epoch += 1;
            state_handlers.set_store(None);
            let _ = state_health.enqueue(
                false,
                state
                    .message
                    .unwrap_or_else(|| "Hive connection unavailable.".into()),
                None,
            );
        }
    }));
    let service = ServiceClient::new(options)?;
    health.write(false, "Connecting Dashboards.", None).await?;
    let runtime = DashboardRuntime(Arc::new(Inner {
        service,
        browser,
        handlers,
        health,
        gate,
        close: tokio::sync::Mutex::new(None),
        stopped: CancellationToken::new(),
    }));
    let control = Arc::downgrade(&runtime.0);
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_millis(500)).await;
            let Some(inner) = control.upgrade() else {
                break;
            };
            if inner.gate.lock().unwrap().closed {
                break;
            }
            if matches!(health_file.control().await, Ok(Some(_))) {
                let _ = DashboardRuntime(inner).close().await;
                break;
            }
        }
    });
    runtime.0.service.start();
    Ok(runtime)
}
