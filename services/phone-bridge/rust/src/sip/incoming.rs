//! Original incoming transaction, answer and accepted dialog ownership.
use super::{
    CallError, CallObservation, CallOutcome, Dialog, DialogRequest, FinalPhase, HostedSipMedia,
    SipEndpoint, SipHostCall, SipHostError, endpoint::DialogFlow, server::ServerExchange,
};
use rsipstack::{
    sip::{Header, Method, Request, StatusCode, headers::*, prelude::*},
    transaction::Transaction,
};
use std::{
    future::pending,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    sync::{Notify, mpsc, watch},
    task::JoinHandle,
    time::{Instant, sleep_until},
};
use tokio_util::sync::CancellationToken;

#[derive(Default)]
struct Intent {
    answer: bool,
    ending: bool,
    late_connection: bool,
}
struct Control {
    intent: Mutex<Intent>,
    changed: Notify,
    ring_stop: CancellationToken,
}
impl Control {
    fn stop(&self) {
        self.intent.lock().unwrap().ending = true;
        self.changed.notify_one();
    }
    fn answer(&self, before: impl FnOnce() -> Result<(), SipHostError>) -> Result<(), CallError> {
        let mut intent = self.intent.lock().unwrap();
        if intent.answer || intent.ending {
            return Err(CallError::AlreadyAttempted);
        }
        intent.answer = true;
        self.ring_stop.cancel();
        before().map_err(|_| CallError::CleanupUnconfirmed)?;
        self.changed.notify_one();
        Ok(())
    }
    fn expire(&self) {
        let mut intent = self.intent.lock().unwrap();
        if !intent.answer {
            intent.ending = true;
            self.changed.notify_one();
        }
    }
}
#[derive(Clone)]
struct Identity {
    wire: String,
    local: String,
    remote: String,
    sequence: i32,
}
impl Identity {
    fn parse(request: &Request) -> Option<Self> {
        Some(Self {
            wire: request.call_id_header().ok()?.value().to_owned(),
            local: request.to_header().ok()?.tag().ok()??.value().to_owned(),
            remote: request.from_header().ok()?.tag().ok()??.value().to_owned(),
            sequence: i32::try_from(request.cseq_header().ok()?.seq().ok()?).ok()?,
        })
    }
    fn matches(&self, request: &Request) -> bool {
        request
            .call_id_header()
            .is_ok_and(|h| h.value() == self.wire)
            && request
                .from_header()
                .and_then(|h| h.tag())
                .ok()
                .flatten()
                .is_some_and(|t| t.value() == self.remote)
            && request
                .to_header()
                .and_then(|h| h.tag())
                .ok()
                .flatten()
                .is_some_and(|t| t.value() == self.local)
    }
}
#[derive(Clone)]
pub(super) struct IncomingInvite {
    control: Arc<Control>,
    commands: mpsc::Sender<Transaction>,
    identity: Option<Identity>,
    observation: watch::Receiver<CallObservation>,
    done: watch::Receiver<Option<Result<(), CallError>>>,
}
impl IncomingInvite {
    pub fn start(
        original: SipHostCall,
        mut transaction: Transaction,
        endpoint: Arc<SipEndpoint>,
        ring_seconds: u8,
    ) -> Self {
        if let Ok(to) = transaction.original.to_header().cloned() {
            if to.tag().ok().flatten().is_none() {
                transaction
                    .original
                    .headers
                    .unique_push(to.with_tag(rsipstack::transaction::make_tag()).into());
            }
        }
        let identity = Identity::parse(&transaction.original);
        let control = Arc::new(Control {
            intent: Mutex::new(Intent::default()),
            changed: Notify::new(),
            ring_stop: CancellationToken::new(),
        });
        let initial = CallObservation {
            outcome: None,
            phase: FinalPhase::Waiting,
            active: false,
            ending: false,
            uncertain: false,
            cleanup_confirmed: false,
            quiescent: false,
            closed: false,
        };
        let (observations, observation) = watch::channel(initial);
        let (finished, done) = watch::channel(None);
        let (commands, receiver) = mpsc::channel(8);
        let mut actor = IncomingActor {
            original,
            transaction: Some(transaction),
            exchange: None,
            endpoint,
            control: control.clone(),
            commands: receiver,
            identity: identity.clone(),
            observations,
            current: initial,
            media: None,
            media_start: None,
            bye: None,
            remote_ended: false,
            established: false,
            dialog: None,
            reinvite: std::collections::VecDeque::new(),
            failed_controls: vec![],
        };
        tokio::spawn(async move {
            let result = actor.run(ring_seconds).await;
            actor.control.ring_stop.cancel();
            if result.is_err() {
                actor.current.uncertain = true;
                actor.current.outcome = Some(CallOutcome::Unknown);
            }
            actor.current.active = false;
            actor.current.ending = true;
            actor.current.outcome.get_or_insert(CallOutcome::Ended);
            actor.current.cleanup_confirmed = result.is_ok();
            actor.current.quiescent = result.is_ok();
            actor.current.closed = result.is_ok();
            let ended = actor.current;
            let observations = actor.observations.clone();
            if result.is_ok() {
                drop(actor);
                observations.send_replace(ended);
                finished.send_replace(Some(Ok(())));
            } else {
                observations.send_replace(ended);
                finished.send_replace(Some(Err(CallError::CleanupUnconfirmed)));
                pending::<()>().await;
                drop(actor);
            }
        });
        Self {
            control,
            commands,
            identity,
            observation,
            done,
        }
    }
    pub fn request_hangup(&self) {
        self.control.stop();
    }
    pub fn request_answer(
        &self,
        before: impl FnOnce() -> Result<(), SipHostError>,
    ) -> Result<(), CallError> {
        self.control.answer(before)
    }
    pub fn deliver_control(&self, transaction: Transaction) -> Result<(), Transaction> {
        if !matches!(
            transaction.original.method,
            Method::Bye | Method::Options | Method::Invite
        ) || !self
            .identity
            .as_ref()
            .is_some_and(|id| id.matches(&transaction.original))
        {
            return Err(transaction);
        }
        self.commands
            .try_send(transaction)
            .map_err(|e| e.into_inner())
    }
    pub fn observation(&self) -> CallObservation {
        let mut current = *self.observation.borrow();
        let intent = self.control.intent.lock().unwrap();
        current.ending |= intent.ending;
        if intent.ending && !intent.late_connection {
            current.active = false;
        }
        if self.done.has_changed().is_err() && self.done.borrow().is_none() {
            current.uncertain = true;
            current.ending = true;
            current.outcome = Some(CallOutcome::Unknown);
        }
        current
    }
    pub fn observations(&self) -> watch::Receiver<CallObservation> {
        self.observation.clone()
    }
    pub async fn wait_answer(&self) -> CallOutcome {
        let mut observed = self.observation.clone();
        loop {
            if let Some(outcome) = observed.borrow().outcome {
                return outcome;
            }
            if observed.changed().await.is_err() {
                return CallOutcome::Unknown;
            }
        }
    }
    pub async fn close(&self) -> Result<(), CallError> {
        self.request_hangup();
        let mut done = self.done.clone();
        tokio::time::timeout(Duration::from_secs(35), async {
            loop {
                if let Some(result) = *done.borrow_and_update() {
                    return result;
                }
                done.changed()
                    .await
                    .map_err(|_| CallError::CleanupUnconfirmed)?;
            }
        })
        .await
        .map_err(|_| CallError::CleanupUnconfirmed)?
    }
}
struct Negotiation {
    sequence: i32,
    server: ServerExchange,
    deadline: Instant,
}
struct IncomingActor {
    original: SipHostCall,
    transaction: Option<Transaction>,
    exchange: Option<ServerExchange>,
    endpoint: Arc<SipEndpoint>,
    control: Arc<Control>,
    commands: mpsc::Receiver<Transaction>,
    identity: Option<Identity>,
    observations: watch::Sender<CallObservation>,
    current: CallObservation,
    media: Option<Arc<dyn HostedSipMedia>>,
    media_start: Option<JoinHandle<bool>>,
    bye: Option<super::ClientExchange>,
    remote_ended: bool,
    established: bool,
    dialog: Option<Dialog>,
    reinvite: std::collections::VecDeque<Negotiation>,
    failed_controls: Vec<Transaction>,
}
fn unknown<T>(_: T) -> CallError {
    CallError::CleanupUnconfirmed
}
fn sdp(request: &Request) -> Option<&str> {
    let body = std::str::from_utf8(&request.body).ok()?;
    if !request.headers.iter().any(|header| {
        matches!(header,Header::ContentType(value)
        if crate::media::valid_sdp(Some(value.value()),body))
    }) {
        return None;
    }
    Some(body)
}
impl IncomingActor {
    fn publish(&self) {
        self.observations.send_replace(self.current);
    }
    fn end(&mut self) {
        if !self.control.intent.lock().unwrap().late_connection || self.remote_ended {
            self.current.active = false;
        }
        self.current.ending = true;
        self.current.outcome.get_or_insert(CallOutcome::Ended);
        self.publish();
    }
    fn ending(&self) -> bool {
        self.current.ending || self.control.intent.lock().unwrap().ending
    }
    async fn run(&mut self, ring_seconds: u8) -> Result<(), CallError> {
        match self.original.constructed().await {
            Ok(()) => (),
            Err(SipHostError::CleanupUnconfirmed) => return Err(CallError::CleanupUnconfirmed),
            Err(_) => {
                return self
                    .transaction
                    .as_mut()
                    .unwrap()
                    .reply(StatusCode::Forbidden)
                    .await
                    .map_err(unknown);
            }
        }
        let identity = self.identity.clone().ok_or(CallError::CleanupUnconfirmed)?;
        self.dialog = Some(Dialog::new(
            identity.wire.clone(),
            identity.local.clone(),
            identity.remote.clone(),
            identity.sequence,
        ));
        self.media = self.original.hosted_media();
        let request = self.transaction.as_ref().unwrap().original.clone();
        let flow = DialogFlow::from_server(self.transaction.as_ref().unwrap());
        self.exchange = Some(ServerExchange::start(self.transaction.take().unwrap()));
        if self.ending() {
            return self.finish_response(StatusCode::Decline).await;
        }
        self.exchange
            .as_ref()
            .unwrap()
            .reply(StatusCode::Ringing, vec![], None)
            .await?;
        let expiry = self.control.clone();
        let ring_stop = self.control.ring_stop.clone();
        let timer = tokio::spawn(async move {
            tokio::select! {_ = ring_stop.cancelled()=>(),_ = tokio::time::sleep(Duration::from_secs(ring_seconds.into()))=>expiry.expire()}
        });
        loop {
            if self.ending() {
                self.control.ring_stop.cancel();
                let _ = timer.await;
                return self.finish_response(StatusCode::Decline).await;
            }
            if self.control.intent.lock().unwrap().answer {
                break;
            }
            tokio::select! {
                _ = self.control.changed.notified()=>(),
                transaction=self.commands.recv()=>{let Some(transaction)=transaction else{return Err(CallError::CleanupUnconfirmed);};self.control_request(transaction).await?;},
                event=self.exchange.as_mut().unwrap().next()=>match event{
                    Some(request) if request.method==Method::Cancel=>{self.control.ring_stop.cancel();let _=timer.await;return self.finish_response(StatusCode::RequestTerminated).await;},
                    None=>return Err(CallError::CleanupUnconfirmed),_=>(),
                },
            }
        }
        self.control.ring_stop.cancel();
        let _ = timer.await;
        let media = self
            .media
            .as_ref()
            .ok_or(CallError::CleanupUnconfirmed)?
            .clone();
        let delayed = request.body.is_empty();
        let body = if delayed {
            media.initial_offer()
        } else {
            sdp(&request).and_then(|body| media.negotiate_offer(body))
        };
        if media.is_closed() || media.failed() || self.ending() {
            return self.finish_response(StatusCode::Decline).await;
        }
        let Some(body) = body else {
            return self.finish_response(StatusCode::from(406)).await;
        };
        self.current.phase = FinalPhase::Acknowledging;
        self.publish();
        self.exchange
            .as_ref()
            .unwrap()
            .reply(
                StatusCode::OK,
                self.answer_headers(&request)?,
                Some(body.into_bytes()),
            )
            .await?;
        let deadline = Instant::now() + Duration::from_secs(32);
        loop {
            tokio::select! {
                event=self.exchange.as_mut().unwrap().next()=>match event{
                    Some(ack) if ack.method==Method::Ack=>{
                        if !identity.matches(&ack){continue;}
                        let local_ending=self.control.intent.lock().unwrap().ending;
                        self.established=local_ending||!delayed||sdp(&ack).is_some_and(|body|media.accept_answer(body));
                        if !self.established{self.end();}
                        else if local_ending&&!delayed {
                            // With an initial offer, the original late ACK
                            // restores SIP-active observation after hangup.
                            // The delayed answer stays ended. Audio authority
                            // stays revoked, and BYE still owns the fence.
                            self.control.intent.lock().unwrap().late_connection=true;
                            self.current.active=true;self.current.outcome=Some(CallOutcome::Connected);self.publish();
                        }
                        break;
                    },
                    None=>return Err(CallError::CleanupUnconfirmed),_=>(),
                },
                transaction=self.commands.recv()=>{let Some(transaction)=transaction else{return Err(CallError::CleanupUnconfirmed);};self.control_request(transaction).await?;},
                _ = sleep_until(deadline)=>return Err(CallError::CleanupUnconfirmed),
            }
        }
        self.exchange.as_ref().unwrap().close().await?;
        self.exchange.take();
        if !self.ending() {
            self.current.phase = FinalPhase::StartingMedia;
            self.publish();
            let original = media.clone();
            self.media_start = Some(tokio::spawn(async move { original.start().await }));
        }
        self.connected(request, flow).await
    }
    fn answer_headers(&self, request: &Request) -> Result<Vec<Header>, CallError> {
        let contact = request
            .to_header()
            .and_then(|h| h.typed())
            .map_err(unknown)?
            .uri;
        Ok(vec![
            Contact::new(format!("<{contact}>")).into(),
            ContentType::new("application/sdp").into(),
        ])
    }
    async fn finish_response(&mut self, status: StatusCode) -> Result<(), CallError> {
        self.exchange
            .as_ref()
            .unwrap()
            .reply(status, vec![], None)
            .await?;
        self.exchange.as_ref().unwrap().close().await?;
        self.exchange.take();
        self.current.phase = FinalPhase::Refused;
        self.end();
        Ok(())
    }
    async fn control_request(&mut self, transaction: Transaction) -> Result<(), CallError> {
        let id = self
            .identity
            .as_ref()
            .ok_or(CallError::CleanupUnconfirmed)?;
        let request = &transaction.original;
        let sequence = request
            .cseq_header()
            .and_then(|h| h.seq())
            .ok()
            .and_then(|s| i32::try_from(s).ok())
            .ok_or(CallError::CleanupUnconfirmed)?;
        let checked = DialogRequest {
            call_id: &id.wire,
            from_tag: &id.remote,
            to_tag: &id.local,
            sequence,
            branch: "",
        };
        if request.method == Method::Invite {
            if !self.current.active || self.ending() {
                return self
                    .reply_control(transaction, StatusCode::CallTransactionDoesNotExist)
                    .await;
            }
            // The original incoming SDK accepts stale/overlapping offers. Its
            // bodyless re-INVITE returns 406; the outgoing policy is separate.
            let request = transaction.original.clone();
            let server = ServerExchange::start(transaction);
            let body =
                sdp(&request).and_then(|body| self.media.as_ref().unwrap().negotiate_offer(body));
            if let Some(body) = body {
                server
                    .reply(
                        StatusCode::OK,
                        self.answer_headers(&request)?,
                        Some(body.into_bytes()),
                    )
                    .await?;
                self.reinvite.push_back(Negotiation {
                    sequence,
                    server,
                    deadline: Instant::now() + Duration::from_secs(32),
                });
            } else {
                server.reply(StatusCode::from(406), vec![], None).await?;
                server.close().await?;
            }
            return Ok(());
        }
        let status = if request.method == Method::Bye {
            let status = self
                .dialog
                .as_mut()
                .ok_or(CallError::CleanupUnconfirmed)?
                .bye(&checked)
                .ok_or(CallError::CleanupUnconfirmed)?;
            if status == 200 {
                self.remote_ended = true;
                self.end();
            }
            StatusCode::from(status)
        } else {
            StatusCode::OK
        };
        self.reply_control(transaction, status).await
    }
    async fn reply_control(
        &mut self,
        mut original: Transaction,
        status: StatusCode,
    ) -> Result<(), CallError> {
        if original.reply(status).await.is_err() {
            self.failed_controls.push(original);
            Err(CallError::CleanupUnconfirmed)
        } else {
            Ok(())
        }
    }
    fn bye_request(&self, original: &Request) -> Result<Request, CallError> {
        let remote = original
            .contact_header()
            .and_then(|h| h.typed())
            .map(|h| h.uri)
            .or_else(|_| {
                original
                    .from_header()
                    .and_then(|h| h.typed())
                    .map(|h| h.uri)
            })
            .map_err(unknown)?;
        let headers: Vec<Header> = vec![
            self.endpoint
                .inner()
                .get_via(None, None)
                .map_err(unknown)?
                .into(),
            From::new(original.to_header().map_err(unknown)?.value()).into(),
            To::new(original.from_header().map_err(unknown)?.value()).into(),
            original.call_id_header().map_err(unknown)?.clone().into(),
            CSeq::new("1 BYE").into(),
            MaxForwards::from(70).into(),
            ContentLength::from(0).into(),
            UserAgent::new("IvyPhone").into(),
        ];
        let mut request = Request {
            method: Method::Bye,
            uri: remote,
            version: rsipstack::sip::Version::V2,
            headers: headers.into(),
            body: vec![],
        };
        request.headers.extend(
            original
                .headers
                .iter()
                .filter_map(|h| match h {
                    Header::RecordRoute(v) => Some(Route::new(v.value()).into()),
                    _ => None,
                })
                .collect(),
        );
        Ok(request)
    }
    async fn connected(&mut self, request: Request, flow: DialogFlow) -> Result<(), CallError> {
        let mut stopping = false;
        loop {
            if self.ending() && !stopping {
                stopping = true;
                self.end();
                if self.established && !self.remote_ended {
                    self.bye = Some(
                        self.endpoint
                            .dialog_client(self.bye_request(&request)?, &flow, None)
                            .map_err(unknown)?,
                    );
                }
            }
            if stopping && self.bye.is_none() && self.media_start.is_none() {
                self.commands.close();
                while let Some(original) = self.commands.recv().await {
                    self.control_request(original).await?;
                }
                while let Some(pending) = self.reinvite.pop_front() {
                    pending.server.close().await?;
                }
                return Ok(());
            }
            let reinvite_deadline = self.reinvite.front().map(|p| p.deadline);
            tokio::select! {
                _ = self.control.changed.notified()=>(),
                transaction=self.commands.recv()=>{let Some(transaction)=transaction else{return Err(CallError::CleanupUnconfirmed);};self.control_request(transaction).await?;},
                result=async{self.media_start.as_mut().unwrap().await},if self.media_start.is_some()=>{
                    self.media_start.take();let media=self.media.as_ref().unwrap();
                    if !result.unwrap_or(false)||media.failed()||media.is_closed(){self.end();}
                    else if !self.ending(){self.current.phase=FinalPhase::Confirmed;self.current.active=true;self.current.outcome=Some(CallOutcome::Connected);self.publish();}
                },
                response=async{self.bye.as_mut().unwrap().next_response().await},if self.bye.is_some()=>{
                    let response=response.map_err(unknown)?;if response.status_code.code()>=200{
                        if response.status_code.code()>=300{return Err(CallError::CleanupUnconfirmed);}
                        self.bye.take().unwrap().retire().await.map_err(unknown)?;
                    }
                },
                event=async{self.reinvite.front_mut().unwrap().server.next().await},if !self.reinvite.is_empty()=>{
                    let pending=self.reinvite.front().unwrap();
                    match event {
                        Some(ack) if ack.method==Method::Ack=>{
                            let id=self.identity.as_ref().unwrap();
                            let sequence=ack.cseq_header().and_then(|h|h.seq()).ok().and_then(|s|i32::try_from(s).ok()).ok_or(CallError::CleanupUnconfirmed)?;
                            if !id.matches(&ack)||sequence!=pending.sequence{continue;}
                            let pending=self.reinvite.pop_front().unwrap();pending.server.close().await?;
                        },
                        Some(cancel) if cancel.method==Method::Cancel=>{pending.server.reply(StatusCode::RequestTerminated,vec![],None).await?;self.end();},
                        Some(_)=>(),None=>return Err(CallError::CleanupUnconfirmed),
                    }
                },
                _ = async{match reinvite_deadline{Some(deadline)=>sleep_until(deadline).await,None=>pending().await}}=>return Err(CallError::CleanupUnconfirmed),
            }
        }
    }
}
