//! Native process composition. Public requests stay in the existing Hive owners.
mod backup;
mod http;
mod maintenance;
mod websocket;

use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString, json::parse};
use ivy_hive::{
    native_runtime::{self, NativeRuntime},
    server_constructor::{Constructed, Dependencies, Options, Prepared},
    server_lifecycle::Listener as ListenerPort,
    server_rpc::Reply,
    worker_runtime::{StorageClient, WorkerLaunch},
};
use ivy_host_runtime::{
    config::{configuration_path, native_instance_config},
    health::HealthFile,
};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock, Weak},
    time::Duration,
};
use tokio::{
    net::TcpListener,
    sync::{oneshot, watch},
    task::JoinHandle,
};

type Server = Constructed<StorageClient>;
pub(super) fn failure(error: impl std::fmt::Display) -> IvyError {
    IvyError::new("internal_error", error.to_string())
}
pub(super) fn string(value: &IvyJson, key: &str) -> IvyResult<String> {
    value.required(key)?.as_str()
}
fn require(value: bool, message: &str) -> IvyResult<()> {
    if value {
        Ok(())
    } else {
        Err(IvyError::new("invalid_arguments", message))
    }
}
fn configuration_error(error: ivy_hive::server_options::Error) -> IvyError {
    match error {
        ivy_hive::server_options::Error::Ivy(error) => error,
        ivy_hive::server_options::Error::UrlConstructor { .. } => {
            IvyError::new("invalid_arguments", "Invalid canonical publicBaseUrl.")
        }
    }
}
pub(super) struct App {
    server: Server,
    assets: ivy_hive::http_static::Assets,
    body_buffers: ivy_hive::http_body::BodyBuffers,
    reset_bootstrap: bool,
}
impl App {
    async fn storage_ready(&self, timeout_ms: u64) -> bool {
        ivy_hive::server_rpc::Storage::request(
            self.server.worker.as_ref(),
            IvyJson::object([("action", "health".into())]),
            ivy_hive::worker_client::RequestOptions {
                timeout_ms,
                terminate_on_timeout: false,
                ..Default::default()
            },
        )
        .await
        .is_ok_and(|value| value.get("ready") == Some(&true.into()))
    }
}
#[derive(Clone)]
struct Listener(Arc<ListenerState>);
type ListenRequest = (f64, JsString, oneshot::Sender<IvyResult<()>>);
struct ListenerState {
    app: Arc<OnceLock<Weak<App>>>,
    address: Mutex<Option<std::net::SocketAddr>>,
    stop: watch::Sender<bool>,
    task: Mutex<Option<JoinHandle<()>>>,
    start: Mutex<Option<oneshot::Sender<ListenRequest>>>,
}
impl Listener {
    fn new(app: Arc<OnceLock<Weak<App>>>) -> Self {
        let (sender, receiver) = oneshot::channel::<ListenRequest>();
        let state = Arc::new(ListenerState {
            app,
            address: Mutex::new(None),
            stop: watch::channel(false).0,
            task: Mutex::new(None),
            start: Mutex::new(Some(sender)),
        });
        let weak = Arc::downgrade(&state);
        // Lifecycle requests may originate in a retained Send task. Perform the
        // actual listener start on the network's local executor instead.
        tokio::task::spawn_local(async move {
            if let Ok((port, host, reply)) = receiver.await {
                if let Some(state) = weak.upgrade() {
                    let _ = reply.send(Listener(state).open(port, host).await);
                }
            }
        });
        Self(state)
    }
    fn open(&self, port: f64, host: JsString) -> Reply<()> {
        let this = self.clone();
        Box::pin(async move {
            require(
                port.is_finite() && port.fract() == 0.0 && (0.0..=65535.0).contains(&port),
                "Invalid listen port.",
            )?;
            let listener =
                TcpListener::bind((String::from_utf16_lossy(&host.0).as_str(), port as u16))
                    .await
                    .map_err(failure)?;
            *this.0.address.lock().unwrap() = Some(listener.local_addr().map_err(failure)?);
            let state = this.0.clone();
            let task = tokio::task::spawn_local(async move {
                let mut stopping = state.stop.subscribe();
                let mut connections = tokio::task::JoinSet::new();
                loop {
                    tokio::select! {
                        _ = stopping.changed() => break,
                        accepted = listener.accept() => match accepted {
                            Ok((stream, remote)) => {
                                let Some(app) = state.app.get().and_then(Weak::upgrade) else { break; };
                                let shutdown = state.stop.subscribe();
                                connections.spawn_local(http::connection(app, stream, remote, shutdown));
                            }
                            Err(error) => { eprintln!("Hive accept failed: {error}"); break; }
                        },
                        _ = connections.join_next(), if !connections.is_empty() => {},
                    }
                }
                // Stop accepting first, then allow active requests and upgrades
                // to close before storage's ordinary shutdown begins.
                if tokio::time::timeout(Duration::from_secs(10), async {
                    while connections.join_next().await.is_some() {}
                })
                .await
                .is_err()
                {
                    connections.abort_all();
                    while connections.join_next().await.is_some() {}
                }
            });
            *this.0.task.lock().unwrap() = Some(task);
            Ok(())
        })
    }
}
impl ListenerPort for Listener {
    fn listen(&self, port: f64, host: JsString) -> Reply<()> {
        let sender = self.0.start.lock().unwrap().take();
        Box::pin(async move {
            let sender = sender.ok_or_else(|| {
                IvyError::new("service_unavailable", "Hive listener has already started.")
            })?;
            let (reply, result) = oneshot::channel();
            sender
                .send((port, host, reply))
                .map_err(|_| IvyError::internal())?;
            result.await.map_err(failure)?
        })
    }
    fn address(&self) -> IvyResult<IvyJson> {
        let address = self
            .0
            .address
            .lock()
            .unwrap()
            .ok_or_else(IvyError::internal)?;
        Ok(IvyJson::object([
            ("address", address.ip().to_string().into()),
            ("port", IvyJson::Number(address.port() as f64)),
        ]))
    }
    fn close_websockets(&self) -> IvyResult<()> {
        self.0.stop.send_replace(true);
        Ok(())
    }
    fn close_connections(&self) -> IvyResult<()> {
        self.0.stop.send_replace(true);
        Ok(())
    }
    fn close(&self) -> Reply<()> {
        self.0.stop.send_replace(true);
        let task = self.0.task.lock().unwrap().take();
        Box::pin(async move {
            if let Some(task) = task {
                task.await.map_err(failure)?;
            }
            Ok(())
        })
    }
}

fn read_optional(path: &Path) -> IvyResult<Option<IvyJson>> {
    match std::fs::read_to_string(path) {
        Ok(text) => parse(&text, 1024 * 1024).map(Some),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(failure(e)),
    }
}

async fn shutdown_signal() -> std::io::Result<()> {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        tokio::select! {
            result = tokio::signal::ctrl_c() => result,
            _ = terminate.recv() => Ok(()),
        }
    }
    #[cfg(not(unix))]
    tokio::signal::ctrl_c().await
}
pub async fn run() -> IvyResult<()> {
    let config_path = configuration_path(
        &std::env::args_os().skip(1).collect::<Vec<_>>(),
        std::env::var_os("IVY_INSTANCE_CONFIG"),
    )?;
    let loaded = native_instance_config(config_path.clone()).await?;
    let config = loaded.json().clone();
    let data = loaded.data_root().to_path_buf();
    let artifact = loaded.artifact_root().to_path_buf();
    let settings = maintenance::settings(&config)?;
    let credentials = maintenance::credentials(settings)?
        .into_iter()
        .map(|credential| {
            IvyJson::object([
                ("principalId", IvyJson::String(credential.principal_id)),
                ("digest", IvyJson::String(credential.digest)),
            ])
        })
        .collect();
    let mut options = IvyJson::object([
        (
            "filename",
            data.join("hive.sqlite")
                .to_string_lossy()
                .to_string()
                .into(),
        ),
        (
            "uiRoot",
            data.join("ui-releases")
                .to_string_lossy()
                .to_string()
                .into(),
        ),
        (
            "packageRoot",
            data.join("packages").to_string_lossy().to_string().into(),
        ),
        ("publicBaseUrl", config.required("publicBaseUrl")?.clone()),
        ("version", config.required("version")?.clone()),
        ("buildId", config.required("buildId")?.clone()),
        ("credentials", IvyJson::Array(credentials)),
    ]);
    for key in [
        "listenHost",
        "listenPort",
        "trustedProxyAddresses",
        "packagePublisherPrincipalIds",
    ] {
        if let Some(value) = settings.get(key) {
            options.insert(key, value.clone());
        }
    }
    options.insert(
        "validateMessages",
        (settings
            .get("debug")
            .and_then(|v| v.get("validateMessages"))
            == Some(&true.into()))
        .into(),
    );
    let reset = read_optional(&data.join("runtime-reset-bootstrap.json"))?
        .filter(|value| value.get("phase") != Some(&"released".into()));
    if let Some(reset) = &reset {
        require(
            reset.get("phase") == Some(&"bootstrap".into()),
            "Runtime reset bootstrap phase is invalid.",
        )?;
        let publisher = string(reset, "publisherCredentialDigest")?;
        require(
            publisher.len() == 71
                && publisher.starts_with("sha256:")
                && publisher[7..]
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
            "Runtime reset bootstrap credential is invalid.",
        )?;
        options.insert("resetBootstrapCredentialDigest", publisher.into());
    }
    let health = Arc::new(HealthFile::new(loaded));
    health
        .write(false, "Starting storage and HTTP listener.".into(), None)
        .await?;
    let holder = Arc::new(OnceLock::new());
    let listener = Listener::new(holder.clone());
    let client = ivy_hive::browser_push_https::NativeHttps::new()?;
    let sender = ivy_hive::browser_push_native::NativeSender::new(
        Arc::new(client),
        ivy_hive::browser_push_native::system_expiration(),
    );
    let executable = std::env::current_exe().map_err(failure)?;
    let job = std::env::var_os("IVY_JOB_LAUNCHER")
        .map(PathBuf::from)
        .or_else(|| {
            let file = executable.with_file_name(if cfg!(windows) {
                "ivy-job.exe"
            } else {
                "ivy-job"
            });
            file.is_file().then_some(file)
        });
    let server = Prepared::new(Options::from_json(options)?)
        .map_err(configuration_error)?
        .native(
            WorkerLaunch {
                root: data.clone(),
                executable,
                args: vec!["--storage-worker".into()],
                options: IvyJson::Null,
                job_launcher: job,
                environment: BTreeMap::new(),
            },
            Dependencies {
                sender: Arc::new(sender),
                redirects: Arc::new(ivy_hive::oauth::UrlRedirects),
                entropy: Arc::new(ivy_hive::oauth::OsEntropy),
                listener: Arc::new(listener),
                now: Arc::new(native_runtime::now),
                dates: Arc::new(NativeRuntime),
                ids: ivy_hive::server_peers::native_ids(),
                provider_error: Arc::new(|e| {
                    let mut error = IvyError::new(
                        String::from_utf16_lossy(&e.code.0),
                        String::from_utf16_lossy(&e.message.0),
                    )
                    .with_provider_outcome(e.outcome);
                    if let Some(details) = e.details {
                        error = error.with_details(details);
                    }
                    error
                }),
                immediate: Arc::new(|call| {
                    tokio::spawn(async move {
                        tokio::task::yield_now().await;
                        call();
                    });
                }),
                unhandled: Arc::new(|_, error| {
                    eprintln!("Hive asynchronous work failed: {error:?}")
                }),
                warn: Arc::new(|message| eprintln!("{message}")),
                observe_mcp_call: None,
            },
            Default::default(),
        )
        .map_err(configuration_error)?;
    let app = Arc::new(App {
        server,
        assets: ivy_hive::http_static::Assets {
            console_root: Some(artifact.join("dist/console")),
        },
        body_buffers: Default::default(),
        reset_bootstrap: reset.is_some(),
    });
    holder
        .set(Arc::downgrade(&app))
        .map_err(|_| IvyError::internal())?;
    let mut maintenance = maintenance::Maintenance::new(config_path, config).await?;
    if let Err(error) = app.server.network.lifecycle.start().await {
        let _ = health
            .write(false, "Hive startup failed.".into(), None)
            .await;
        return Err(error);
    }
    health.write(!app.reset_bootstrap, "".into(), None).await?;
    eprintln!("Hive listener and storage started.");
    let mut tick = tokio::time::interval(Duration::from_millis(500));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let interrupt = shutdown_signal();
    tokio::pin!(interrupt);
    let observed = {
        let background = maintenance.run(app.clone(), health.clone());
        tokio::pin!(background);
        loop {
            tokio::select! {
                result = &mut interrupt => break result.map_err(failure),
                _ = &mut background => break Err(IvyError::new("internal_error", "Hive maintenance stopped unexpectedly.")),
                _ = tick.tick() => match health.control().await {
                    Ok(Some(control)) if matches!(control.get("action").and_then(|v| v.as_str().ok()).as_deref(), Some("shutdown" | "drain")) => break Ok(()),
                    Ok(_) => {},
                    Err(error) => break Err(error),
                },
            }
        }
    };
    let incomplete = maintenance.stop();
    tokio::time::timeout(Duration::from_secs(15), async {
        let reported = health
            .write(false, "Hive is shutting down.".into(), None)
            .await;
        app.server.network.lifecycle.close().await?;
        if let Some(plan) = incomplete {
            plan.discard_incomplete().await?;
        }
        reported
    })
    .await
    .map_err(failure)??;
    observed
}
