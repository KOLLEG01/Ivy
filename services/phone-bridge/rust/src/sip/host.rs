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
    InitializationFailed,
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
    pub(crate) fn settings(&self) -> &SipRegistrationSettings {
        &self.settings
    }
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
    signalling_view: OnceLock<super::SipSignalling>,
    host: Weak<Mutex<State>>,
    state: Mutex<CallState>,
    media: OnceLock<Arc<dyn HostedSipMedia>>,
    initializing: Option<Arc<Completion<()>>>,
}
impl Call {
    fn request_media_close(&self) {
        if let Some(media) = self.media.get() {
            media.request_close();
        }
    }
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
        self.request_media_close();
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
    pub fn signalling(&self) -> Option<super::SipSignallingObservation> {
        self.0
            .signalling_view
            .get()
            .map(super::SipSignalling::observation)
    }
    pub(crate) async fn constructed(&self) -> Result<(), SipHostError> {
        if let Some(initializing) = &self.0.initializing {
            initializing.wait().await?;
        }
        Ok(())
    }
    /// Observe the original startup. Dropping this waiter does not cancel it,
    /// return its slot or grant a later object authority over the same ID.
    pub async fn prepared(&self) -> Result<(), SipHostError> {
        if let Some(initializing) = &self.0.initializing {
            initializing.wait().await?;
        }
        let state = self.0.state.lock().unwrap();
        if state.ending || self.0.media.get().is_none() {
            Err(SipHostError::NotReady)
        } else {
            Ok(())
        }
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
    calls: crate::call_slots::CallSlots<Arc<Call>>,
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
            calls: Default::default(),
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
        self.configure_with_setup(configuration, || async { Ok(()) })
            .await
    }
    /// Native setup runs after publishing the original binding and before any
    /// registration traffic. It must own its work without blocking/re-entering
    /// admission synchronously. Failed setup retains the configured-once latch
    /// and bound socket until explicit close; cancelled callers cannot retry it.
    pub async fn configure_with_setup<F, S>(
        &self,
        configuration: SipHostConfiguration,
        setup: S,
    ) -> Result<SocketAddr, SipHostError>
    where
        F: Future<Output = Result<(), SipHostError>> + Send + 'static,
        S: FnOnce() -> F + Send + 'static,
    {
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
                let result = async {
                    let bound = bound.map_err(SipHostError::Binding)?;
                    let endpoint = bound.local_addr();
                    let bound = Arc::new(bound);
                    owner.lock().unwrap().binding = Some(bound.clone());
                    // Observe a panicking setup without losing the owned
                    // configure completion or poisoning host serialization.
                    tokio::spawn(async move { setup().await })
                        .await
                        .map_err(|_| SipHostError::CleanupUnconfirmed)??;
                    let mut state = owner.lock().unwrap();
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
                }
                .await;
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
        admission(&state, &id)?;
        let endpoint = state
            .binding
            .as_ref()
            .unwrap()
            .endpoint()
            .map_err(SipHostError::Binding)?;
        let media = create()?;
        let signalling_view = OnceLock::from(endpoint.observe_signalling(&id));
        let call = Arc::new(Call {
            id,
            signalling_view,
            host: Arc::downgrade(self.shared()),
            state: Mutex::new(CallState {
                attempted: false,
                ending: false,
                failed_before_send: false,
                invite: None,
                preparing: None,
                releasing: None,
            }),
            media: OnceLock::from(media),
            initializing: None,
        });
        state.calls.push(call.clone());
        Ok(SipHostCall(call))
    }
    /// Reserve and publish the original slot before invoking asynchronous
    /// RTP/audio construction. The factory runs once outside host ownership;
    /// its failure must confirm cleanup, or return CleanupUnconfirmed. A panic
    /// is uncertain and keeps the slot, including through release/host close.
    pub fn prepare_async<F, S>(&self, id: String, create: S) -> Result<SipHostCall, SipHostError>
    where
        F: Future<Output = Result<Arc<dyn HostedSipMedia>, SipHostError>> + Send + 'static,
        S: FnOnce() -> F + Send + 'static,
    {
        if !super::super::native_rpc::guid_d(&id) {
            return Err(SipHostError::InvalidRequest);
        }
        let runtime = tokio::runtime::Handle::try_current().map_err(|_| SipHostError::NotReady)?;
        let initializing = Completion::new();
        let (call, observers) = {
            let mut state = self.shared().lock().unwrap();
            admission(&state, &id)?;
            let observers = state
                .binding
                .as_ref()
                .unwrap()
                .endpoint()
                .map_err(SipHostError::Binding)?
                .signalling_observers();
            let call = Arc::new(Call {
                id,
                signalling_view: OnceLock::new(),
                host: Arc::downgrade(self.shared()),
                state: Mutex::new(CallState {
                    attempted: false,
                    ending: false,
                    failed_before_send: false,
                    invite: None,
                    preparing: None,
                    releasing: None,
                }),
                media: OnceLock::new(),
                initializing: Some(initializing.clone()),
            });
            state.calls.push(call.clone());
            (call, observers)
        };
        let original = call.clone();
        runtime.spawn(async move {
            let result = tokio::spawn(async move { create().await })
                .await
                .unwrap_or(Err(SipHostError::CleanupUnconfirmed));
            let mut state = original.state.lock().unwrap();
            let result = match result {
                Ok(media) => {
                    let _ = original
                        .signalling_view
                        .set(observers.observe(&original.id));
                    // Hangup/release may have arrived while the original
                    // constructor was suspended. Revoke before publication.
                    if state.ending {
                        media.request_close();
                    }
                    let _ = original.media.set(media);
                    Ok(())
                }
                Err(error) => {
                    state.failed_before_send = true;
                    Err(error)
                }
            };
            initializing.finish(result);
        });
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
        if !request.body.is_empty() {
            return Err(SipHostError::InvalidRequest);
        }
        self.begin_dial(expected, request, next_hop, credentials, ring_seconds)?
            .await
    }
    /// Reserve the original attempt before returning a cancellable observer.
    /// Initial and delayed offers share this transaction/dialog owner.
    pub(crate) fn begin_dial(
        &self,
        expected: &SipHostCall,
        request: Request,
        next_hop: SipAddr,
        credentials: Option<DigestCredentials>,
        ring_seconds: u32,
    ) -> Result<impl Future<Output = Result<CallOutcome, SipHostError>> + Send + use<>, SipHostError>
    {
        if !(1..=120).contains(&ring_seconds) || InviteExchange::new(request.clone()).is_err() {
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
            let media = expected
                .0
                .media
                .get()
                .ok_or(SipHostError::NotReady)?
                .clone();
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
                        media,
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
                                    original.request_media_close();
                                }
                                if observation.closed {
                                    break;
                                }
                                if changes.changed().await.is_err() {
                                    original.request_media_close();
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
                        call.request_media_close();
                        preparing.finish(Ok(()));
                        done.finish(Err(error));
                    }
                }
            });
            completion
        };
        Ok(async move { completion.wait().await })
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
        self.begin_registration_reconnect()?.await
    }
    /// Native receipt admission reserves the original transition before its
    /// observer future is returned. Protocol work remains independently owned.
    pub(crate) fn begin_registration_reconnect(
        &self,
    ) -> Result<
        impl Future<Output = Result<RegistrationObservation, SipHostError>> + Send + use<>,
        SipHostError,
    > {
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
                        drop(bound);
                        done.finish(Err(SipHostError::Registration(error)));
                        return;
                    }
                }
                let result = {
                    let mut state = owner.lock().unwrap();
                    if state.stopping {
                        Err(SipHostError::Stopping)
                    } else {
                        SipRegistration::start(bound.clone(), settings, password, proxy)
                            .map_err(SipHostError::Registration)
                            .map(|registration| {
                                let observation = registration.observation();
                                state.registration_view = Some(registration.observer());
                                state.registration = Some(registration);
                                observation
                            })
                    }
                };
                // Publish completion after releasing this worker's binding
                // reference, so the waiting close can confirm the same fence.
                drop(bound);
                done.finish(result);
            });
            completion
        };
        Ok(async move { completion.wait().await })
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
        for call in state.calls.iter() {
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
fn admission(state: &State, id: &str) -> Result<(), SipHostError> {
    ready(state)?;
    if state
        .reconnecting
        .as_ref()
        .is_some_and(|work| work.pending())
        || state.calls.len() >= state.configuration.as_ref().unwrap().binding.maximum_calls
        || state.calls.iter().any(|call| call.id == id)
    {
        Err(SipHostError::OperationConflict)
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
    call.request_media_close();
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
        let original = call.clone();
        let result = tokio::spawn(async move {
            if let Some(initializing) = &original.initializing {
                if initializing.wait().await == Err(SipHostError::CleanupUnconfirmed) {
                    return Err(SipHostError::CleanupUnconfirmed);
                }
            }
            if let Some(view) = original.signalling_view.get() {
                view.close();
            }
            if let Some(preparing) = preparing {
                let _ = preparing.wait().await;
            }
            let invite = original.state.lock().unwrap().invite.clone();
            let signalling = match invite {
                Some(invite) => invite
                    .close()
                    .await
                    .map_err(|_: CallError| SipHostError::CleanupUnconfirmed),
                None => Ok(()),
            };
            let media = match original.media.get() {
                Some(media) => media.close().await,
                None => Ok(()),
            };
            signalling.and(media)
        })
        .await
        .unwrap_or(Err(SipHostError::CleanupUnconfirmed));
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
