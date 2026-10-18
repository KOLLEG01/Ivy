//! Bound TLS setup has no SIP capability. A timed-out verifier may finish on
//! its blocking worker, but its socket is shut down and its result is refused.
use super::binding::{BindError, CONNECT_TIMEOUT, TCP_CONNECTIONS, tcp_socket};
use rsipstack::transport::{SipAddr, SipConnection, tls::TlsConnection};
use rustls_platform_verifier::BuilderVerifierExt;
use std::{
    net::{Shutdown, SocketAddr, TcpStream as Socket},
    sync::{
        Arc, Mutex as SyncMutex,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::{net::TcpStream, sync::Mutex, task::JoinHandle, time::Instant};
use tokio_rustls::{
    TlsConnector,
    client::TlsStream,
    rustls::{ClientConfig, client::Resumption, crypto::ring, pki_types::ServerName},
};
use tokio_util::sync::CancellationToken;

#[derive(Default)]
struct SocketState {
    stopped: bool,
    socket: Option<Socket>,
}
#[derive(Default)]
struct SocketControl(SyncMutex<SocketState>);
impl SocketControl {
    fn install(&self, socket: Socket) -> Result<(), BindError> {
        let mut state = self.0.lock().unwrap();
        if state.stopped {
            let _ = socket.shutdown(Shutdown::Both);
            return Err(BindError::Stopped);
        }
        state.socket = Some(socket);
        Ok(())
    }
    fn stop(&self) -> Result<(), BindError> {
        let mut state = self.0.lock().unwrap();
        state.stopped = true;
        if let Some(socket) = &state.socket {
            match socket.shutdown(Shutdown::Both) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotConnected => {}
                Err(_) => return Err(BindError::ConnectFailed),
            }
        }
        state.socket.take();
        Ok(())
    }
}
struct Attempt {
    stop: CancellationToken,
    socket: Arc<SocketControl>,
    task: Mutex<Option<JoinHandle<Result<TlsStream<TcpStream>, BindError>>>>,
    consumed: AtomicBool,
}
struct AttemptGuard(Arc<Attempt>, bool);
impl Drop for AttemptGuard {
    fn drop(&mut self) {
        if self.1 {
            self.0.stop.cancel();
            let _ = self.0.socket.stop();
        }
    }
}
struct Flow {
    retired: CancellationToken,
    socket: Arc<SocketControl>,
}
pub(super) struct TlsClient {
    config: Arc<ClientConfig>,
    attempts: Mutex<Vec<Arc<Attempt>>>,
    flows: SyncMutex<Vec<Flow>>,
}
impl TlsClient {
    pub(super) fn platform() -> Result<Self, BindError> {
        let config = ClientConfig::builder_with_provider(Arc::new(ring::default_provider()))
            .with_safe_default_protocol_versions()
            .map_err(|_| BindError::TlsConfiguration)?
            .with_platform_verifier()
            .map_err(|_| BindError::TlsConfiguration)?
            .with_no_client_auth();
        Ok(Self::new(config))
    }
    fn new(mut config: ClientConfig) -> Self {
        // Every new flow authenticates its expected authority before SIP.
        config.enable_early_data = false;
        config.resumption = Resumption::disabled();
        Self {
            config: Arc::new(config),
            attempts: Mutex::new(Vec::new()),
            flows: SyncMutex::new(Vec::new()),
        }
    }
    #[cfg(test)]
    pub(super) fn with_config(config: ClientConfig) -> Self {
        Self::new(config)
    }
    async fn retire_finished(&self) {
        let mut attempts = self.attempts.lock().await;
        for attempt in attempts.iter() {
            let mut task = attempt.task.lock().await;
            if task.as_ref().is_some_and(|task| task.is_finished()) {
                // A cancelled observer left this result here; never attach it.
                let _ = attempt.socket.stop();
                drop(task.take().unwrap().await);
                attempt.consumed.store(true, Ordering::Release);
            }
        }
        attempts.retain(|attempt| !attempt.consumed.load(Ordering::Acquire));
        let mut flows = self.flows.lock().unwrap();
        flows.retain(|flow| {
            if flow.retired.is_cancelled() {
                // Stale stack handles cannot send after this OS shutdown.
                flow.socket.stop().is_err()
            } else {
                true
            }
        });
    }
    /// The binding's connection-cache lock serializes callers. Setup is owned
    /// independently of its observer because platform verification can block.
    pub(super) async fn connect(
        &self,
        local: SocketAddr,
        destination: SipAddr,
        name: ServerName<'static>,
        host_stop: &CancellationToken,
    ) -> Result<SipConnection, BindError> {
        self.retire_finished().await;
        if self.attempts.lock().await.len() + self.flows.lock().unwrap().len() >= TCP_CONNECTIONS {
            return Err(BindError::Capacity);
        }
        let remote = destination
            .get_socketaddr()
            .map_err(|_| BindError::InvalidDestination)?;
        let deadline = Instant::now() + CONNECT_TIMEOUT;
        let stop = host_stop.child_token();
        let socket = Arc::new(SocketControl::default());
        let worker_stop = stop.clone();
        let worker_socket = socket.clone();
        let config = self.config.clone();
        let runtime = tokio::runtime::Handle::current();
        // Take the owner slot before spawning: cancellation at an await must
        // never leave a detached setup worker outside the shutdown fence.
        let mut attempts = self.attempts.lock().await;
        // Windows's platform chain verifier can perform synchronous retrieval.
        // Poll it away from the runtime that enforces our five-second bound.
        let task = tokio::task::spawn_blocking(move || {
            runtime.block_on(async move {
            let setup = async {
                let stream = tcp_socket(local)?.connect(remote).await.map_err(|_| BindError::ConnectFailed)?;
                let stream = stream.into_std().map_err(|_| BindError::ConnectFailed)?;
                worker_socket.install(stream.try_clone().map_err(|_| BindError::ConnectFailed)?)?;
                let stream = TcpStream::from_std(stream).map_err(|_| BindError::ConnectFailed)?;
                let stream = TlsConnector::from(config).connect(name, stream).await.map_err(|_| BindError::TlsRejected)?;
                if worker_stop.is_cancelled() { return Err(BindError::Stopped); }
                if Instant::now() >= deadline { return Err(BindError::ConnectTimedOut); }
                Ok(stream)
            };
            tokio::select! {
                biased;
                _ = worker_stop.cancelled() => Err(BindError::Stopped),
                result = tokio::time::timeout_at(deadline, setup) => result.map_err(|_| BindError::ConnectTimedOut)?,
            }
        })
        });
        let attempt = Arc::new(Attempt {
            stop,
            socket,
            task: Mutex::new(Some(task)),
            consumed: AtomicBool::new(false),
        });
        attempts.push(attempt.clone());
        drop(attempts);
        let mut guard = AttemptGuard(attempt.clone(), true);
        let mut task = attempt.task.lock().await;
        let result = tokio::select! {
            biased;
            _ = host_stop.cancelled() => return Err(BindError::Stopped),
            result = tokio::time::timeout_at(deadline, task.as_mut().unwrap()) => result.map_err(|_| BindError::ConnectTimedOut)?,
        };
        task.take();
        attempt.consumed.store(true, Ordering::Release);
        let stream = result.map_err(|_| BindError::ConnectFailed)??;
        if host_stop.is_cancelled() {
            return Err(BindError::Stopped);
        }
        if Instant::now() >= deadline {
            return Err(BindError::ConnectTimedOut);
        }
        let retired = host_stop.child_token();
        let connection =
            TlsConnection::from_client_stream(stream, destination, Some(retired.clone()))
                .await
                .map_err(|_| BindError::ConnectFailed)?;
        self.flows.lock().unwrap().push(Flow {
            retired,
            socket: attempt.socket.clone(),
        });
        guard.1 = false;
        Ok(SipConnection::Tls(connection))
    }
    pub(super) async fn stop(&self) -> Result<(), BindError> {
        let attempts = self.attempts.lock().await;
        for attempt in attempts.iter() {
            attempt.stop.cancel();
            attempt.socket.stop()?;
        }
        for flow in self.flows.lock().unwrap().iter() {
            flow.socket.stop()?;
        }
        // Do not detach a still-blocked certificate worker or clear its fence.
        tokio::time::timeout(CONNECT_TIMEOUT, async {
            for attempt in attempts.iter() {
                let mut task = attempt.task.lock().await;
                if let Some(task) = task.as_mut() {
                    drop(task.await);
                }
                task.take();
                attempt.consumed.store(true, Ordering::Release);
            }
        })
        .await
        .map_err(|_| BindError::Endpoint(super::SipIoError::InUse))?;
        drop(attempts);
        self.attempts.lock().await.clear();
        self.flows.lock().unwrap().clear();
        Ok(())
    }
}
impl Drop for TlsClient {
    fn drop(&mut self) {
        for flow in self.flows.get_mut().unwrap().iter() {
            let _ = flow.socket.stop();
        }
        for attempt in self.attempts.get_mut().iter() {
            attempt.stop.cancel();
            let _ = attempt.socket.stop();
        }
    }
}
