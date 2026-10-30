//! Native RPC lifetime after the launch owner has verified executable, home and catalog.
//! An external proxy owns only its tunnel process, never the shared daemon.
use super::{
    codex_handshake::{self, Handshake},
    codex_proxy_transport::{ProxyControl, ProxyTransport},
    codex_rpc::{Events, NativeRpc},
    process::{OwnedProcess, ProcessResult},
    process_fence::record_process_stop_fence,
};
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString};
use std::{future::Future, path::PathBuf, sync::Arc, time::Duration};
use tokio::{
    io::AsyncReadExt,
    sync::{oneshot, watch},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    OwnedStdio,
    ExternalProxy,
}

pub struct Identity {
    pub data_root: PathBuf,
    /// Canonical server home, already selected by launch preflight.
    pub home: PathBuf,
    pub epoch: String,
    pub native_version: JsString,
    pub client_name: JsString,
    pub client_version: JsString,
    pub mode: Mode,
}

type Stopped = IvyResult<Arc<ProcessResult>>;
#[derive(Clone)]
pub struct Completion(watch::Receiver<Option<Stopped>>);
impl Completion {
    pub async fn wait(&self) -> Stopped {
        let mut result = self.0.clone();
        loop {
            if let Some(value) = result.borrow_and_update().clone() {
                return value;
            }
            if result.changed().await.is_err() {
                return Err(IvyError::internal());
            }
        }
    }
}

pub struct NativeConnection {
    pub rpc: NativeRpc,
    pub home: PathBuf,
    pub epoch: String,
    pub mode: Mode,
    pub completion: Completion,
    stop: watch::Sender<bool>,
}
impl NativeConnection {
    /// All callers join one retirement result. Dropping this handle also stops
    /// the owned process even when another consumer retains an RPC clone.
    pub async fn close(&self) -> IvyResult<()> {
        self.stop.send_replace(true);
        let result = self.completion.wait().await?;
        if result.error_code.as_deref() == Some("outcome_unknown") {
            return Err(IvyError::new(
                "outcome_unknown",
                "Codex process cleanup remains unknown.",
            ));
        }
        Ok(())
    }
}

struct Owner {
    child: OwnedProcess,
    identity: Identity,
    rpc: Option<NativeRpc>,
    transport: Option<ProxyControl>,
}
impl Owner {
    async fn initialize(&mut self, catalog: Arc<IvyJson>, events: Events) -> IvyResult<()> {
        let input = self.child.stdin.take().ok_or_else(IvyError::internal)?;
        let output = self.child.stdout.take().ok_or_else(IvyError::internal)?;
        self.rpc = Some(if self.identity.mode == Mode::ExternalProxy {
            let transport = ProxyTransport::start(input, output).await?;
            self.transport = Some(transport.control);
            NativeRpc::start(catalog, transport.input, transport.output, events)?
        } else {
            NativeRpc::start(catalog, input, output, events)?
        });
        codex_handshake::initialize(
            self.rpc.as_ref().unwrap(),
            Handshake {
                client_name: &self.identity.client_name,
                client_version: &self.identity.client_version,
                native_version: &self.identity.native_version,
                expected_home: &self.identity.home,
            },
        )
        .await
    }

    async fn evidence(&self, result: Arc<ProcessResult>) -> Stopped {
        retain_stop_evidence(
            self.identity.data_root.clone(),
            &self.identity.epoch,
            self.child.pid,
            result,
        )
        .await
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
            Some(value) => value,
            None => self.evidence(result).await,
        }
    }
}

pub(crate) async fn retain_stop_evidence(
    data_root: PathBuf,
    epoch: &str,
    pid: u32,
    result: Arc<ProcessResult>,
) -> Stopped {
    if result.error_code.as_deref() == Some("outcome_unknown") {
        record_process_stop_fence(
            data_root,
            IvyJson::object([
                ("schemaVersion", IvyJson::Number(1.0)),
                ("owner", "native".into()),
                ("ownerId", epoch.into()),
                ("processId", IvyJson::Number(pid as f64)),
                (
                    "observedAt",
                    chrono::Utc::now()
                        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
                        .into(),
                ),
                ("code", "outcome_unknown".into()),
            ]),
        )
        .await?;
    }
    Ok(result)
}

/// The caller must perform all original launch checks (including a clear stop
/// fence) BEFORE spawning `child`. This boundary performs handshake, callbacks,
/// and retirement; it is not a replacement for startCodexProcess preflight.
/// Ownership transfers immediately when called, so cancelling the returned
/// future still retires the child and persists uncertain stop evidence.
pub fn connect(
    child: OwnedProcess,
    identity: Identity,
    catalog: IvyJson,
    events: Events,
) -> impl Future<Output = IvyResult<NativeConnection>> + Send {
    let (mut ready, response) = oneshot::channel();
    tokio::spawn(async move {
        let mut owner = Owner {
            child,
            identity,
            rpc: None,
            transport: None,
        };
        // Raw protocol mode hands stderr to this owner too. Drain without
        // retaining unbounded output or publishing potential native secrets.
        let stderr = owner.child.stderr.take().map(|mut stream| {
            tokio::spawn(async move {
                let mut buffer = [0; 8192];
                while stream.read(&mut buffer).await.is_ok_and(|count| count != 0) {}
            })
        });
        let initialized = tokio::select! {
            value = owner.initialize(Arc::new(catalog), events) => value,
            _ = ready.closed() => Err(IvyError::new("native_owner_stopping", "Native connection startup was cancelled.")),
        };
        if let Err(error) = initialized {
            let cleanup = owner.retire(None).await;
            let error = match cleanup {
                Err(error) => error,
                Ok(result) if result.error_code.as_deref() == Some("outcome_unknown") => {
                    IvyError::new("outcome_unknown", "Codex process cleanup remains unknown.")
                }
                Ok(_) => error,
            };
            let _ = ready.send(Err(error));
        } else {
            let (stop, mut stopping) = watch::channel(false);
            let (stopped, result) = watch::channel(None);
            let rpc = owner.rpc.as_ref().unwrap().clone();
            let connection = NativeConnection {
                rpc: rpc.clone(),
                home: owner.identity.home.clone(),
                epoch: owner.identity.epoch.clone(),
                mode: owner.identity.mode,
                completion: Completion(result),
                stop,
            };
            let mut observed = None;
            if ready.send(Ok(connection)).is_ok() {
                tokio::select! {
                    _ = stopping.changed() => {},
                    _ = rpc.closed() => {},
                    result = owner.child.completion.wait() => {
                        let evidence = owner.evidence(result.clone()).await;
                        rpc.close(if evidence.is_err() { "native_stop_evidence_failed" }
                            else { result.error_code.as_deref().unwrap_or("native_process_exited") }).await;
                        observed = Some(evidence);
                    },
                }
            }
            stopped.send_replace(Some(owner.retire(observed).await));
        }
        if let Some(stderr) = stderr {
            stderr.abort();
        }
    });
    async move { response.await.map_err(|_| IvyError::internal())? }
}
