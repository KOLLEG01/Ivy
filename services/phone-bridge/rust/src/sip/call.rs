//! One original delayed INVITE, independent of native IPC command waiters.
//! The host persists admission/intent and owns the PCM/media implementation.
use super::{
    CallAction, CallError, CallOutcome, ClientExchange, DelayedOfferCall, Dialog, DialogRequest,
    FinalPhase, InviteExchange, SipEndpoint, SipError, SipIoError,
    endpoint::DialogFlow,
    invite::{bounded_secret, protected},
};
use rsipstack::{
    dialog::authenticate::{Credential, handle_client_authenticate},
    sip::{
        Header, Method, Request, Response, StatusCode,
        headers::{CSeq, ContentLength, UserAgent},
        prelude::HeadersExt,
    },
    transaction::Transaction,
    transport::{SipAddr, SipConnection},
};
use std::{
    future::{Future, pending},
    pin::Pin,
    sync::Arc,
    time::Duration,
};
use tokio::{
    sync::{mpsc, oneshot, watch},
    task::{JoinHandle, JoinSet},
    time::{Instant, sleep_until},
};
use tokio_util::sync::CancellationToken;

/// The media owner implements real SDP negotiation and RTP startup. Rejected
/// answers use that same SDP parser; signalling must not invent a second one.
pub trait DelayedInviteMedia: Send + Sync + 'static {
    fn is_closed(&self) -> bool;
    fn failed(&self) -> bool;
    fn negotiate_offer(&self, offer: &str) -> Option<String>;
    fn rejected_answer(&self, offer: &str) -> Option<String>;
    fn start(&self) -> Pin<Box<dyn Future<Output = bool> + Send + '_>>;
}

// No Debug: these protected runtime inputs are never printable observations.
pub struct DigestCredentials {
    username: String,
    password: String,
}
impl DigestCredentials {
    pub fn new(username: String, password: String) -> Result<Self, SipError> {
        if !protected(&username, 256) || !bounded_secret(&password, 4096) {
            return Err(SipError::InvalidCredentials);
        }
        Ok(Self { username, password })
    }
    fn credential(&self) -> Credential {
        Credential {
            username: self.username.clone(),
            password: self.password.clone(),
            realm: None,
            auth_username: None,
        }
    }
}

/// DNS/proxy resolution precedes the external INVITE attempt. Keep the wire
/// URI intact for identity/digest while passing its resolved destination
/// separately. A no-destination preparation never starts this actor.
pub struct PreparedInvite {
    exchange: InviteExchange,
    destination: SipAddr,
    proxy: Option<SipAddr>,
    connection: Option<(Arc<SipEndpoint>, SipConnection)>,
}
impl PreparedInvite {
    pub fn new(
        request: Request,
        destination: SipAddr,
        proxy: Option<SipAddr>,
    ) -> Result<Self, SipIoError> {
        if !request.body.is_empty() || destination.get_socketaddr().is_err() {
            return Err(SipIoError::InvalidRequest);
        }
        let exchange = InviteExchange::new(request).map_err(|_| SipIoError::InvalidRequest)?;
        Ok(Self {
            exchange,
            destination,
            proxy,
            connection: None,
        })
    }
    pub(super) fn bind_connection(
        &mut self,
        endpoint: Arc<SipEndpoint>,
        connection: SipConnection,
    ) {
        self.connection = Some((endpoint, connection));
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CallObservation {
    pub outcome: Option<CallOutcome>,
    pub phase: FinalPhase,
    pub active: bool,
    pub ending: bool,
    pub uncertain: bool,
    pub cleanup_confirmed: bool,
    pub quiescent: bool,
    pub closed: bool,
}

#[derive(Clone)]
struct Identity {
    call_id: String,
    local_tag: String,
    remote_tag: String,
}
impl Identity {
    fn selected(invite: &Request, response: &Response) -> Option<Self> {
        Some(Self {
            call_id: invite.call_id_header().ok()?.value().to_owned(),
            local_tag: invite.from_header().ok()?.tag().ok()??.value().to_owned(),
            remote_tag: response.to_header().ok()?.tag().ok()??.value().to_owned(),
        })
    }
    fn matches(&self, request: &Request) -> bool {
        request
            .call_id_header()
            .is_ok_and(|h| h.value() == self.call_id)
            && request
                .from_header()
                .and_then(|h| h.tag())
                .ok()
                .flatten()
                .is_some_and(|tag| tag.value() == self.remote_tag)
            && request
                .to_header()
                .and_then(|h| h.tag())
                .ok()
                .flatten()
                .is_some_and(|tag| tag.value() == self.local_tag)
    }
}
enum Command {
    Hangup(oneshot::Sender<()>),
    Close(oneshot::Sender<Result<(), CallError>>),
    Control(Transaction),
}

/// Clones observe/control one actor; abandoning a command waiter cannot abort
/// signalling or release the original call's cleanup fence.
#[derive(Clone)]
pub struct DelayedInvite {
    commands: mpsc::Sender<Command>,
    hangup: CancellationToken,
    observation: watch::Receiver<CallObservation>,
    identity: watch::Receiver<Option<Identity>>,
    stopped: watch::Receiver<bool>,
}
impl DelayedInvite {
    /// Call only after native admission and original intent were persisted.
    /// The request is the original bodyless INVITE, not a replacement call.
    pub fn start(
        endpoint: Arc<SipEndpoint>,
        prepared: PreparedInvite,
        credentials: Option<DigestCredentials>,
        ring_seconds: u32,
        media: Arc<dyn DelayedInviteMedia>,
    ) -> Result<Self, SipIoError> {
        if !(1..=120).contains(&ring_seconds) {
            return Err(SipIoError::InvalidRequest);
        }
        let PreparedInvite {
            exchange,
            destination,
            proxy,
            connection,
        } = prepared;
        let connection = match connection {
            Some((owner, connection)) if Arc::ptr_eq(&owner, &endpoint) => Some(connection),
            Some(_) => return Err(SipIoError::InvalidRequest),
            None => None,
        };
        let mut lifecycle = DelayedOfferCall::default();
        lifecycle.start().map_err(|_| SipIoError::InvalidRequest)?;
        lifecycle
            .sending(1)
            .map_err(|_| SipIoError::InvalidRequest)?;
        let current = match &connection {
            Some(connection) => endpoint.connected_client(
                exchange.request().clone(),
                destination.clone(),
                connection.clone(),
                proxy.clone(),
            ),
            None => endpoint.resolved_client(
                exchange.request().clone(),
                destination.clone(),
                proxy.clone(),
            ),
        }?;
        let hangup = CancellationToken::new();
        let (commands, receiver) = mpsc::channel(8);
        let (identity, identity_rx) = watch::channel(None);
        let initial = CallObservation {
            outcome: None,
            phase: FinalPhase::Waiting,
            active: false,
            ending: false,
            uncertain: false,
            cleanup_confirmed: false,
            quiescent: true,
            closed: false,
        };
        let (observations, observation) = watch::channel(initial);
        let (stopped, stopped_rx) = watch::channel(false);
        let actor = CallActor {
            endpoint,
            destination,
            media,
            proxy,
            credentials,
            prepared_connection: connection,
            exchange,
            lifecycle,
            current: Some(current),
            cancelling: None,
            bye: None,
            flow: None,
            template: None,
            dialog: None,
            selected: None,
            ack: None,
            initial_ack: false,
            media_start: None,
            replies: JoinSet::new(),
            commands: receiver,
            hangup: hangup.clone(),
            hangup_seen: false,
            commands_open: true,
            identity,
            observations,
            ring_deadline: Some(Instant::now() + Duration::from_secs(ring_seconds as u64)),
            transaction: 1,
            listen_invite: true,
            listen_cancel: false,
            listen_bye: false,
            uncertain: false,
            closed: false,
        };
        tokio::spawn(async move {
            let mut actor = actor;
            actor.run().await;
            // Endpoint/transaction/media references drop before native release
            // can report a completed owner stop.
            drop(actor);
            stopped.send_replace(true);
        });
        Ok(Self {
            commands,
            hangup,
            observation,
            identity: identity_rx,
            stopped: stopped_rx,
        })
    }
    pub fn request_hangup(&self) {
        self.hangup.cancel();
    }
    /// A native hangup receipt may complete after this serialized transition;
    /// it does not claim that the remote dialog has finished cleanup yet.
    pub async fn hangup(&self) -> Result<(), CallError> {
        self.request_hangup();
        if self.observation().closed {
            return Ok(());
        }
        let (reply, received) = oneshot::channel();
        if self.commands.send(Command::Hangup(reply)).await.is_err() {
            return if self.observation().closed {
                Ok(())
            } else {
                Err(CallError::CleanupUnconfirmed)
            };
        }
        received.await.map_err(|_| CallError::CleanupUnconfirmed)
    }
    pub fn observation(&self) -> CallObservation {
        let mut current = *self.observation.borrow();
        if !current.closed && self.observation.has_changed().is_err() {
            current.active = false;
            current.uncertain = true;
            current.outcome.get_or_insert(CallOutcome::Unknown);
        }
        current
    }
    pub fn observations(&self) -> watch::Receiver<CallObservation> {
        self.observation.clone()
    }
    pub async fn wait_answer(&self) -> CallOutcome {
        let mut observation = self.observation.clone();
        loop {
            if let Some(outcome) = observation.borrow().outcome {
                return outcome;
            }
            if observation.changed().await.is_err() {
                return CallOutcome::Unknown;
            }
        }
    }
    /// The host retains responsibility for INFO, re-INVITE/ACK/CANCEL and
    /// unrelated dialogs. Those transactions are returned without consuming.
    pub fn deliver_control(&self, transaction: Transaction) -> Result<(), Transaction> {
        if !matches!(transaction.original.method, Method::Bye | Method::Options)
            || !self
                .identity
                .borrow()
                .as_ref()
                .is_some_and(|id| id.matches(&transaction.original))
        {
            return Err(transaction);
        }
        self.commands
            .try_send(Command::Control(transaction))
            .map_err(|error| match error.into_inner() {
                Command::Control(transaction) => transaction,
                _ => unreachable!(),
            })
    }
    async fn wait_stopped(&self) -> Result<(), CallError> {
        let mut stopped = self.stopped.clone();
        loop {
            if *stopped.borrow() {
                return Ok(());
            }
            if stopped.changed().await.is_err() {
                return Err(CallError::CleanupUnconfirmed);
            }
        }
    }
    /// A refused close leaves the original actor and its protocol handles live.
    pub async fn try_close(&self) -> Result<(), CallError> {
        if self.observation().closed {
            return self.wait_stopped().await;
        }
        let (reply, result) = oneshot::channel();
        if self.commands.send(Command::Close(reply)).await.is_err() {
            return if self.observation().closed {
                self.wait_stopped().await
            } else {
                Err(CallError::CleanupUnconfirmed)
            };
        }
        let result = result.await.unwrap_or(Err(CallError::CleanupUnconfirmed));
        if result.is_ok() || self.observation().closed {
            self.wait_stopped().await
        } else {
            result
        }
    }
    /// Matches the original disposal budget without cancelling protocol work.
    /// Timeout retains this handle/actor for a later cleanup observation.
    pub async fn close(&self) -> Result<(), CallError> {
        self.request_hangup();
        let mut observation = self.observation.clone();
        let wait = |mut observation: watch::Receiver<CallObservation>, quiescent: bool| async move {
            loop {
                let ready = {
                    let value = observation.borrow();
                    if quiescent {
                        value.quiescent
                    } else {
                        value.cleanup_confirmed
                    }
                };
                if ready {
                    return Ok(());
                }
                if observation.changed().await.is_err() {
                    return Err(CallError::CleanupUnconfirmed);
                }
            }
        };
        tokio::time::timeout(Duration::from_secs(35), wait(observation.clone(), false))
            .await
            .map_err(|_| CallError::CleanupUnconfirmed)??;
        observation.borrow_and_update();
        tokio::time::timeout(Duration::from_secs(5), wait(observation, true))
            .await
            .map_err(|_| CallError::CleanupUnconfirmed)??;
        self.try_close().await
    }
}

struct CallActor {
    endpoint: Arc<SipEndpoint>,
    destination: SipAddr,
    media: Arc<dyn DelayedInviteMedia>,
    proxy: Option<SipAddr>,
    credentials: Option<DigestCredentials>,
    prepared_connection: Option<SipConnection>,
    exchange: InviteExchange,
    lifecycle: DelayedOfferCall,
    current: Option<ClientExchange>,
    cancelling: Option<ClientExchange>,
    bye: Option<ClientExchange>,
    flow: Option<DialogFlow>,
    template: Option<Request>,
    dialog: Option<Dialog>,
    selected: Option<Response>,
    ack: Option<JoinHandle<(ClientExchange, bool)>>,
    initial_ack: bool,
    media_start: Option<JoinHandle<bool>>,
    replies: JoinSet<bool>,
    commands: mpsc::Receiver<Command>,
    hangup: CancellationToken,
    hangup_seen: bool,
    commands_open: bool,
    identity: watch::Sender<Option<Identity>>,
    observations: watch::Sender<CallObservation>,
    ring_deadline: Option<Instant>,
    transaction: u64,
    listen_invite: bool,
    listen_cancel: bool,
    listen_bye: bool,
    uncertain: bool,
    closed: bool,
}
async fn next(exchange: &mut Option<ClientExchange>) -> Result<Response, SipIoError> {
    match exchange.as_mut() {
        Some(exchange) => exchange.next_response().await,
        None => pending().await,
    }
}
impl CallActor {
    fn observation(&self) -> CallObservation {
        CallObservation {
            outcome: self.lifecycle.outcome(),
            phase: self.lifecycle.phase(),
            active: self.lifecycle.is_active(),
            ending: self.lifecycle.is_ending(),
            uncertain: self.uncertain,
            cleanup_confirmed: self.lifecycle.cleanup_confirmed(),
            quiescent: self.ack.is_none() && self.media_start.is_none() && self.replies.is_empty(),
            closed: self.closed,
        }
    }
    fn publish(&self) {
        let current = self.observation();
        self.observations.send_if_modified(|value| {
            if *value == current {
                false
            } else {
                *value = current;
                true
            }
        });
    }
    fn fail(&mut self) {
        let actions = self.lifecycle.fail();
        self.apply(actions);
        self.ring_deadline = None;
    }
    fn apply(&mut self, actions: Vec<CallAction>) {
        for action in actions {
            let result = match action {
                CallAction::Cancel => self
                    .exchange
                    .cancel_request()
                    .map_err(|_| SipIoError::InvalidRequest)
                    .and_then(|request| {
                        let cancelling = match &self.prepared_connection {
                            Some(connection) => self.endpoint.connected_client(
                                request,
                                self.destination.clone(),
                                connection.clone(),
                                self.proxy.clone(),
                            ),
                            None => self.endpoint.resolved_client(
                                request,
                                self.destination.clone(),
                                self.proxy.clone(),
                            ),
                        }?;
                        self.cancelling = Some(cancelling);
                        self.listen_cancel = true;
                        Ok(())
                    }),
                CallAction::Bye {
                    authenticated: false,
                } => self.send_bye(),
                CallAction::StartMedia => {
                    let media = self.media.clone();
                    self.media_start = Some(tokio::spawn(async move { media.start().await }));
                    Ok(())
                }
                CallAction::Uncertain => {
                    self.uncertain = true;
                    Ok(())
                }
                _ => Ok(()),
            };
            if result.is_err() {
                self.fail();
            }
        }
    }
    fn send_bye(&mut self) -> Result<(), SipIoError> {
        let mut bye = self
            .template
            .as_ref()
            .ok_or(SipIoError::InvalidRequest)?
            .clone();
        let sequence = bye
            .cseq_header()
            .and_then(|h| h.seq())
            .ok()
            .and_then(|value| value.checked_add(1))
            .ok_or(SipIoError::InvalidRequest)?;
        bye.method = Method::Bye;
        bye.body.clear();
        bye.headers.retain(|h| {
            !matches!(
                h,
                Header::Authorization(_)
                    | Header::ProxyAuthorization(_)
                    | Header::WwwAuthenticate(_)
                    | Header::ProxyAuthenticate(_)
                    | Header::ContentType(_)
                    | Header::ContentLength(_)
            )
        });
        bye.headers.unique_push(ContentLength::from(0).into());
        bye.headers
            .unique_push(CSeq::new(format!("{sequence} BYE")).into());
        bye.headers.unique_push(
            self.endpoint
                .inner()
                .get_via(None, None)
                .map_err(|_| SipIoError::InvalidRequest)?
                .into(),
        );
        bye.headers.unique_push(UserAgent::new("IvyPhone").into());
        self.bye = Some(self.endpoint.dialog_client(
            bye,
            self.flow.as_ref().ok_or(SipIoError::InvalidRequest)?,
            self.proxy.clone(),
        )?);
        self.listen_bye = true;
        Ok(())
    }
    async fn invite_response(&mut self, response: Response) -> Result<(), SipIoError> {
        let status = response.status_code.code();
        if status < 200 {
            let reliable = response.headers.iter().any(|h| matches!(h, Header::Require(value) if value.value().split(',').any(|value| value.trim() == "100rel")));
            let actions = self.lifecycle.provisional(self.transaction, reliable);
            self.apply(actions);
            return Ok(());
        }
        if let Some(ack) = self.exchange.ack_for_retransmission(&response).cloned() {
            self.start_ack(ack, false);
            return Ok(());
        }
        if self.selected.is_some() {
            return Ok(());
        }
        if matches!(status, 401 | 407) && !self.lifecycle.is_ending() && !self.hangup.is_cancelled()
        {
            if let Some(credentials) = &self.credentials {
                let original = self
                    .current
                    .as_mut()
                    .ok_or(SipIoError::Stopped)?
                    .completed()
                    .await?;
                if let Ok(continuation) = self
                    .exchange
                    .authenticate(
                        original,
                        &response,
                        &credentials.username,
                        &credentials.password,
                    )
                    .await
                {
                    if !self.hangup.is_cancelled() {
                        let next = self.transaction + 1;
                        self.lifecycle
                            .authenticated(self.transaction, next)
                            .map_err(|_| SipIoError::InvalidRequest)?;
                        let current = self
                            .endpoint
                            .continuation(continuation, self.proxy.clone())?;
                        let previous = self.current.replace(current).unwrap();
                        previous.retire().await?;
                        self.transaction = next;
                        return Ok(());
                    }
                }
            }
        }
        self.ring_deadline = None;
        if !self.lifecycle.final_response(self.transaction, status) {
            self.listen_invite = false;
            return Ok(());
        }
        self.exchange
            .select_final(&response)
            .map_err(|_| SipIoError::InvalidRequest)?;
        let identity = Identity::selected(self.exchange.request(), &response)
            .ok_or(SipIoError::InvalidRequest)?;
        self.dialog = Some(Dialog::new(
            identity.call_id.clone(),
            identity.local_tag.clone(),
            identity.remote_tag.clone(),
            0,
        ));
        self.identity.send_replace(Some(identity));
        self.selected = Some(response.clone());
        self.flow = Some(
            self.current
                .as_mut()
                .ok_or(SipIoError::Stopped)?
                .dialog_flow()
                .await?,
        );
        if self.hangup.is_cancelled() {
            let actions = self.lifecycle.hangup();
            self.apply(actions);
        }
        let offer = String::from_utf8_lossy(&response.body);
        let mut answer =
            if !self.lifecycle.is_ending() && !self.media.is_closed() && valid_offer(&response) {
                self.media.negotiate_offer(&offer)
            } else {
                None
            };
        if self.hangup.is_cancelled() {
            let actions = self.lifecycle.hangup();
            self.apply(actions);
        }
        let accepted = answer
            .as_ref()
            .is_some_and(|answer| !answer.trim().is_empty())
            && !self.media.failed();
        let action = self
            .lifecycle
            .negotiated(accepted, self.media.is_closed())
            .map_err(|_| SipIoError::InvalidRequest)?;
        if action == (CallAction::Acknowledge { reject_media: true }) {
            answer = if bounded_sdp(&offer) {
                self.media.rejected_answer(&offer)
            } else {
                None
            };
        }
        let ack = self
            .exchange
            .prepare_ack(&response, answer.as_deref(), self.endpoint.inner())
            .map_err(|_| SipIoError::InvalidRequest)?;
        self.template = Some(ack.clone());
        self.start_ack(ack, true);
        Ok(())
    }
    fn start_ack(&mut self, request: Request, initial: bool) {
        let mut current = self.current.take().expect("original INVITE is owned");
        self.initial_ack = initial;
        self.ack = Some(tokio::spawn(async move {
            let success = current.send_ack(&request).await.is_ok();
            (current, success)
        }));
    }
    async fn bye_response(&mut self, response: Response) -> Result<(), SipIoError> {
        if response.status_code.code() < 200 {
            return Ok(());
        }
        self.listen_bye = false;
        let actions = self
            .lifecycle
            .bye_response(response.status_code.code(), self.credentials.is_some())
            .map_err(|_| SipIoError::InvalidRequest)?;
        if actions.contains(&CallAction::Bye {
            authenticated: true,
        }) {
            let original = self
                .bye
                .as_mut()
                .ok_or(SipIoError::Stopped)?
                .completed()
                .await?;
            let sequence = original
                .original
                .cseq_header()
                .and_then(|h| h.seq())
                .ok()
                .and_then(|value| value.checked_add(1))
                .ok_or(SipIoError::InvalidRequest)?;
            let credentials = self
                .credentials
                .as_ref()
                .ok_or(SipIoError::InvalidRequest)?
                .credential();
            let continuation =
                handle_client_authenticate(sequence, original, response, &credentials)
                    .await
                    .map_err(|_| SipIoError::InvalidRequest)?;
            let next = self
                .endpoint
                .continuation(continuation, self.proxy.clone())?;
            let previous = self.bye.replace(next).unwrap();
            previous.retire().await?;
            self.listen_bye = true;
        } else {
            self.apply(actions);
        }
        Ok(())
    }
    fn control(&mut self, mut transaction: Transaction) {
        let request = &transaction.original;
        if !self
            .identity
            .borrow()
            .as_ref()
            .is_some_and(|id| id.matches(request))
        {
            return;
        }
        let status = if request.method == Method::Bye {
            let Some(sequence) = request
                .cseq_header()
                .and_then(|h| h.seq())
                .ok()
                .and_then(|value| i32::try_from(value).ok())
            else {
                return;
            };
            let Some(identity) = self.identity.borrow().clone() else {
                return;
            };
            let from = identity.remote_tag;
            let to = identity.local_tag;
            let checked = DialogRequest {
                call_id: &identity.call_id,
                from_tag: &from,
                to_tag: &to,
                sequence,
                branch: "",
            };
            let Some(status) = self.dialog.as_mut().and_then(|dialog| dialog.bye(&checked)) else {
                return;
            };
            if status == 200 {
                let actions = self.lifecycle.remote_bye();
                self.apply(actions);
                self.ring_deadline = None;
            }
            status
        } else {
            200
        };
        self.replies.spawn(async move {
            transaction
                .reply(if status == 200 {
                    StatusCode::OK
                } else {
                    StatusCode::from(500)
                })
                .await
                .is_ok()
        });
    }
    async fn retire(&mut self) -> Result<(), SipIoError> {
        for exchange in [&mut self.current, &mut self.cancelling, &mut self.bye] {
            if let Some(exchange) = exchange.take() {
                exchange.retire().await?;
            }
        }
        self.closed = true;
        self.publish();
        Ok(())
    }
    async fn run(&mut self) {
        loop {
            self.publish();
            if !self.commands_open
                && self.observation().cleanup_confirmed
                && self.observation().quiescent
            {
                if self.lifecycle.close().is_ok() {
                    let _ = self.retire().await;
                    return;
                }
            }
            tokio::select! {
                biased;
                command = self.commands.recv(), if self.commands_open => {
                    match command {
                        Some(Command::Control(transaction)) => self.control(transaction),
                        Some(Command::Hangup(reply)) => {
                            self.stop(); self.publish(); let _ = reply.send(());
                        }
                        Some(Command::Close(reply)) => {
                            if !self.observation().quiescent || self.lifecycle.close().is_err() { let _ = reply.send(Err(CallError::CleanupUnconfirmed)); }
                            else {
                                let result = self.retire().await.map_err(|_| CallError::CleanupUnconfirmed);
                                let _ = reply.send(result); return;
                            }
                        }
                        None => { self.commands_open = false; self.hangup.cancel(); }
                    }
                }
                _ = self.hangup.cancelled(), if !self.hangup_seen => {
                    self.stop();
                }
                _ = async { match self.ring_deadline { Some(deadline) => sleep_until(deadline).await, None => pending().await } } => {
                    self.ring_deadline = None; let actions = self.lifecycle.ring_deadline(); self.apply(actions);
                }
                result = async { self.ack.as_mut().unwrap().await }, if self.ack.is_some() => {
                    self.ack.take();
                    match result {
                        Ok((current, success)) => {
                            self.current = Some(current);
                            if self.initial_ack {
                                if self.hangup.is_cancelled() { self.stop(); }
                                let actions = self.lifecycle.acknowledged(success, self.media.is_closed()).unwrap_or_else(|_| vec![CallAction::Uncertain]); self.apply(actions);
                            } else if !success { self.fail(); }
                        }
                        Err(_) => {
                            self.listen_invite = false;
                            if self.initial_ack { let actions = self.lifecycle.acknowledged(false, self.media.is_closed()).unwrap_or_else(|_| vec![CallAction::Uncertain]); self.apply(actions); }
                            else { self.fail(); }
                        }
                    }
                }
                result = async { self.media_start.as_mut().unwrap().await }, if self.media_start.is_some() => {
                    self.media_start.take();
                    if self.hangup.is_cancelled() { self.stop(); }
                    let actions = self.lifecycle.media_started(result.unwrap_or(false), self.media.is_closed()).unwrap_or_else(|_| vec![CallAction::Uncertain]); self.apply(actions);
                }
                result = self.replies.join_next(), if !self.replies.is_empty() => {
                    if !matches!(result, Some(Ok(true))) { self.fail(); }
                }
                result = next(&mut self.current), if self.listen_invite && self.current.is_some() => {
                    match result { Ok(response) => if self.invite_response(response).await.is_err() { self.listen_invite = false; self.fail(); }, Err(_) => { self.listen_invite = false; self.fail(); } }
                }
                result = next(&mut self.cancelling), if self.listen_cancel => {
                    match result {
                        Ok(response) if response.status_code.code() >= 200 => self.listen_cancel = false,
                        Ok(_) => {}, Err(_) => { self.listen_cancel = false; self.fail(); }
                    }
                }
                result = next(&mut self.bye), if self.listen_bye => {
                    match result { Ok(response) => if self.bye_response(response).await.is_err() { self.listen_bye = false; self.fail(); }, Err(_) => { self.listen_bye = false; self.fail(); } }
                }
            }
        }
    }
    fn stop(&mut self) {
        self.hangup_seen = true;
        self.ring_deadline = None;
        let actions = self.lifecycle.hangup();
        self.apply(actions);
    }
}

fn bounded_sdp(offer: &str) -> bool {
    !offer.trim().is_empty() && offer.encode_utf16().count() <= 65_536
}
fn valid_offer(response: &Response) -> bool {
    let offer = String::from_utf8_lossy(&response.body);
    bounded_sdp(&offer)
        && response.headers.iter().any(|header| matches!(header, Header::ContentType(value) if value.value().split(';').next().unwrap_or("").trim().eq_ignore_ascii_case("application/sdp")))
}
