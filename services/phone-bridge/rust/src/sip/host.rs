//! Native SIP ownership. Reservation and publication are serialized; protocol
//! and disposal work outlive command observers without holding that lock.
use super::{
    BindError, BoundSipTransport, CallError, CallObservation, CallOutcome, DelayedInvite,
    DelayedInviteMedia, DigestCredentials, InviteExchange, RegistrationError,
    RegistrationObservation, RegistrationPeer, SipIoError, SipRegistration,
    SipRegistrationObserver, SipRegistrationSettings, invite::bounded_secret,
};
use rsipstack::{
    sip::{Request, Transport},
    transaction::{TransactionReceiver, endpoint::EndpointOption, transaction::Transaction},
    transport::SipAddr,
};
use std::{
    future::Future,
    net::SocketAddr,
    pin::Pin,
    sync::{Arc, Mutex, OnceLock, Weak},
};
use tokio::sync::Notify;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SipHostError {
    InvalidRequest,
    OperationConflict,
    NotReady,
    Stopping,
    Binding(BindError),
    Registration(RegistrationError),
    Signalling(SipIoError),
    CleanupUnconfirmed,
}
impl std::fmt::Display for SipHostError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for SipHostError {}

/// The native media owner revokes authority synchronously and separately
/// acknowledges disposal of its original RTP/PCM work. Neither is inferred.
/// Authority revocation must not block or re-enter the host owner.
pub trait HostedSipMedia: DelayedInviteMedia {
    fn request_close(&self);
    fn close(&self) -> Pin<Box<dyn Future<Output = Result<(), SipHostError>> + Send + '_>>;
}

pub struct SipHostBinding {
    local: SocketAddr,
    protocol: Transport,
    proxy: Option<SipAddr>,
    maximum_calls: usize,
}
impl SipHostBinding {
    /// Native DTO validation owns ring/offer/codec/RTP settings. This is the
    /// validated socket/admission portion, not a replacement wire DTO.
    pub fn new(
        local: SocketAddr,
        protocol: Transport,
        proxy: Option<SipAddr>,
        maximum_calls: usize,
    ) -> Result<Self, SipHostError> {
        if !matches!(protocol, Transport::Udp | Transport::Tcp | Transport::Tls)
            || !(1..=32).contains(&maximum_calls)
            || proxy.as_ref().is_some_and(|proxy| {
                proxy.r#type.unwrap_or_default() != protocol
                    || proxy.get_socketaddr().is_err()
                    || proxy
                        .get_socketaddr()
                        .is_ok_and(|address| address.port() == 0)
            })
        {
            return Err(SipHostError::InvalidRequest);
        }
        Ok(Self {
            local,
            protocol,
            proxy,
            maximum_calls,
        })
    }
}
// No Debug: protected credentials stay separate from status and receipts.
pub struct SipRegistrationPlan {
    settings: SipRegistrationSettings,
    password: String,
}
impl SipRegistrationPlan {
    pub fn new(settings: SipRegistrationSettings, password: String) -> Result<Self, SipHostError> {
        if !bounded_secret(&password, 4096) {
            return Err(SipHostError::InvalidRequest);
        }
        Ok(Self { settings, password })
    }
}
pub struct SipHostConfiguration {
    binding: SipHostBinding,
    registration: Option<SipRegistrationPlan>,
}
impl SipHostConfiguration {
    pub fn new(
        binding: SipHostBinding,
        registration: Option<SipRegistrationPlan>,
    ) -> Result<Self, SipHostError> {
        if registration
            .as_ref()
            .is_some_and(|plan| plan.settings.protocol() != binding.protocol)
        {
            return Err(SipHostError::InvalidRequest);
        }
        Ok(Self {
            binding,
            registration,
        })
    }
}

struct Completion<T> {
    result: OnceLock<Result<T, SipHostError>>,
    changed: Notify,
}
impl<T: Clone> Completion<T> {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            result: OnceLock::new(),
            changed: Notify::new(),
        })
    }
    fn finish(&self, result: Result<T, SipHostError>) {
        let _ = self.result.set(result);
        self.changed.notify_waiters();
    }
    fn pending(&self) -> bool {
        self.result.get().is_none()
    }
    async fn wait(&self) -> Result<T, SipHostError> {
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if let Some(result) = self.result.get() {
                return result.clone();
            }
            changed.await;
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SipHostCallPhase {
    Prepared,
    Dialing,
    Connected,
    LocalEnded,
    OutcomeUnknown,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SipHostCallObservation {
    pub id: String,
    pub phase: SipHostCallPhase,
    pub attempted: bool,
    pub ending: bool,
    pub signalling: Option<CallObservation>,
}
struct CallState {
    attempted: bool,
    ending: bool,
    failed_before_send: bool,
    invite: Option<DelayedInvite>,
    preparing: Option<Arc<Completion<()>>>,
    releasing: Option<Arc<Completion<()>>>,
}
struct Call {
    id: String,
    host: Weak<Mutex<State>>,
    state: Mutex<CallState>,
    media: Arc<dyn HostedSipMedia>,
}
impl Call {
    fn observation(&self) -> SipHostCallObservation {
        let state = self.state.lock().unwrap();
        let signalling = state.invite.as_ref().map(DelayedInvite::observation);
        let phase = if signalling.is_some_and(|s| s.uncertain) {
            SipHostCallPhase::OutcomeUnknown
        } else if state.ending
            || state.failed_before_send
            || signalling.is_some_and(|s| s.outcome.is_some() && !s.active)
        {
            SipHostCallPhase::LocalEnded
        } else if signalling.is_some_and(|s| s.active) {
            SipHostCallPhase::Connected
        } else if state.attempted {
            SipHostCallPhase::Dialing
        } else {
            SipHostCallPhase::Prepared
        };
        SipHostCallObservation {
            id: self.id.clone(),
            phase,
            attempted: state.attempted,
            ending: state.ending || signalling.is_some_and(|s| s.ending),
            signalling,
        }
    }
    fn hangup(&self) {
        let mut state = self.state.lock().unwrap();
        state.ending = true;
        self.media.request_close();
        if let Some(invite) = &state.invite {
            invite.request_hangup();
        }
    }
}
/// An opaque reference to one original slot. Reusing an ID never makes an old
/// handle authority over a later call or a call in a different host.
#[derive(Clone)]
pub struct SipHostCall(Arc<Call>);
impl SipHostCall {
    pub fn observation(&self) -> SipHostCallObservation {
        self.0.observation()
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SipHostObservation {
    pub configured: bool,
    pub stopping: bool,
    pub closed: bool,
    pub endpoint: Option<SocketAddr>,
    pub registration: Option<RegistrationObservation>,
    pub calls: Vec<SipHostCallObservation>,
}
struct State {
    configured: bool,
    stopping: bool,
    closed: bool,
    configuration: Option<SipHostConfiguration>,
    configuring: Option<Arc<Completion<SocketAddr>>>,
    binding: Option<Arc<BoundSipTransport>>,
    registration: Option<SipRegistration>,
    registration_view: Option<SipRegistrationObserver>,
    reconnecting: Option<Arc<Completion<RegistrationObservation>>>,
    calls: Vec<Arc<Call>>,
    closing: Option<Arc<Completion<()>>>,
}
type Shared = Arc<Mutex<State>>;
struct Handle(Shared);
impl Drop for Handle {
    fn drop(&mut self) {
        if tokio::runtime::Handle::try_current().is_ok() {
            let closing = begin_close(&self.0);
            let owner = self.0.clone();
            tokio::spawn(async move {
                // A forgotten failed owner must not silently release its
                // original socket. Native process exit remains the boundary.
                if closing.wait().await.is_err() {
                    std::future::pending::<()>().await;
                }
                drop(owner);
            });
        }
    }
}
#[derive(Clone)]
pub struct SipHost(Arc<Handle>);
impl Default for SipHost {
    fn default() -> Self {
        Self::new()
    }
}
impl SipHost {
    pub fn new() -> Self {
        Self(Arc::new(Handle(Arc::new(Mutex::new(State {
            configured: false,
            stopping: false,
            closed: false,
            configuration: None,
            configuring: None,
            binding: None,
            registration: None,
            registration_view: None,
            reconnecting: None,
            calls: Vec::new(),
            closing: None,
        })))))
    }
    fn shared(&self) -> &Shared {
        &self.0.0
    }
    pub fn observation(&self) -> SipHostObservation {
        let state = self.shared().lock().unwrap();
        SipHostObservation {
            configured: state.configured,
            stopping: state.stopping,
            closed: state.closed,
            endpoint: state.binding.as_ref().map(|b| b.local_addr()),
            registration: state
                .registration_view
                .as_ref()
                .map(SipRegistrationObserver::observation),
            calls: state.calls.iter().map(|call| call.observation()).collect(),
        }
    }
    /// Validation precedes the configured-once latch. A partial bind/start
    /// retains that latch and its original resources; a waiter cannot undo it.
    pub async fn configure(
        &self,
        configuration: SipHostConfiguration,
    ) -> Result<SocketAddr, SipHostError> {
        let completion = {
            let mut state = self.shared().lock().unwrap();
            if state.stopping {
                return Err(SipHostError::Stopping);
            }
            if state.configured {
                return Err(SipHostError::OperationConflict);
            }
            let completion = Completion::new();
            state.configured = true;
            state.configuration = Some(configuration);
            state.configuring = Some(completion.clone());
            let owner = self.shared().clone();
            let done = completion.clone();
            tokio::spawn(async move {
                let (local, protocol) = {
                    let s = owner.lock().unwrap();
                    let b = &s.configuration.as_ref().unwrap().binding;
                    (b.local, b.protocol)
                };
                let bound = match protocol {
                    Transport::Udp => {
                        BoundSipTransport::udp(local, EndpointOption::default()).await
                    }
                    Transport::Tcp => BoundSipTransport::tcp(local, EndpointOption::default()),
                    Transport::Tls => BoundSipTransport::tls(local, EndpointOption::default()),
                    _ => unreachable!(),
                };
                let result = bound.map_err(SipHostError::Binding).and_then(|bound| {
                    let endpoint = bound.local_addr();
                    let bound = Arc::new(bound);
                    let mut state = owner.lock().unwrap();
                    state.binding = Some(bound.clone());
                    let config = state.configuration.as_ref().unwrap();
                    if let Some(plan) = &config.registration {
                        let registration = SipRegistration::start(
                            bound,
                            plan.settings.clone(),
                            plan.password.clone(),
                            config.binding.proxy.clone(),
                        )
                        .map_err(SipHostError::Registration)?;
                        state.registration_view = Some(registration.observer());
                        state.registration = Some(registration);
                    }
                    Ok(endpoint)
                });
                done.finish(result);
            });
            completion
        };
        completion.wait().await
    }
    pub fn matches_registered_peer(&self, peer: RegistrationPeer) -> bool {
        let state = self.shared().lock().unwrap();
        !state.stopping
            && state
                .registration_view
                .as_ref()
                .is_some_and(|view| view.matches_peer(peer))
    }
    /// The media constructor runs only after admission succeeds. It must not
    /// block or re-enter this owner; asynchronous startup belongs to the call.
    pub fn prepare(
        &self,
        id: String,
        create: impl FnOnce() -> Result<Arc<dyn HostedSipMedia>, SipHostError>,
    ) -> Result<SipHostCall, SipHostError> {
        if !super::super::native_rpc::guid_d(&id) {
            return Err(SipHostError::InvalidRequest);
        }
        let mut state = self.shared().lock().unwrap();
        ready(&state)?;
        if state
            .reconnecting
            .as_ref()
            .is_some_and(|work| work.pending())
            || state.calls.len() >= state.configuration.as_ref().unwrap().binding.maximum_calls
            || state.calls.iter().any(|call| call.id == id)
        {
            return Err(SipHostError::OperationConflict);
        }
        let media = create()?;
        let call = Arc::new(Call {
            id,
            host: Arc::downgrade(self.shared()),
            state: Mutex::new(CallState {
                attempted: false,
                ending: false,
                failed_before_send: false,
                invite: None,
                preparing: None,
                releasing: None,
            }),
            media,
        });
        state.calls.push(call.clone());
        Ok(SipHostCall(call))
    }
    pub fn find(&self, id: &str) -> Option<SipHostCall> {
        self.shared()
            .lock()
            .unwrap()
            .calls
            .iter()
            .find(|call| call.id == id)
            .cloned()
            .map(SipHostCall)
    }
    /// Route selection is a separate pre-DNS authority; a proxy overrides it.
    /// The native DTO/descriptor owner constructs the original INVITE identity.
    pub async fn dial_delayed(
        &self,
        expected: &SipHostCall,
        request: Request,
        next_hop: SipAddr,
        credentials: Option<DigestCredentials>,
        ring_seconds: u32,
    ) -> Result<CallOutcome, SipHostError> {
        if !(1..=120).contains(&ring_seconds)
            || !request.body.is_empty()
            || InviteExchange::new(request.clone()).is_err()
        {
            return Err(SipHostError::InvalidRequest);
        }
        let completion = {
            let state = self.shared().lock().unwrap();
            ready(&state)?;
            original(&state, self.shared(), expected)?;
            let bound = state.binding.as_ref().unwrap().clone();
            let proxy = state.configuration.as_ref().unwrap().binding.proxy.clone();
            let mut call_state = expected.0.state.lock().unwrap();
            if call_state.attempted || call_state.ending {
                return Err(SipHostError::OperationConflict);
            }
            call_state.attempted = true;
            let preparing = Completion::new();
            call_state.preparing = Some(preparing.clone());
            let completion = Completion::new();
            let done = completion.clone();
            let call = expected.0.clone();
            tokio::spawn(async move {
                let route = proxy.as_ref().unwrap_or(&next_hop).clone();
                let prepared = match bound.connect_next_hop(route, proxy.is_some()).await {
                    Ok(flow) => flow
                        .prepare_invite(request)
                        .map_err(SipHostError::Signalling),
                    Err(error) => Err(SipHostError::Binding(error)),
                };
                let started = prepared.and_then(|prepared| {
                    let mut state = call.state.lock().unwrap();
                    if state.ending {
                        return Ok(None);
                    }
                    let invite = DelayedInvite::start(
                        bound.endpoint().map_err(SipHostError::Binding)?,
                        prepared,
                        credentials,
                        ring_seconds,
                        call.media.clone(),
                    )
                    .map_err(SipHostError::Signalling)?;
                    state.invite = Some(invite.clone());
                    Ok(Some(invite))
                });
                drop(bound);
                match started {
                    Ok(Some(invite)) => {
                        let observed = invite.clone();
                        let original = call.clone();
                        tokio::spawn(async move {
                            let mut changes = observed.observations();
                            loop {
                                let observation = *changes.borrow_and_update();
                                if observation.ending
                                    || observation.uncertain
                                    || observation.outcome.is_some() && !observation.active
                                {
                                    original.media.request_close();
                                }
                                if observation.closed {
                                    break;
                                }
                                if changes.changed().await.is_err() {
                                    original.media.request_close();
                                    break;
                                }
                            }
                        });
                        preparing.finish(Ok(()));
                        done.finish(Ok(invite.wait_answer().await));
                    }
                    Ok(None) => {
                        preparing.finish(Ok(()));
                        done.finish(Ok(CallOutcome::Ended));
                    }
                    Err(error) => {
                        call.state.lock().unwrap().failed_before_send = true;
                        call.media.request_close();
                        preparing.finish(Ok(()));
                        done.finish(Err(error));
                    }
                }
            });
            completion
        };
        completion.wait().await
    }
    pub fn hangup(&self, expected: &SipHostCall) -> Result<SipHostCallObservation, SipHostError> {
        let state = self.shared().lock().unwrap();
        original(&state, self.shared(), expected)?;
        expected.0.hangup();
        Ok(expected.observation())
    }
    pub async fn release(&self, expected: &SipHostCall) -> Result<(), SipHostError> {
        let completion = {
            let state = self.shared().lock().unwrap();
            original(&state, self.shared(), expected)?;
            begin_release(self.shared(), &expected.0)
        };
        completion.wait().await
    }
    pub async fn reconnect_registration(&self) -> Result<RegistrationObservation, SipHostError> {
        let completion = {
            let mut state = self.shared().lock().unwrap();
            ready(&state)?;
            if !state.calls.is_empty()
                || state
                    .reconnecting
                    .as_ref()
                    .is_some_and(|work| work.pending())
            {
                return Err(SipHostError::OperationConflict);
            }
            let config = state.configuration.as_ref().unwrap();
            let plan = config.registration.as_ref().ok_or(SipHostError::NotReady)?;
            let settings = plan.settings.clone();
            let password = plan.password.clone();
            let proxy = config.binding.proxy.clone();
            let bound = state.binding.as_ref().unwrap().clone();
            let previous = state.registration.take();
            if let Some(previous) = &previous {
                previous.request_stop();
            }
            let completion = Completion::new();
            state.reconnecting = Some(completion.clone());
            let done = completion.clone();
            let owner = self.shared().clone();
            tokio::spawn(async move {
                if let Some(mut previous) = previous {
                    if let Err(error) = previous.close().await {
                        owner.lock().unwrap().registration = Some(previous);
                        done.finish(Err(SipHostError::Registration(error)));
                        return;
                    }
                }
                let next = SipRegistration::start(bound, settings, password, proxy)
                    .map_err(SipHostError::Registration);
                let result = next.map(|registration| {
                    let observation = registration.observation();
                    let mut state = owner.lock().unwrap();
                    state.registration_view = Some(registration.observer());
                    state.registration = Some(registration);
                    observation
                });
                done.finish(result);
            });
            completion
        };
        completion.wait().await
    }
    /// The native incoming-request owner takes the original stream once and
    /// offers matching controls back here; it owns all other SIP requests.
    pub fn incoming_transactions(&self) -> Result<TransactionReceiver, SipHostError> {
        let state = self.shared().lock().unwrap();
        ready(&state)?;
        state
            .binding
            .as_ref()
            .unwrap()
            .endpoint()
            .map_err(SipHostError::Binding)?
            .incoming_transactions()
            .map_err(SipHostError::Signalling)
    }
    pub fn deliver_control(&self, mut transaction: Transaction) -> Result<(), Transaction> {
        let state = self.shared().lock().unwrap();
        for call in &state.calls {
            if let Some(invite) = &call.state.lock().unwrap().invite {
                transaction = match invite.deliver_control(transaction) {
                    Ok(()) => return Ok(()),
                    Err(transaction) => transaction,
                };
            }
        }
        Err(transaction)
    }
    pub async fn close(&self) -> Result<(), SipHostError> {
        begin_close(self.shared()).wait().await
    }
}
fn ready(state: &State) -> Result<(), SipHostError> {
    if state.stopping {
        Err(SipHostError::Stopping)
    } else if !state.configured
        || state.binding.is_none()
        || state
            .configuring
            .as_ref()
            .is_some_and(|work| work.pending())
    {
        Err(SipHostError::NotReady)
    } else {
        Ok(())
    }
}
fn original(state: &State, owner: &Shared, call: &SipHostCall) -> Result<(), SipHostError> {
    if !Weak::ptr_eq(&call.0.host, &Arc::downgrade(owner))
        || !state
            .calls
            .iter()
            .any(|original| Arc::ptr_eq(original, &call.0))
    {
        Err(SipHostError::NotReady)
    } else {
        Ok(())
    }
}
fn begin_release(owner: &Shared, call: &Arc<Call>) -> Arc<Completion<()>> {
    let mut state = call.state.lock().unwrap();
    if let Some(releasing) = &state.releasing {
        return releasing.clone();
    }
    state.ending = true;
    call.media.request_close();
    if let Some(invite) = &state.invite {
        invite.request_hangup();
    }
    let preparing = state.preparing.clone();
    let completion = Completion::new();
    state.releasing = Some(completion.clone());
    let done = completion.clone();
    let call = call.clone();
    let owner = owner.clone();
    tokio::spawn(async move {
        if let Some(preparing) = preparing {
            let _ = preparing.wait().await;
        }
        let invite = call.state.lock().unwrap().invite.clone();
        let signalling = match invite {
            Some(invite) => invite
                .close()
                .await
                .map_err(|_: CallError| SipHostError::CleanupUnconfirmed),
            None => Ok(()),
        };
        let media = call.media.close().await;
        let result = signalling.and(media);
        if result.is_ok() {
            owner
                .lock()
                .unwrap()
                .calls
                .retain(|original| !Arc::ptr_eq(original, &call));
        }
        done.finish(result);
    });
    completion
}
fn begin_close(owner: &Shared) -> Arc<Completion<()>> {
    let mut state = owner.lock().unwrap();
    if let Some(closing) = &state.closing {
        return closing.clone();
    }
    state.stopping = true;
    let configuring = state.configuring.clone();
    let reconnecting = state.reconnecting.clone();
    let completion = Completion::new();
    state.closing = Some(completion.clone());
    let done = completion.clone();
    let owner = owner.clone();
    tokio::spawn(async move {
        if let Some(configuring) = configuring {
            let _ = configuring.wait().await;
        }
        if let Some(reconnecting) = reconnecting {
            let _ = reconnecting.wait().await;
        }
        let releases = {
            let state = owner.lock().unwrap();
            state
                .calls
                .iter()
                .map(|call| begin_release(&owner, call))
                .collect::<Vec<_>>()
        };
        let mut result = Ok(());
        for release in releases {
            if let Err(error) = release.wait().await {
                result = Err(error);
            }
        }
        if result.is_err() {
            done.finish(result);
            return;
        }
        let registration = owner.lock().unwrap().registration.take();
        if let Some(mut registration) = registration {
            if let Err(error) = registration.close().await {
                owner.lock().unwrap().registration = Some(registration);
                done.finish(Err(SipHostError::Registration(error)));
                return;
            }
        }
        let binding = owner.lock().unwrap().binding.take();
        if let Some(binding) = binding {
            let mut binding = match Arc::try_unwrap(binding) {
                Ok(binding) => binding,
                Err(binding) => {
                    owner.lock().unwrap().binding = Some(binding);
                    done.finish(Err(SipHostError::CleanupUnconfirmed));
                    return;
                }
            };
            if let Err(error) = binding.close().await {
                owner.lock().unwrap().binding = Some(Arc::new(binding));
                done.finish(Err(SipHostError::Binding(error)));
                return;
            }
        }
        owner.lock().unwrap().closed = true;
        done.finish(Ok(()));
    });
    completion
}
