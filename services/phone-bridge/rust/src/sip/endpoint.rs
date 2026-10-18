//! Owned transaction drivers and bounded delivery of TU-owned INVITE 2xx ACKs.
//! The call owner retains these handles until its protocol cleanup is settled.
use rsipstack::{
    EndpointBuilder,
    sip::{Method, Request, Response, SipMessage},
    transaction::{
        TransactionReceiver,
        endpoint::{Endpoint, EndpointInnerRef, EndpointOption, MessageInspector},
        key::{TransactionKey, TransactionRole},
        transaction::{Transaction, TransactionEvent, TransactionEventSender},
    },
    transport::{SipAddr, TransportLayer},
};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};
use tokio::{
    sync::{mpsc, watch},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

const RESPONSE_QUEUE: usize = 8;

/// No raw SIP, credentials, transport addresses or stack error text crosses
/// this observation boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SipIoError {
    InvalidRequest,
    DuplicateTransaction,
    EndpointStopped,
    InUse,
    SendFailed,
    Stopped,
    ResponseOverflow,
    DriverFailed,
}
impl std::fmt::Display for SipIoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for SipIoError {}

struct Route {
    invite: bool,
    finals: mpsc::Sender<Response>,
    failure: watch::Sender<Option<SipIoError>>,
    stop: Mutex<Option<TransactionEventSender>>,
}
impl Route {
    fn fail(&self, error: SipIoError) {
        self.failure.send_if_modified(|value| {
            if value.is_some() {
                false
            } else {
                *value = Some(error);
                true
            }
        });
    }
    fn stop(&self, key: &TransactionKey) {
        if let Some(sender) = self.stop.lock().unwrap().as_ref() {
            let _ = sender.send(TransactionEvent::Terminate(key.clone()));
        }
    }
}
#[derive(Default)]
struct Routes {
    closed: bool,
    clients: HashMap<TransactionKey, Arc<Route>>,
}
#[derive(Clone, Default)]
struct FinalResponses(Arc<Mutex<Routes>>);
impl FinalResponses {
    fn close(&self) {
        let mut routes = self.0.lock().unwrap();
        routes.closed = true;
        for (key, route) in &routes.clients {
            route.fail(SipIoError::EndpointStopped);
            route.stop(key);
        }
    }
    fn register(&self, key: TransactionKey, route: Arc<Route>) -> Result<Registration, SipIoError> {
        let mut routes = self.0.lock().unwrap();
        if routes.closed {
            return Err(SipIoError::EndpointStopped);
        }
        if routes.clients.contains_key(&key) {
            return Err(SipIoError::DuplicateTransaction);
        }
        routes.clients.insert(key.clone(), route.clone());
        Ok(Registration {
            key,
            route,
            owner: self.clone(),
        })
    }
}
impl MessageInspector for FinalResponses {
    fn before_send(&self, message: SipMessage, _: Option<&SipAddr>) -> SipMessage {
        message
    }
    fn after_received(&self, message: SipMessage, _: Option<&SipAddr>) -> SipMessage {
        // auto_ack_2xx=false retires the library transaction on its first 2xx.
        // The public inspector still observes later 2xx before stack routing.
        if let SipMessage::Response(response) = &message {
            if (200..=299).contains(&response.status_code.code()) {
                if let Ok(key) = TransactionKey::from_response(response, TransactionRole::Client) {
                    let routes = self.0.lock().unwrap();
                    if let Some(route) = routes.clients.get(&key).filter(|r| r.invite) {
                        if route.finals.try_send(response.clone()).is_err() {
                            route.fail(SipIoError::ResponseOverflow);
                        }
                    }
                }
            }
        }
        message
    }
}
struct Registration {
    key: TransactionKey,
    route: Arc<Route>,
    owner: FinalResponses,
}
impl Drop for Registration {
    fn drop(&mut self) {
        let mut routes = self.owner.0.lock().unwrap();
        if routes
            .clients
            .get(&self.key)
            .is_some_and(|r| Arc::ptr_eq(r, &self.route))
        {
            routes.clients.remove(&self.key);
        }
    }
}

pub struct SipEndpoint {
    endpoint: Endpoint,
    routes: FinalResponses,
    serving: Option<JoinHandle<Result<(), SipIoError>>>,
}
impl SipEndpoint {
    /// Transport binding/TLS policy and call admission belong to the host.
    /// The supplied token must be scoped to this endpoint and its transports.
    pub fn start(
        transport: TransportLayer,
        token: CancellationToken,
        mut options: EndpointOption,
    ) -> Self {
        options.auto_ack_2xx = false;
        let routes = FinalResponses::default();
        let endpoint = EndpointBuilder::new()
            .with_user_agent("IvyPhone")
            .with_transport_layer(transport)
            .with_cancel_token(token)
            .with_option(options)
            .with_inspector(Box::new(routes.clone()))
            .build();
        let inner = endpoint.inner.clone();
        let observed = routes.clone();
        let serving = tokio::spawn(async move {
            let result = inner.serve().await.map_err(|_| SipIoError::EndpointStopped);
            observed.close();
            result
        });
        Self {
            endpoint,
            routes,
            serving: Some(serving),
        }
    }
    pub fn inner(&self) -> &EndpointInnerRef {
        &self.endpoint.inner
    }
    pub fn incoming_transactions(&self) -> Result<TransactionReceiver, SipIoError> {
        self.endpoint
            .incoming_transactions()
            .map_err(|_| SipIoError::InUse)
    }
    pub fn client(
        &self,
        request: Request,
        proxy: Option<SipAddr>,
    ) -> Result<ClientExchange, SipIoError> {
        if request.method == Method::Ack {
            return Err(SipIoError::InvalidRequest);
        }
        let key = TransactionKey::from_request(&request, TransactionRole::Client)
            .map_err(|_| SipIoError::InvalidRequest)?;
        let registration = self.reserve(key.clone(), request.method == Method::Invite)?;
        let transaction = Transaction::new_client(key, request, self.endpoint.inner.clone(), None);
        Ok(ClientExchange::start(transaction, proxy, registration))
    }
    /// Adopt only an unsent authentication continuation produced by the stack.
    pub fn continuation(
        &self,
        transaction: Transaction,
        proxy: Option<SipAddr>,
    ) -> Result<ClientExchange, SipIoError> {
        use rsipstack::transaction::TransactionState;
        if !Arc::ptr_eq(&transaction.endpoint_inner, &self.endpoint.inner)
            || transaction.state != TransactionState::Nothing
        {
            return Err(SipIoError::InvalidRequest);
        }
        let registration = self.reserve(
            transaction.key.clone(),
            transaction.original.method == Method::Invite,
        )?;
        Ok(ClientExchange::start(transaction, proxy, registration))
    }
    fn reserve(&self, key: TransactionKey, invite: bool) -> Result<Channels, SipIoError> {
        let (finals, final_rx) = mpsc::channel(RESPONSE_QUEUE);
        let (failure, failed) = watch::channel(None);
        let route = Arc::new(Route {
            invite,
            finals,
            failure,
            stop: Mutex::new(None),
        });
        let registration = self.routes.register(key, route)?;
        Ok(Channels {
            registration,
            final_rx,
            failed,
        })
    }
    /// Refuse idle reconnect/shutdown while any original exchange is owned.
    /// Local transaction retirement never proves remote call cleanup.
    pub async fn shutdown(&mut self) -> Result<(), SipIoError> {
        {
            let mut routes = self.routes.0.lock().unwrap();
            if !routes.clients.is_empty() {
                return Err(SipIoError::InUse);
            }
            routes.closed = true;
        }
        self.endpoint.shutdown();
        if let Some(task) = self.serving.as_mut() {
            let result = task.await.map_err(|_| SipIoError::DriverFailed)?;
            self.serving.take();
            result?;
        }
        Ok(())
    }
}
impl Drop for SipEndpoint {
    fn drop(&mut self) {
        self.routes.close();
        self.endpoint.shutdown();
    }
}

struct Channels {
    registration: Registration,
    final_rx: mpsc::Receiver<Response>,
    failed: watch::Receiver<Option<SipIoError>>,
}
struct Finished {
    transaction: Transaction,
    failure: Option<SipIoError>,
}

/// One owning task drives each transaction without cancelling receive/send.
/// Dropping a response waiter cannot drop that task or lose its original state.
pub struct ClientExchange {
    registration: Registration,
    responses: mpsc::Receiver<Response>,
    finals: mpsc::Receiver<Response>,
    failed: watch::Receiver<Option<SipIoError>>,
    driver: Option<JoinHandle<Finished>>,
    finished: Option<Finished>,
    responses_closed: bool,
    proxy: Option<SipAddr>,
}
impl ClientExchange {
    fn start(mut transaction: Transaction, proxy: Option<SipAddr>, channels: Channels) -> Self {
        if let Some(proxy) = proxy.as_ref() {
            transaction.destination = Some(proxy.clone());
        }
        *channels.registration.route.stop.lock().unwrap() = Some(transaction.tu_sender.clone());
        let (responses, received) = mpsc::channel(RESPONSE_QUEUE);
        let route = channels.registration.route.clone();
        let driver = tokio::spawn(async move {
            let initial_failure = *route.failure.borrow();
            let failure = if let Some(error) = initial_failure {
                Some(error)
            } else if transaction.send().await.is_err() {
                Some(SipIoError::SendFailed)
            } else {
                loop {
                    // Deliberately no select!/timeout around this future.
                    let Some(SipMessage::Response(response)) = transaction.receive().await else {
                        break Some(SipIoError::Stopped);
                    };
                    let final_response = response.status_code.code() >= 200;
                    let routed = route.invite && (200..=299).contains(&response.status_code.code());
                    if !routed && responses.try_send(response).is_err() {
                        break Some(SipIoError::ResponseOverflow);
                    }
                    if final_response {
                        break None;
                    }
                }
            };
            if let Some(error) = failure {
                route.fail(error);
            }
            Finished {
                transaction,
                failure,
            }
        });
        Self {
            registration: channels.registration,
            responses: received,
            finals: channels.final_rx,
            failed: channels.failed,
            driver: Some(driver),
            finished: None,
            responses_closed: false,
            proxy,
        }
    }
    /// Cancellation-safe observation. All successful INVITE responses, even
    /// after the driver completes, still need selected-dialog/ACK validation.
    pub async fn next_response(&mut self) -> Result<Response, SipIoError> {
        loop {
            if let Some(error) = *self.failed.borrow() {
                return Err(error);
            }
            if self.responses_closed && !self.registration.route.invite {
                return Err(SipIoError::Stopped);
            }
            tokio::select! {
                biased;
                changed = self.failed.changed() => {
                    if changed.is_err() { return Err(SipIoError::DriverFailed); }
                }
                response = self.finals.recv(), if self.registration.route.invite => {
                    return response.ok_or(SipIoError::Stopped);
                }
                response = self.responses.recv(), if !self.responses_closed => {
                    if let Some(response) = response { return Ok(response); }
                    self.responses_closed = true;
                }
                result = async { self.driver.as_mut().unwrap().await }, if self.driver.is_some() => {
                    self.retain_completion(result)?;
                }
            }
        }
    }
    fn retain_completion(
        &mut self,
        result: Result<Finished, tokio::task::JoinError>,
    ) -> Result<(), SipIoError> {
        self.driver.take();
        match result {
            Ok(finished) => self.finished = Some(finished),
            Err(_) => {
                self.registration.route.fail(SipIoError::DriverFailed);
                return Err(SipIoError::DriverFailed);
            }
        }
        Ok(())
    }
    async fn join_driver(&mut self) -> Result<(), SipIoError> {
        if let Some(task) = self.driver.as_mut() {
            let result = task.await;
            self.retain_completion(result)?;
        }
        Ok(())
    }
    /// Retains the completed original for authentication and reliable flow use.
    /// Awaiting this handle is cancellation-safe; its driver remains owned.
    pub async fn completed(&mut self) -> Result<&Transaction, SipIoError> {
        self.join_driver().await?;
        let finished = self.finished.as_ref().ok_or(SipIoError::DriverFailed)?;
        if let Some(error) = finished.failure {
            return Err(error);
        }
        Ok(&finished.transaction)
    }
    /// The call owner must await this send fully before media startup or BYE.
    /// InviteExchange validates/saves the exact selected-dialog ACK first.
    pub async fn send_ack(&mut self, ack: &Request) -> Result<(), SipIoError> {
        if ack.method != Method::Ack {
            return Err(SipIoError::InvalidRequest);
        }
        self.join_driver().await?;
        let transaction = &self
            .finished
            .as_ref()
            .ok_or(SipIoError::DriverFailed)?
            .transaction;
        let inner = &transaction.endpoint_inner;
        let (connection, destination) =
            if let Some(connection) = transaction.connection.as_ref().filter(|c| c.is_reliable()) {
                (connection.clone(), transaction.destination.clone())
            } else {
                let target = match self.proxy.as_ref() {
                    Some(proxy) => proxy.clone(),
                    None => SipAddr::try_from(&ack.destination())
                        .map_err(|_| SipIoError::InvalidRequest)?,
                };
                let (connection, destination) = inner
                    .transport_layer
                    .lookup(&target, Some(&transaction.key))
                    .await
                    .map_err(|_| SipIoError::SendFailed)?;
                (connection, Some(destination))
            };
        connection
            .send(SipMessage::Request(ack.clone()), destination.as_ref())
            .await
            .map_err(|_| SipIoError::SendFailed)
    }
    /// Explicit local retirement, after the call owner has settled or fenced
    /// cleanup. This does not send CANCEL/BYE or report the call as ended.
    pub async fn retire(mut self) -> Result<(), SipIoError> {
        self.registration.route.stop(&self.registration.key);
        self.join_driver().await
    }
}
impl Drop for ClientExchange {
    fn drop(&mut self) {
        // Ask the driver to finish through its own event channel. Never abort
        // a task inside the stack's non-cancellation-safe receive/send.
        self.registration.route.stop(&self.registration.key);
    }
}
