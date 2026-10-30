//! Network lifecycle with credential effects applied at the DB action boundary.
//! HTTP/WS listening and runtime error policy remain explicit owners; no listener
//! or production-runtime fallback is constructed here.
use crate::{
    browser_push::BrowserPush,
    oauth::{HiveOAuth, Secrets},
    packages::PackageRegistry,
    server_peers::{Peers, UnhandledKind},
    server_rpc::{Reply, Storage},
    store::Credential,
    storage::{Payload, Request, RequestEffects, RequestOptions},
    storage::Notification,
    storage::{Event, EventSink, StorageClient},
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
    health: Option<AbortHandle>,
}
struct Parts {
    owners: Owners,
    listen_host: Option<JsString>,
    listen_port: Option<f64>,
    state: Mutex<State>,
}
#[derive(Clone)]
pub struct Lifecycle(Arc<Parts>);

struct CredentialsEffects {
    lifecycle: Lifecycle,
    credentials: Arc<Vec<Credential>>,
}
impl RequestEffects for CredentialsEffects {
    fn retained_bytes(&self) -> usize {
        // Payload::ConfigureCredentials accounts for the shared vector once.
        std::mem::size_of::<Self>()
    }
    fn complete(self: Box<Self>, result: IvyResult<IvyJson>) -> IvyResult<IvyJson> {
        let value = result?;
        let owners = &self.lifecycle.0.owners;
        owners.peers.clear_authentication();
        self.lifecycle.0.state.lock().unwrap().credentials = self.credentials.as_ref().clone();
        owners.oauth.set_credentials(self.credentials.as_ref().clone())?;
        owners.peers.retire_credentials(&self.credentials)?;
        Ok(value)
    }
}
fn retained<T: Send + 'static>(
    work: impl std::future::Future<Output = IvyResult<T>> + Send + 'static,
) -> Reply<T> {
    let (sender, receiver) = oneshot::channel();
    tokio::spawn(async move {
        let _ = sender.send(work.await);
    });
    Box::pin(async move { receiver.await.map_err(|_| IvyError::internal())? })
}
impl Lifecycle {
    /// Entry points run on the network executor; committed credential effects
    /// run on the DB owner before it starts the next action.
    /// Abandoning a returned future never cancels accepted lifecycle work.
    pub fn new(owners: Owners, options: Options) -> Self {
        Self(Arc::new(Parts {
            owners,
            listen_host: options.listen_host,
            listen_port: options.listen_port,
            state: Mutex::new(State {
                credentials: options.credentials,
                health: None,
            }),
        }))
    }
    pub fn credentials(&self) -> Vec<Credential> {
        self.0.state.lock().unwrap().credentials.clone()
    }
    fn stop(&self) {
        self.0.owners.peers.stop_publication();
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
            Event::Failure(_) => self.0.owners.peers.storage_failed(),
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
            this.0.owners.peers.reject_for_shutdown()?;
            this.0.owners.listener.close_websockets()?;
            this.0.owners.listener.close_connections()?;
            let _ = this.0.owners.listener.close().await;
            this.0.owners.storage.close().await
        })
    }
    pub fn set_credentials(&self, credentials: Vec<Credential>) -> Reply<()> {
        let credentials = Arc::new(credentials);
        let reply = self.0.owners.storage.dispatch(
            Request::with_effects(
                Payload::ConfigureCredentials(credentials.clone()),
                CredentialsEffects { lifecycle: self.clone(), credentials },
            ),
            RequestOptions::default(),
        );
        retained(async move { reply.await.map(|_| ()) })
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
