//! Hive-owned constructor composition. Native launch and network effects remain
//! explicit inputs; constructing these owners opens no storage or listener.
use crate::{
    browser_push::{BrowserPush, Sender, Worker},
    http_auth::{self, Authentication},
    oauth::{self, HiveOAuth, WorkerFamilies},
    package_ui::UiPackageInstaller,
    packages::{PackageClock, PackageOptions, PackageRegistry, PackageStorage},
    retention::Runtime,
    server_http::Http,
    server_lifecycle::{self, Listener, StorageLifecycle},
    server_options::{self, BaseUrl},
    server_peers::{ErrorConstructor, Ids},
    server_rpc::{self, Clock},
    server_runtime::{self, Admission, Server},
    store::Credential,
    ui_files::UiFiles,
    worker_runtime::{EventSink, StorageClient, WorkerLaunch},
};
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString};
use ivy_host_runtime::native_path;
use std::{
    path::Path,
    sync::{Arc, OnceLock},
};

/// Original ServerOptions data after typed configuration admission. Retain its
/// ordered JSON for worker initialization instead of rebuilding a second shape.
/// Callback providers are separate and never enter the storage payload.
pub struct Options {
    source: IvyJson,
    filename: JsString,
    ui_root: Option<JsString>,
    public_base_url: JsString,
    credentials: Vec<Credential>,
    validate_messages: Option<bool>,
    reset_bootstrap_credential_digest: Option<JsString>,
    listen_host: Option<JsString>,
    listen_port: Option<f64>,
    call_timeout_ms: Option<f64>,
    trusted_proxy_addresses: Option<Vec<JsString>>,
    package_root: Option<JsString>,
    package_publisher_principal_ids: Option<Vec<JsString>>,
}
impl Options {
    /// Narrow already admitted settings/caller options. Malformed dynamic JS
    /// object/exception semantics belong to the external configuration boundary.
    pub fn from_json(source: IvyJson) -> IvyResult<Self> {
        let required_string = |value: &IvyJson, key: &str| {
            value
                .get(key)
                .and_then(IvyJson::as_js_string)
                .cloned()
                .ok_or_else(IvyError::internal)
        };
        let string = |key: &str| {
            source
                .get(key)
                .filter(|v| !v.is_null())
                .map(|v| v.as_js_string().cloned().ok_or_else(IvyError::internal))
                .transpose()
        };
        let number = |key: &str| {
            source
                .get(key)
                .filter(|v| !v.is_null())
                .map(|v| v.as_f64().ok_or_else(IvyError::internal))
                .transpose()
        };
        let strings = |key: &str| {
            source
                .get(key)
                .filter(|v| !v.is_null())
                .map(|v| {
                    v.as_array()
                        .ok_or_else(IvyError::internal)?
                        .iter()
                        .map(|v| v.as_js_string().cloned().ok_or_else(IvyError::internal))
                        .collect::<IvyResult<Vec<_>>>()
                })
                .transpose()
        };
        required_string(&source, "version")?;
        required_string(&source, "buildId")?;
        let options = Self {
            filename: required_string(&source, "filename")?,
            ui_root: string("uiRoot")?,
            public_base_url: required_string(&source, "publicBaseUrl")?,
            credentials: source
                .get("credentials")
                .and_then(IvyJson::as_array)
                .ok_or_else(IvyError::internal)?
                .iter()
                .map(|v| {
                    Ok(Credential {
                        principal_id: required_string(v, "principalId")?,
                        digest: required_string(v, "digest")?,
                    })
                })
                .collect::<IvyResult<Vec<_>>>()?,
            validate_messages: source
                .get("validateMessages")
                .filter(|v| !v.is_null())
                .map(|v| match v {
                    IvyJson::Bool(v) => Ok(*v),
                    _ => Err(IvyError::internal()),
                })
                .transpose()?,
            reset_bootstrap_credential_digest: string("resetBootstrapCredentialDigest")?,
            listen_host: string("listenHost")?,
            listen_port: number("listenPort")?,
            call_timeout_ms: number("callTimeoutMs")?,
            trusted_proxy_addresses: strings("trustedProxyAddresses")?,
            package_root: string("packageRoot")?,
            package_publisher_principal_ids: strings("packagePublisherPrincipalIds")?,
            source,
        };
        Ok(options)
    }
    fn worker_options(&self, ui_root: &JsString) -> IvyJson {
        let mut value = self.source.clone();
        value.insert("uiRoot", IvyJson::String(ui_root.clone()));
        value.remove("observeMcpCall");
        value.remove("sendBrowserPush");
        value
    }
}
pub struct Dependencies {
    pub sender: Arc<dyn Sender>,
    pub redirects: Arc<dyn oauth::Redirects>,
    pub entropy: Arc<dyn oauth::Entropy>,
    pub listener: Arc<dyn Listener>,
    pub now: Clock,
    pub dates: Arc<dyn Runtime + Send + Sync>,
    pub ids: Ids,
    pub provider_error: ErrorConstructor,
    pub immediate: server_lifecycle::Immediate,
    pub unhandled: server_lifecycle::Unhandled,
    pub warn: Arc<dyn Fn(&'static str) + Send + Sync>,
    pub observe_mcp_call: Option<crate::server_mcp::Observer>,
}
struct Dates(Clock, Arc<dyn Runtime + Send + Sync>);
impl PackageClock for Dates {
    fn now(&self) -> f64 {
        (self.0)()
    }
    fn iso_date(&self, milliseconds: f64) -> IvyResult<JsString> {
        self.1.iso_date(milliseconds)
    }
}
pub struct Prepared {
    base: BaseUrl,
    ui_root: JsString,
    ui_files: Arc<UiFiles>,
    worker_options: IvyJson,
    options: Options,
}
pub struct Constructed<S> {
    pub base: BaseUrl,
    pub ui_files: Arc<UiFiles>,
    pub worker: Arc<S>,
    pub browser_push: BrowserPush,
    pub oauth: HiveOAuth,
    pub authentication: Authentication,
    pub http: Http,
    pub mcp: crate::server_mcp::Mcp,
    pub packages: Option<PackageRegistry>,
    pub network: Server,
}
impl Prepared {
    pub fn base(&self) -> &BaseUrl {
        &self.base
    }
    pub fn ui_root(&self) -> &JsString {
        &self.ui_root
    }
    pub fn worker_options(&self) -> &IvyJson {
        &self.worker_options
    }
    pub fn new(options: Options) -> Result<Self, server_options::Error> {
        let base = server_options::validate(
            &options.public_base_url,
            options
                .trusted_proxy_addresses
                .as_deref()
                .unwrap_or_default(),
            options
                .package_publisher_principal_ids
                .as_deref()
                .unwrap_or_default(),
            &options.credentials,
        )?;
        let (ui_root, path) = match &options.ui_root {
            Some(root) => (root.clone(), native_path::to_native(root)),
            None => {
                let filename = native_path::to_native(&options.filename);
                let parent = filename.parent().unwrap_or(if filename.has_root() {
                    &filename
                } else {
                    Path::new(".")
                });
                let path = native_path::lexical(&parent.join("ui-releases"));
                (native_path::from_native(&path), path)
            }
        };
        let ui_files = Arc::new(UiFiles::new(path));
        let worker_options = options.worker_options(&ui_root);
        Ok(Self {
            base,
            ui_root,
            ui_files,
            worker_options,
            options,
        })
    }
    /// Preserve the caller's exact native executable, arguments, environment and
    /// process owner. Only the original constructor's worker data is supplied.
    pub fn launch_options(&self, mut launch: WorkerLaunch) -> WorkerLaunch {
        launch.options = self.worker_options.clone();
        launch
    }
    /// Bind all business owners to one initialized storage-client identity. A
    /// caller supplying storage must use these exact prepared worker options.
    pub fn bind<S>(
        self,
        worker: Arc<S>,
        dependencies: Dependencies,
        admission: Admission,
    ) -> Result<Constructed<S>, server_options::Error>
    where
        S: StorageLifecycle + Worker + PackageStorage + 'static,
    {
        let browser_push =
            BrowserPush::new(worker.clone(), self.base.href.clone(), dependencies.sender);
        let oauth = HiveOAuth::new(
            &self.base.href,
            self.options.credentials.clone(),
            oauth::Dependencies {
                families: Arc::new(WorkerFamilies(worker.clone())),
                redirects: dependencies.redirects,
                clock: dependencies.now.clone(),
                entropy: dependencies.entropy,
            },
        )?;
        let authentication = Authentication::new(
            http_auth::Options {
                canonical_origin: self.base.origin.clone(),
                base_path: self.base.base_path.clone(),
                trusted_proxy_addresses: self.options.trusted_proxy_addresses.unwrap_or_default(),
            },
            worker.clone(),
            Arc::new(oauth.clone()),
            dependencies.now.clone(),
        )?;
        let packages = self
            .options
            .package_root
            .as_ref()
            .filter(|v| !v.0.is_empty())
            .map(|root| {
                PackageRegistry::new(
                    native_path::to_native(root),
                    self.base.base_path.clone(),
                    Arc::new(Dates(dependencies.now.clone(), dependencies.dates.clone())),
                    Some(worker.clone()),
                    PackageOptions {
                        install_ui: Some(Arc::new(UiPackageInstaller::new(
                            worker.clone(),
                            self.ui_files.clone(),
                        ))),
                        ..Default::default()
                    },
                )
            });
        let request_budget = admission.requests.clone();
        let mcp_options = crate::server_mcp::Options {
            version: self
                .options
                .source
                .required("version")?
                .as_js_string()
                .unwrap()
                .clone(),
            public_base_url: self.options.public_base_url.clone(),
            base: self.base.clone(),
            ids: dependencies.ids.clone(),
            now: dependencies.now.clone(),
            observe: dependencies.observe_mcp_call,
        };
        let reset_bootstrap = self
            .options
            .reset_bootstrap_credential_digest
            .as_ref()
            .is_some_and(|v| !v.0.is_empty());
        let network = Server::new(
            server_runtime::Owners {
                storage: worker.clone(),
                oauth: Arc::new(oauth.clone()),
                push: Arc::new(browser_push.clone()),
                packages: packages.clone(),
                listener: dependencies.listener,
                now: dependencies.now,
                dates: dependencies.dates,
                ids: dependencies.ids,
                provider_error: dependencies.provider_error,
                clear_authentication: Arc::new({
                    let authentication = authentication.clone();
                    move || authentication.reset()
                }),
                immediate: dependencies.immediate,
                unhandled: dependencies.unhandled,
                warn: dependencies.warn,
            },
            server_runtime::Options {
                validate_messages: self.options.validate_messages == Some(true),
                call_timeout_ms: self.options.call_timeout_ms,
                package_publisher_principal_ids: self
                    .options
                    .package_publisher_principal_ids
                    .unwrap_or_default(),
                invocation: server_rpc::Options {
                    reset_bootstrap_credential_digest: self
                        .options
                        .reset_bootstrap_credential_digest,
                },
                lifecycle: server_lifecycle::Options {
                    credentials: self.options.credentials,
                    listen_host: self.options.listen_host,
                    listen_port: self.options.listen_port,
                },
            },
            admission,
        );
        let http = Http::new(
            worker.clone(),
            authentication.clone(),
            network.invocation.clone(),
            request_budget,
            reset_bootstrap,
        );
        let mcp = crate::server_mcp::Mcp::new(
            worker.clone(),
            network.invocation.clone(),
            oauth.clone(),
            mcp_options,
        );
        Ok(Constructed {
            base: self.base,
            ui_files: self.ui_files,
            worker,
            browser_push,
            oauth,
            authentication,
            http,
            mcp,
            packages,
            network,
        })
    }
    /// Construct the native storage client without starting it. Notifications
    /// bind before the client is returned; weak lifecycle ownership avoids a cycle.
    pub fn native(
        self,
        launch: WorkerLaunch,
        dependencies: Dependencies,
        admission: Admission,
    ) -> Result<Constructed<StorageClient>, server_options::Error> {
        let events = Arc::new(OnceLock::<EventSink>::new());
        let target = events.clone();
        let worker = Arc::new(StorageClient::new(
            self.launch_options(launch),
            Arc::new(move |event| {
                target.get().expect("Storage events bind before startup")(event);
            }),
        ));
        let server = self.bind(worker, dependencies, admission)?;
        let _ = events.set(server.network.lifecycle.event_sink());
        Ok(server)
    }
}
