//! Shared network state for admitted server work. Settings and I/O providers are
//! supplied by their owners; no listener or error-policy fallback is selected.
use crate::{
    admission::RequestBudget,
    browser_push::BrowserPush,
    live_routing::LiveRouting,
    packages::PackageRegistry,
    retention::Runtime,
    server_direct::{Dispatcher, Options as DirectOptions},
    server_lifecycle::{self, Lifecycle, Listener, OAuth, StorageLifecycle},
    server_options::{self, BaseUrl},
    server_peers::{self, ErrorConstructor, Ids, Peers},
    server_rpc::{Clock, Invocation, Options as InvocationOptions},
    tool_binding_diagnostics::ToolBindingDiagnostics,
    storage::RequestOptions,
};
use ivy_contracts::{IvyJson, JsString};
use std::sync::{Arc, Mutex};

pub trait Push: server_lifecycle::Push {
    fn wake(&self);
}
impl Push for BrowserPush {
    fn wake(&self) {
        BrowserPush::wake(self);
    }
}
/// HTTP invocation and socket ingress share these request reservations. All DB
/// actions, including credential changes, use the storage owner's bounded queue.
pub struct Admission {
    pub requests: RequestBudget,
}
impl Default for Admission {
    fn default() -> Self {
        Self {
            requests: RequestBudget::default(),
        }
    }
}
pub struct Owners {
    pub storage: Arc<dyn StorageLifecycle>,
    pub oauth: Arc<dyn OAuth>,
    pub push: Arc<dyn Push>,
    pub packages: Option<PackageRegistry>,
    pub listener: Arc<dyn Listener>,
    pub now: Clock,
    pub dates: Arc<dyn Runtime + Send + Sync>,
    pub ids: Ids,
    pub provider_error: ErrorConstructor,
    pub clear_authentication: Arc<dyn Fn() + Send + Sync>,
    pub immediate: server_lifecycle::Immediate,
    pub unhandled: server_lifecycle::Unhandled,
    pub warn: Arc<dyn Fn(&'static str) + Send + Sync>,
}
#[derive(Default)]
pub struct Options {
    pub validate_messages: bool,
    pub call_timeout_ms: Option<f64>,
    pub package_publisher_principal_ids: Vec<JsString>,
    pub invocation: InvocationOptions,
    pub lifecycle: server_lifecycle::Options,
}
pub struct Configuration {
    pub public_base_url: JsString,
    pub trusted_proxy_addresses: Vec<JsString>,
    pub runtime: Options,
}
#[derive(Clone)]
pub struct Server {
    pub invocation: Invocation,
    pub peers: Peers,
    pub lifecycle: Lifecycle,
}
impl Server {
    /// Validate the original constructor guards before assembling shared state.
    /// The returned canonical base feeds the HTTP/UI/OAuth owners; required I/O
    /// bindings remain explicit and are not initialized by this entry point.
    pub fn configured(
        owners: Owners,
        configuration: Configuration,
        admission: Admission,
    ) -> Result<(Self, BaseUrl), server_options::Error> {
        let base = server_options::validate(
            &configuration.public_base_url,
            &configuration.trusted_proxy_addresses,
            &configuration.runtime.package_publisher_principal_ids,
            &configuration.runtime.lifecycle.credentials,
        )?;
        Ok((Self::new(owners, configuration.runtime, admission), base))
    }
    pub fn new(owners: Owners, options: Options, admission: Admission) -> Self {
        let diagnostics = Arc::new(Mutex::new(ToolBindingDiagnostics::new(
            {
                let storage = owners.storage.clone();
                let warn = owners.warn.clone();
                Box::new(move |value| {
                    let work = storage.request(
                        IvyJson::object([("action", "diagnostic".into()), ("value", value)]),
                        RequestOptions::default(),
                    );
                    let warn = warn.clone();
                    tokio::spawn(async move {
                        if work.await.is_err() {
                            warn("Hive could not retain a Tool binding diagnostic.");
                        }
                    });
                    Ok(())
                })
            },
            owners.now.clone(),
            owners.dates,
        )));
        let routing = Arc::new(Mutex::new(LiveRouting::new(
            options.validate_messages,
            Some(Box::new({
                let diagnostics = diagnostics.clone();
                move |target| diagnostics.lock().unwrap().rejected(&target)
            })),
        )));
        let direct = Dispatcher::new(
            owners.now.clone(),
            owners.packages.clone(),
            DirectOptions {
                validate_messages: options.validate_messages,
                package_publisher_principal_ids: options.package_publisher_principal_ids,
            },
        );
        let peers = Peers::new(
            server_peers::Owners {
                storage: owners.storage.clone(),
                routing,
                diagnostics,
                requests: admission.requests.clone(),
                direct: Arc::new({
                    let direct = direct.clone();
                    move |peers, context, request, peer| {
                        direct.dispatch(peers, context, request, Some(peer))
                    }
                }),
                now: owners.now.clone(),
                ids: owners.ids,
                provider_error: owners.provider_error,
                wake_browser_push: Arc::new({
                    let push = owners.push.clone();
                    move || push.wake()
                }),
                clear_authentication: owners.clear_authentication,
                unhandled: owners.unhandled.clone(),
            },
            server_peers::Options {
                call_timeout_ms: options.call_timeout_ms,
                validate_messages: options.validate_messages,
            },
        );
        let invocation = Invocation::new(
            owners.storage.clone(),
            Arc::new({
                let peers = peers.clone();
                move |context, request| direct.dispatch(&peers, context, request, None)
            }),
            owners.now,
            admission.requests,
            options.invocation,
        );
        let lifecycle = Lifecycle::new(
            server_lifecycle::Owners {
                storage: owners.storage,
                peers: peers.clone(),
                oauth: owners.oauth,
                push: owners.push,
                packages: owners
                    .packages
                    .map(|packages| Arc::new(packages) as Arc<dyn server_lifecycle::Packages>),
                listener: owners.listener,
                immediate: owners.immediate,
                unhandled: owners.unhandled,
            },
            options.lifecycle,
        );
        Self {
            invocation,
            peers,
            lifecycle,
        }
    }
}
