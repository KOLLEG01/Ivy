//! Agent-owned native lifetime. A proxy connection never owns its shared daemon.
use crate::native_handshake::{AgentHandshake, NativeInitialization, NativeMode, initialize_agent};
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString, Outcome};
use ivy_host_runtime::{
    codex_proxy_transport::{ProxyControl, ProxyTransport},
    codex_rpc::{Events, NativeRpc},
    node_path,
    process::{OwnedProcess, ProcessResult},
};
use ivy_sdk::service::ServiceFuture;
use std::{future::Future, path::PathBuf, sync::Arc, time::Duration};
use tokio::{
    io::AsyncReadExt,
    sync::{oneshot, watch},
};

/// Startup supplies the original retained-evidence writer, including the full
/// process-tree observation. The connection never infers retirement from a PID.
pub type NativeStopEvidence =
    dyn Fn(JsString, u32, NativeMode, Arc<ProcessResult>) -> ServiceFuture<()> + Send + Sync;
pub struct NativeConnectionSettings {
    pub epoch: JsString,
    pub catalog: IvyJson,
    pub expected_home: PathBuf,
    pub native_version: JsString,
    pub client_version: JsString,
    pub mode: NativeMode,
    pub home_source: &'static str,
    pub owns_native_home: bool,
    pub executable: PathBuf,
    pub windows_shell: Option<IvyJson>,
    pub socket_path: Option<JsString>,
    pub daemon: Option<IvyJson>,
    pub record_stop: Arc<NativeStopEvidence>,
}
type Stopped = IvyResult<Arc<ProcessResult>>;
#[derive(Clone)]
pub struct NativeCompletion(watch::Receiver<Option<Stopped>>);
impl NativeCompletion {
    pub async fn wait(&self) -> Stopped {
        let mut receiver = self.0.clone();
        loop {
            if let Some(result) = receiver.borrow_and_update().clone() {
                return result;
            }
            receiver.changed().await.map_err(|_| IvyError::internal())?;
        }
    }
}
pub struct AgentNativeConnection {
    pub rpc: NativeRpc,
    pub catalog: IvyJson,
    pub epoch: JsString,
    pub initialized: IvyJson,
    pub native_home: PathBuf,
    pub actual_server_version: JsString,
    pub mode: NativeMode,
    pub home_source: &'static str,
    pub owns_native_home: bool,
    pub executable: PathBuf,
    pub windows_shell: Option<IvyJson>,
    pub socket_path: Option<JsString>,
    pub daemon: Option<IvyJson>,
    pub launcher_pid: u32,
    pub completion: NativeCompletion,
    stop: watch::Sender<bool>,
}
impl AgentNativeConnection {
    /// Retain one completion even if a close waiter is abandoned. Losing the
    /// connection also releases its owned child, without stopping a daemon.
    pub async fn close(&self) -> IvyResult<()> {
        self.stop.send_replace(true);
        let result = self.completion.wait().await?;
        if result.error_code.as_deref() == Some("outcome_unknown") {
            return Err(unknown());
        }
        Ok(())
    }
    pub fn status_owner(&self) -> IvyJson {
        IvyJson::object([
            ("epoch", IvyJson::String(self.epoch.clone())),
            ("initialized", self.initialized.clone()),
            (
                "nativeHome",
                IvyJson::String(node_path::from_native(&self.native_home)),
            ),
            (
                "actualServerVersion",
                IvyJson::String(self.actual_server_version.clone()),
            ),
            ("connectionMode", self.mode.connection_mode().into()),
            ("homeSource", self.home_source.into()),
            ("ownsNativeHome", self.owns_native_home.into()),
            (
                "executable",
                IvyJson::String(node_path::from_native(&self.executable)),
            ),
            (
                "windowsShell",
                self.windows_shell.clone().unwrap_or(IvyJson::Null),
            ),
            (
                "socketPath",
                self.socket_path
                    .clone()
                    .map(IvyJson::String)
                    .unwrap_or(IvyJson::Null),
            ),
            ("daemon", self.daemon.clone().unwrap_or(IvyJson::Null)),
            ("launcherPid", IvyJson::Number(self.launcher_pid as f64)),
        ])
    }
}
fn unknown() -> IvyError {
    IvyError::new(
        "outcome_unknown",
        "Native process stop remains unknown; its durable fence prevents another owner.",
    )
    .with_outcome(Outcome::Unknown)
}
struct Owner {
    child: OwnedProcess,
    settings: NativeConnectionSettings,
    rpc: Option<NativeRpc>,
    transport: Option<ProxyControl>,
}
impl Owner {
    async fn initialize(&mut self, events: Events) -> IvyResult<NativeInitialization> {
        let input = self.child.stdin.take().ok_or_else(IvyError::internal)?;
        let output = self.child.stdout.take().ok_or_else(IvyError::internal)?;
        self.rpc = Some(if self.settings.mode == NativeMode::ExternalProxy {
            let transport = ProxyTransport::start(input, output).await?;
            self.transport = Some(transport.control);
            NativeRpc::start(
                &self.settings.catalog,
                transport.input,
                transport.output,
                events,
            )?
        } else {
            NativeRpc::start(&self.settings.catalog, input, output, events)?
        });
        initialize_agent(
            self.rpc.as_ref().unwrap(),
            AgentHandshake {
                client_version: &self.settings.client_version,
                native_version: &self.settings.native_version,
                expected_home: &self.settings.expected_home,
                mode: self.settings.mode,
                daemon: self.settings.daemon.as_ref(),
            },
        )
        .await
    }
    async fn evidence(&self, result: Arc<ProcessResult>) -> Stopped {
        (self.settings.record_stop)(
            self.settings.epoch.clone(),
            self.child.pid,
            self.settings.mode,
            result.clone(),
        )
        .await?;
        Ok(result)
    }
    async fn retire(&self, observed: Option<Stopped>) -> Stopped {
        if let Some(rpc) = &self.rpc {
            rpc.close("native_owner_stopping").await;
        }
        if let Some(transport) = &self.transport {
            transport.close().await;
        }
        let result = self.child.stop(Duration::from_secs(2)).await?;
        match observed {
            Some(result) => result,
            None => self.evidence(result).await,
        }
    }
}

/// Call after the Agent launch checks and beginEpoch. Ownership transfers now;
/// cancellation while the handshake is pending still stops and records the tree.
pub fn connect_agent_native(
    child: OwnedProcess,
    settings: NativeConnectionSettings,
    events: Events,
) -> impl Future<Output = IvyResult<AgentNativeConnection>> + Send {
    let (mut ready, response) = oneshot::channel();
    tokio::spawn(async move {
        let mut owner = Owner {
            child,
            settings,
            rpc: None,
            transport: None,
        };
        let stderr = owner.child.stderr.take().map(|mut stream| {
            tokio::spawn(async move {
                let mut buffer = [0; 8192];
                while stream.read(&mut buffer).await.is_ok_and(|count| count != 0) {}
            })
        });
        let failed_setup = events.on_close.clone();
        let initialized = tokio::select! {
            value = owner.initialize(events) => value,
            _ = ready.closed() => Err(IvyError::new("native_owner_stopping", "Native connection startup was cancelled.")),
        };
        match initialized {
            Err(error) => {
                let setup_failed = owner.rpc.is_none();
                let cleanup = owner.retire(None).await;
                if setup_failed {
                    let _ = failed_setup("native_protocol_setup_failed".into()).await;
                }
                let error = match cleanup {
                    Err(error) => error,
                    Ok(result) if result.error_code.as_deref() == Some("outcome_unknown") => {
                        unknown()
                    }
                    Ok(_) => error,
                };
                let _ = ready.send(Err(error));
            }
            Ok(initialized) => {
                let (stop, mut stopping) = watch::channel(false);
                let (stopped, result) = watch::channel(None);
                let rpc = owner.rpc.as_ref().unwrap().clone();
                let settings = &owner.settings;
                let connection = AgentNativeConnection {
                    rpc: rpc.clone(),
                    catalog: settings.catalog.clone(),
                    epoch: settings.epoch.clone(),
                    initialized: initialized.initialized,
                    native_home: initialized.native_home,
                    actual_server_version: initialized.actual_server_version,
                    mode: settings.mode,
                    home_source: settings.home_source,
                    owns_native_home: settings.owns_native_home,
                    executable: settings.executable.clone(),
                    windows_shell: settings.windows_shell.clone(),
                    socket_path: settings.socket_path.clone(),
                    daemon: settings.daemon.clone(),
                    launcher_pid: owner.child.pid,
                    completion: NativeCompletion(result),
                    stop,
                };
                let mut observed = None;
                if ready.send(Ok(connection)).is_ok() {
                    tokio::select! {
                        _ = stopping.changed() => {},
                        _ = rpc.closed() => {},
                        result = owner.child.completion.wait() => {
                            let evidence = owner.evidence(result.clone()).await;
                            rpc.close(if evidence.is_err() { "native_stop_evidence_failed" } else { result.error_code.as_deref().unwrap_or("native_process_exited") }).await;
                            observed = Some(evidence);
                        },
                    }
                }
                stopped.send_replace(Some(owner.retire(observed).await));
            }
        }
        if let Some(stderr) = stderr {
            stderr.abort();
        }
    });
    async move { response.await.map_err(|_| IvyError::internal())? }
}
