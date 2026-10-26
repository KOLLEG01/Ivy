//! Startup, recovery, shutdown and credential changes on the network executor.
//! HTTP/WS listening and runtime error policy remain explicit owners; no listener
//! or production-runtime fallback is constructed here.
use crate::{
    admission::DispatchGate,
    browser_push::BrowserPush,
    oauth::{HiveOAuth, Secrets},
    packages::PackageRegistry,
    server_peers::{Peers, UnhandledKind},
    server_rpc::{Reply, Storage},
    store::Credential,
    worker_client::RequestOptions,
    worker_protocol::Notification,
    worker_runtime::{Event, EventSink, StorageClient},
};
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{sync::oneshot, task::AbortHandle, time::Instant};

pub trait StorageLifecycle: Storage {
    fn start(&self) -> Reply<()>;
    fn close(&self) -> Reply<()>;
}
impl StorageLifecycle for StorageClient {
    fn start(&self) -> Reply<()> {
        let waiter = StorageClient::start(self);
        Box::pin(async move { waiter.wait().await })
    }
    fn close(&self) -> Reply<()> {
        let waiter = StorageClient::close(self);
        Box::pin(async move { waiter.wait().await })
    }
}
/// listen observes the actual bind/error callback. close observes its completion;
/// as in the source, its callback error does not prevent storage shutdown.
pub trait Listener: Send + Sync {
    fn listen(&self, port: f64, host: JsString) -> Reply<()>;
    fn address(&self) -> IvyResult<IvyJson>;
    fn close_websockets(&self) -> IvyResult<()>;
    fn close_connections(&self) -> IvyResult<()>;
    fn close(&self) -> Reply<()>;
}
pub trait Packages: Send + Sync {
    fn initialize(&self) -> Reply<()>;
    fn collect_artifacts(&self) -> Reply<()>;
}
impl Packages for PackageRegistry {
    fn initialize(&self) -> Reply<()> {
        let owner = self.clone();
        Box::pin(async move { PackageRegistry::initialize(&owner).await })
    }
    fn collect_artifacts(&self) -> Reply<()> {
        let owner = self.clone();
        Box::pin(async move { PackageRegistry::collect_artifacts(&owner).await })
    }
}
pub trait OAuth: Send + Sync {
    fn initialize(&self, value: &IvyJson) -> IvyResult<()>;
    fn set_credentials(&self, credentials: Vec<Credential>) -> IvyResult<()>;
}
impl OAuth for HiveOAuth {
    fn initialize(&self, value: &IvyJson) -> IvyResult<()> {
        // These are the storage owner's typed oauth.initialize reply fields.
        // The canonical OAuth owner applies its existing key policy.
        HiveOAuth::initialize(
            self,
            &Secrets {
                signing_key: value
                    .get("signingKey")
                    .and_then(IvyJson::as_js_string)
                    .cloned()
                    .unwrap_or_default(),
                reference_key: value
                    .get("referenceKey")
                    .and_then(IvyJson::as_js_string)
                    .cloned()
                    .unwrap_or_default(),
            },
        )
    }
    fn set_credentials(&self, credentials: Vec<Credential>) -> IvyResult<()> {
        HiveOAuth::set_credentials(self, credentials);
        Ok(())
    }
}
pub trait Push: Send + Sync {
    fn start(&self) -> IvyResult<()>;
    fn close(&self) -> Reply<()>;
}
impl Push for BrowserPush {
    fn start(&self) -> IvyResult<()> {
        BrowserPush::start(self);
        Ok(())
    }
    fn close(&self) -> Reply<()> {
        let close = BrowserPush::close(self);
        Box::pin(async move {
            close.await;
            Ok(())
        })
    }
}
pub type Immediate = Arc<dyn Fn(Box<dyn FnOnce() + Send>) + Send + Sync>;
pub type Unhandled = Arc<dyn Fn(UnhandledKind, IvyError) + Send + Sync>;
pub struct Owners {
    pub storage: Arc<dyn StorageLifecycle>,
    /// Shares routing, diagnostics, HTTP authentication reset and push wake with
    /// the actual request/connection composition.
    pub peers: Peers,
    pub dispatch: DispatchGate,
    pub oauth: Arc<dyn OAuth>,
    pub push: Arc<dyn Push>,
    pub packages: Option<Arc<dyn Packages>>,
    pub listener: Arc<dyn Listener>,
    /// The real network executor schedules its setImmediate-equivalent turn.
    pub immediate: Immediate,
    pub unhandled: Unhandled,
}
#[derive(Default)]
pub struct Options {
    pub listen_host: Option<JsString>,
    pub listen_port: Option<f64>,
    pub credentials: Vec<Credential>,
}
struct State {
    credentials: Vec<Credential>,
    stopped: bool,
    health: Option<AbortHandle>,
    recovery: Option<AbortHandle>,
}
struct Parts {
    owners: Owners,
    listen_host: Option<JsString>,
    listen_port: Option<f64>,
    state: Mutex<State>,
}
#[derive(Clone)]
pub struct Lifecycle(Arc<Parts>);
fn retained<T: Send + 'static>(
    work: impl std::future::Future<Output = IvyResult<T>> + Send + 'static,
) -> Reply<T> {
    let (sender, receiver) = oneshot::channel();
    tokio::spawn(async move {
        let _ = sender.send(work.await);
    });
    Box::pin(async move { receiver.await.map_err(|_| IvyError::internal())? })
}
fn credentials_json(credentials: &[Credential]) -> IvyJson {
    IvyJson::Array(
        credentials
            .iter()
            .map(|c| {
                IvyJson::object([
                    ("principalId", IvyJson::String(c.principal_id.clone())),
                    ("digest", IvyJson::String(c.digest.clone())),
                ])
            })
            .collect(),
    )
}
impl Lifecycle {
    /// Entry points and owner callbacks run on the single network executor.
    /// Abandoning a returned future never cancels accepted lifecycle work.
    pub fn new(owners: Owners, options: Options) -> Self {
        Self(Arc::new(Parts {
            owners,
            listen_host: options.listen_host,
            listen_port: options.listen_port,
            state: Mutex::new(State {
                credentials: options.credentials,
                stopped: false,
                health: None,
                recovery: None,
            }),
        }))
    }
    pub fn credentials(&self) -> Vec<Credential> {
        self.0.state.lock().unwrap().credentials.clone()
    }
    fn stop(&self) {
        self.0.state.lock().unwrap().stopped = true;
        self.0.owners.peers.stop_publication();
    }
    fn cancel_recovery(&self) {
        if let Some(timer) = self.0.state.lock().unwrap().recovery.take() {
            timer.abort();
        }
    }
    fn cancel_health(&self) {
        if let Some(timer) = self.0.state.lock().unwrap().health.take() {
            timer.abort();
        }
    }
    pub fn start(&self) -> Reply {
        // WorkerClient.start is eager before the original first await.
        let startup = self.0.owners.storage.start();
        let this = self.clone();
        retained(async move {
            let initializing = async {
                startup.await?;
                let diagnostics = this
                    .0
                    .owners
                    .storage
                    .request(
                        IvyJson::object([("action", "binding.diagnostics".into())]),
                        RequestOptions::default(),
                    )
                    .await?;
                this.0.owners.peers.restore_diagnostics(&diagnostics)?;
                let secrets = this
                    .0
                    .owners
                    .storage
                    .request(
                        IvyJson::object([("action", "oauth.initialize".into())]),
                        RequestOptions::default(),
                    )
                    .await?;
                this.0.owners.oauth.initialize(&secrets)?;
                if let Some(packages) = &this.0.owners.packages {
                    packages.initialize().await?;
                } else {
                    // The source awaits packages?.initialize() even when it is
                    // undefined. Keep that deferred continuation before listen.
                    tokio::task::yield_now().await;
                }
                this.0
                    .owners
                    .listener
                    .listen(
                        this.0.listen_port.unwrap_or(39081.0),
                        this.0
                            .listen_host
                            .clone()
                            .unwrap_or_else(|| "127.0.0.1".into()),
                    )
                    .await
            }
            .await;
            if let Err(error) = initializing {
                this.stop();
                this.cancel_recovery();
                // A failed close replaces the original startup error, as await
                // worker.close in the source catch block does.
                this.0.owners.storage.close().await?;
                return Err(error);
            }
            // The source catch ends at listen. Push, timer and address effects
            // below it are deliberately not reclassified as startup cleanup.
            this.0.owners.push.start()?;
            this.start_health();
            let address = this.0.owners.listener.address()?;
            if address.as_object().is_none() {
                return Err(IvyError::new(
                    "internal_error",
                    "Hive listener address is unavailable.",
                ));
            }
            Ok(IvyJson::object(
                [
                    ("port", address.get("port")),
                    ("host", address.get("address")),
                ]
                .into_iter()
                .filter_map(|(name, value)| value.map(|value| (name, value.clone()))),
            ))
        })
    }
    fn start_health(&self) {
        let this = self.clone();
        let period = Duration::from_secs(10);
        let mut deadline = Instant::now() + period;
        let task = tokio::spawn(async move {
            loop {
                tokio::time::sleep_until(deadline).await;
                deadline = Instant::now() + period;
                if let Err(error) = this.0.owners.peers.health() {
                    (this.0.owners.unhandled)(UnhandledKind::Callback, error);
                }
            }
        });
        if let Some(previous) = self
            .0
            .state
            .lock()
            .unwrap()
            .health
            .replace(task.abort_handle())
        {
            previous.abort();
        }
    }
    pub fn schedule_recovery(&self) {
        let mut state = self.0.state.lock().unwrap();
        if state.stopped || state.recovery.is_some() {
            return;
        }
        let this = self.clone();
        let deadline = Instant::now() + Duration::from_secs(1);
        let task = tokio::spawn(async move {
            tokio::time::sleep_until(deadline).await;
            // Clear before starting: close cancels scheduled timers, not an
            // already posted recovery or its diagnostic restoration.
            this.0.state.lock().unwrap().recovery = None;
            let startup = this.0.owners.storage.start();
            let result = async {
                startup.await?;
                let values = this
                    .0
                    .owners
                    .storage
                    .request(
                        IvyJson::object([("action", "binding.diagnostics".into())]),
                        RequestOptions::default(),
                    )
                    .await?;
                this.0.owners.peers.restore_diagnostics(&values)
            }
            .await;
            if result.is_err() {
                this.schedule_recovery();
            }
        });
        state.recovery = Some(task.abort_handle());
    }
    pub fn events_available(&self, through: f64) {
        if self.0.owners.peers.events_available(through) {
            let this = self.clone();
            (self.0.owners.immediate)(Box::new(move || {
                if let Err(error) = this.0.owners.peers.publish_events() {
                    (this.0.owners.unhandled)(UnhandledKind::Callback, error);
                }
            }));
        }
    }
    /// Bind directly to the native StorageClient EventSink on that executor.
    pub fn event_sink(&self) -> EventSink {
        let owner = Arc::downgrade(&self.0);
        Arc::new(move |event| {
            if let Some(owner) = owner.upgrade() {
                Lifecycle(owner).worker_event(event);
            }
        })
    }
    pub fn worker_event(&self, event: Event) {
        let result = match event {
            Event::Failure(_) => self
                .0
                .owners
                .peers
                .storage_failed()
                .map(|()| self.schedule_recovery()),
            Event::Notification(Notification::Routing(value)) => {
                self.0.owners.peers.routing_update(&value)
            }
            Event::Notification(Notification::Changes(scopes)) => {
                self.0.owners.peers.changes(&scopes)
            }
            Event::Notification(Notification::EventsAvailable(value)) => {
                self.events_available(
                    value
                        .get("throughSequence")
                        .and_then(IvyJson::as_f64)
                        .unwrap_or(f64::NAN),
                );
                Ok(())
            }
        };
        if let Err(error) = result {
            (self.0.owners.unhandled)(UnhandledKind::Callback, error);
        }
    }
    pub fn close(&self) -> Reply<()> {
        self.stop();
        let push = self.0.owners.push.close();
        let this = self.clone();
        retained(async move {
            push.await?;
            this.cancel_health();
            this.cancel_recovery();
            this.0.owners.peers.reject_for_shutdown()?;
            this.0.owners.listener.close_websockets()?;
            this.0.owners.listener.close_connections()?;
            let _ = this.0.owners.listener.close().await;
            this.0.owners.storage.close().await
        })
    }
    pub fn set_credentials(&self, credentials: Vec<Credential>) -> Reply<()> {
        let this = self.clone();
        let reply = self.0.owners.dispatch.run(move || async move {
            this.0
                .owners
                .storage
                .request(
                    IvyJson::object([
                        ("action", "credentials.configure".into()),
                        ("credentials", credentials_json(&credentials)),
                    ]),
                    RequestOptions::default(),
                )
                .await?;
            this.0.owners.peers.clear_authentication();
            this.0.state.lock().unwrap().credentials = credentials.clone();
            this.0.owners.oauth.set_credentials(credentials.clone())?;
            this.0.owners.peers.retire_credentials(&credentials)
        });
        Box::pin(async move { reply.await.map_err(|_| IvyError::internal())? })
    }
    pub fn collect_package_artifacts(&self) -> Reply<()> {
        let reply = self
            .0
            .owners
            .packages
            .as_ref()
            .map(|packages| packages.collect_artifacts());
        retained(async move {
            match reply {
                Some(reply) => reply.await,
                None => Ok(()),
            }
        })
    }
}
