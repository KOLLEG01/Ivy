//! Native SIP configuration and the original call's retained media admission.
use crate::{
    call_features::{
        CallAccessSettings, CallFeatureObservation, CallFeatureTime, FeatureError,
        PhoneCallFeatures, SystemCallFeatureTime,
    },
    dtmf_access::FailureLimiter,
    media::{SipMediaIdentity, SipMediaSession, SipMediaSettings},
    native_config::{
        NativeCodecSettings, NativeConfigError, NativeIncomingPeers, NativeMicroSettings,
        NativeSipBinding, NativeSipConfiguration,
    },
    native_dial::NativeDial,
    native_media_settings::{NativeAudioSettings, NativeScreeningSettings},
    native_rpc::NativeRequest,
    sip::{
        CallOutcome, HostedSipMedia, RegistrationObservation, SipHost, SipHostCall,
        SipHostCallObservation, SipHostCallPhase, SipHostError, SipHostObservation,
        SipRegistrationSettings,
    },
};
use std::{
    future::Future,
    net::{IpAddr, SocketAddr},
    pin::Pin,
    sync::{Arc, Mutex, OnceLock},
};
use tokio::sync::watch;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeSipError {
    OfferExpired,
    Features(FeatureError),
    Configuration(NativeConfigError),
    Host(SipHostError),
}
impl NativeSipError {
    pub fn code(self) -> &'static str {
        match self {
            Self::OfferExpired => "phone_offer_expired",
            Self::Features(error) => error.code(),
            Self::Configuration(error) => error.code(),
            Self::Host(SipHostError::InvalidRequest) => "invalid_request",
            Self::Host(SipHostError::OperationConflict) => "operation_conflict",
            Self::Host(SipHostError::NotReady) => "runtime_not_ready",
            Self::Host(SipHostError::Stopping) => "runtime_stopping",
            Self::Host(_) => "operation_outcome_unknown",
        }
    }
}
impl From<FeatureError> for NativeSipError {
    fn from(error: FeatureError) -> Self {
        Self::Features(error)
    }
}
impl From<NativeConfigError> for NativeSipError {
    fn from(error: NativeConfigError) -> Self {
        Self::Configuration(error)
    }
}
impl From<SipHostError> for NativeSipError {
    fn from(error: SipHostError) -> Self {
        Self::Host(error)
    }
}

/// A waiter observes retained native work. Dropping every waiter cannot stop
/// configuration/construction/disposal or permit another factory invocation.
#[derive(Clone)]
pub struct NativeSipOperation<T: Clone>(watch::Receiver<Option<Result<T, SipHostError>>>);
impl<T: Clone> NativeSipOperation<T> {
    pub fn result(&self) -> Option<Result<T, SipHostError>> {
        self.0.borrow().clone()
    }
    pub async fn wait(mut self) -> Result<T, SipHostError> {
        loop {
            if let Some(result) = self.result() {
                return result;
            }
            if self.0.changed().await.is_err() {
                return Err(SipHostError::CleanupUnconfirmed);
            }
        }
    }
}
pub(crate) fn retained<T, F>(work: F) -> NativeSipOperation<T>
where
    T: Clone + Send + Sync + 'static,
    F: Future<Output = Result<T, SipHostError>> + Send + 'static,
{
    let (result, waiter) = watch::channel(None);
    tokio::spawn(async move {
        let outcome = tokio::spawn(work)
            .await
            .unwrap_or(Err(SipHostError::CleanupUnconfirmed));
        result.send_replace(Some(outcome));
    });
    NativeSipOperation(waiter)
}

/// Validated settings remain available to the original media/call owner after
/// the host consumes registration credentials. No protected configuration is
/// serialized into status or command receipts.
pub struct NativeCallConfiguration {
    binding: NativeSipBinding,
    codecs: NativeCodecSettings,
    incoming: NativeIncomingPeers,
    registration: Option<SipRegistrationSettings>,
}
impl NativeCallConfiguration {
    pub fn binding(&self) -> &NativeSipBinding {
        &self.binding
    }
    pub fn codecs(&self) -> &NativeCodecSettings {
        &self.codecs
    }
    pub fn incoming_peers(&self) -> &NativeIncomingPeers {
        &self.incoming
    }
    pub fn has_registration(&self) -> bool {
        self.registration.is_some()
    }
    /// The call owner supplies its original entropy and resolved SDP address.
    /// Construct these only inside the admitted factory, before RTP startup.
    pub fn media_settings(
        &self,
        id: String,
        identity: SipMediaIdentity,
        connection_address: IpAddr,
    ) -> SipMediaSettings {
        let (first_port, last_port) = self.binding.rtp_port_range();
        let mut interface = self.binding.endpoint();
        interface.set_port(0);
        SipMediaSettings {
            call_id: id,
            interface,
            connection_address,
            first_port,
            last_port,
            reorder_ms: self.codecs.receive_reorder_ms().into(),
            preferences: self.codecs.preferences().to_vec(),
            identity,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn media_binding_preserves_the_validated_ipv6_scope() {
        let frame = br#"{"version":1,"epoch":"11111111-1111-4111-8111-111111111111","requestId":1,"operationId":"11111111-1111-4111-8111-111111111111","method":"configure","params":{"binding":{"address":"fe80::1%12","port":5060,"transport":"udp"},"codecs":{"preferences":["PCMA"]},"incomingPeers":[],"registration":null,"password":null}}"#;
        let request = NativeRequest::parse(frame).unwrap();
        let parts = NativeSipConfiguration::parse(&request)
            .unwrap()
            .into_parts()
            .unwrap();
        let configuration = NativeCallConfiguration {
            binding: parts.binding,
            codecs: parts.codecs,
            incoming: parts.incoming,
            registration: None,
        };
        let mut expected = configuration.binding().endpoint();
        expected.set_port(0);
        let settings = configuration.media_settings(
            "11111111-1111-4111-8111-111111111111".into(),
            SipMediaIdentity {
                session_id: 1,
                source: 17,
                sequence: 100,
                timestamp: 200,
                cname: "fixture".into(),
            },
            "fe80::1".parse().unwrap(),
        );
        assert_eq!(settings.interface, expected);
        assert_eq!(settings.interface, "[fe80::1%12]:0".parse().unwrap());
    }
}
pub type NativeSipMediaFuture =
    Pin<Box<dyn Future<Output = Result<SipMediaSession, SipHostError>> + Send>>;
/// This constructor runs once after the original host slot is reserved. On
/// failure it must return CleanupUnconfirmed unless all created audio/media
/// resources are confirmed drained. A panic is also uncertain.
pub trait NativeSipMediaFactory: Send + Sync + 'static {
    fn create(
        &self,
        id: String,
        configuration: Arc<NativeCallConfiguration>,
    ) -> NativeSipMediaFuture;
}
struct Call {
    original: SipHostCall,
    media: Arc<OnceLock<SipMediaSession>>,
    features: PhoneCallFeatures,
    incoming: Option<crate::native_incoming::NativeIncoming>,
    result: Mutex<CallResult>,
    releasing: Mutex<Option<NativeSipOperation<()>>>,
}
#[derive(Default)]
struct CallResult {
    attempted: bool,
    ending: bool,
    unknown: bool,
    error: Option<&'static str>,
    claimed: bool,
    retiring: bool,
    feedback_sequences: std::collections::HashSet<i64>,
    screening_timeout: Option<u16>,
    screening_deadline: Option<tokio_util::sync::CancellationToken>,
    windows_connected: bool,
}
#[derive(Clone)]
pub struct NativeSipCall(Arc<Call>);
impl NativeSipCall {
    fn media_failed(&self) {
        {
            let mut result = self.0.result.lock().unwrap();
            if !result.unknown {
                result.error = Some("media_failed");
            }
            result.ending = true;
        }
        // A saved callback belongs to this handle even after its ID is reused.
        self.0.original.request_hangup();
    }
    pub(crate) fn incoming(&self) -> Option<&crate::native_incoming::NativeIncoming> {
        self.0.incoming.as_ref()
    }
    pub(crate) fn sip_call_id(&self) -> &str {
        self.incoming()
            .map(|i| i.sip_call_id.as_str())
            .unwrap_or(self.0.original.id())
    }
    pub fn signalling(&self) -> Option<crate::sip::SipSignallingObservation> {
        self.0.original.signalling()
    }
    pub(crate) async fn constructed(&self) -> Result<(), SipHostError> {
        self.0.original.constructed().await
    }
    pub fn features(&self) -> PhoneCallFeatures {
        self.0.features.clone()
    }
    pub fn observation(&self) -> SipHostCallObservation {
        let mut observed = self.0.original.observation();
        let remote_ended = self.0.original.remote_ended();
        let mut result = self.0.result.lock().unwrap();
        if observed.phase == SipHostCallPhase::OutcomeUnknown {
            result.unknown = true;
            result.ending = true;
            result.error = Some("sip_operation_outcome_unknown");
        }
        result.ending |= remote_ended;
        if result.unknown {
            observed.phase = SipHostCallPhase::OutcomeUnknown;
        }
        observed.ending |= result.ending;
        observed.attempted |= result.attempted;
        observed
    }
    pub(crate) fn error(&self) -> Option<&'static str> {
        self.observation();
        self.0.result.lock().unwrap().error
    }
    pub async fn prepared(&self) -> Result<(), SipHostError> {
        self.0.original.prepared().await
    }
    pub fn media(&self) -> Option<SipMediaSession> {
        self.0.media.get().cloned()
    }
}
struct State {
    configured: bool,
    stopping: bool,
    configuration: Option<Arc<NativeCallConfiguration>>,
    configuring: Option<NativeSipOperation<SocketAddr>>,
    calls: crate::call_slots::CallSlots<NativeSipCall>,
    closing: Option<NativeSipOperation<()>>,
}
struct Owner {
    state: Mutex<State>,
    host: SipHost,
    media: Arc<dyn NativeSipMediaFactory>,
    limiter: Arc<Mutex<FailureLimiter>>,
    feature_time: Arc<dyn CallFeatureTime>,
}
#[derive(Clone)]
pub struct NativeSipOperations(Arc<Owner>);
pub(crate) struct NativeSipStatusSnapshot {
    pub configured: bool,
    pub call: Option<NativeSipCall>,
    pub registration: Option<RegistrationObservation>,
    pub call_ids: Vec<String>,
    pub endpoint: Option<SocketAddr>,
}
impl NativeSipOperations {
    pub(crate) fn status_snapshot(
        &self,
        id: Option<&str>,
    ) -> Result<NativeSipStatusSnapshot, SipHostError> {
        let s = self.0.state.lock().unwrap();
        if s.stopping {
            return Err(SipHostError::Stopping);
        }
        let host = self.0.host.observation();
        Ok(NativeSipStatusSnapshot {
            configured: s.configured,
            call: match id {
                Some(id) => s.calls.iter().find(|c| c.observation().id == id).cloned(),
                None => s.calls.first().cloned(),
            },
            registration: host.registration,
            call_ids: s.calls.iter().map(|c| c.observation().id).collect(),
            endpoint: host.endpoint,
        })
    }
    pub fn new(media: Arc<dyn NativeSipMediaFactory>) -> Self {
        Self::with_feature_time(media, Arc::new(SystemCallFeatureTime::default()))
    }
    pub fn with_feature_time(
        media: Arc<dyn NativeSipMediaFactory>,
        time: Arc<dyn CallFeatureTime>,
    ) -> Self {
        Self(Arc::new(Owner {
            state: Mutex::new(State {
                configured: false,
                stopping: false,
                configuration: None,
                configuring: None,
                calls: Default::default(),
                closing: None,
            }),
            host: SipHost::new(),
            media,
            limiter: Arc::new(Mutex::new(FailureLimiter::default())),
            feature_time: time,
        }))
    }
    /// Exact DTO validation precedes the one-way latch. Micro setup is required
    /// explicitly, occurs after binding and before registration, and is never
    /// silently discarded by this owner.
    pub fn configure<F, S>(
        &self,
        request: &NativeRequest,
        setup: S,
    ) -> Result<NativeSipOperation<SocketAddr>, NativeSipError>
    where
        F: Future<Output = Result<(), SipHostError>> + Send + 'static,
        S: FnOnce(Option<NativeMicroSettings>) -> F + Send + 'static,
    {
        let mut state = self.0.state.lock().unwrap();
        if state.stopping {
            return Err(SipHostError::Stopping.into());
        }
        if state.configured {
            return Err(SipHostError::OperationConflict.into());
        }
        let parsed = NativeSipConfiguration::parse(request)?;
        tokio::runtime::Handle::try_current().map_err(|_| SipHostError::NotReady)?;
        state.configured = true;
        let registration = parsed.registration_settings();
        let parts = parsed.into_parts()?;
        state.configuration = Some(Arc::new(NativeCallConfiguration {
            binding: parts.binding,
            codecs: parts.codecs,
            incoming: parts.incoming,
            registration,
        }));
        let host = self.0.host.clone();
        let incoming_owner = Arc::downgrade(&self.0);
        let operation = retained(async move {
            let endpoint = host
                .configure_with_setup(parts.host, move || setup(parts.micro))
                .await?;
            host.start_requests(Arc::new(move |transaction, source| {
                let owner = incoming_owner.clone();
                Box::pin(async move { Self::receive_incoming(owner, transaction, source).await })
            }))?;
            Ok(endpoint)
        });
        state.configuring = Some(operation.clone());
        Ok(operation)
    }
    pub fn configured(&self) -> bool {
        self.0.state.lock().unwrap().configured
    }
    pub fn configuration_result(&self) -> Option<Result<SocketAddr, SipHostError>> {
        self.0
            .state
            .lock()
            .unwrap()
            .configuring
            .as_ref()
            .and_then(NativeSipOperation::result)
    }
    pub fn observation(&self) -> SipHostObservation {
        let state = self.0.state.lock().unwrap();
        let mut observation = self.0.host.observation();
        observation.configured = state.configured;
        observation.stopping |= state.stopping;
        observation.calls = state.calls.iter().map(NativeSipCall::observation).collect();
        observation
    }
    pub fn configuration(&self) -> Option<Arc<NativeCallConfiguration>> {
        self.0.state.lock().unwrap().configuration.clone()
    }
    pub fn prepare(&self, id: String) -> Result<NativeSipCall, SipHostError> {
        self.prepare_call(id, None)
    }
    fn prepare_call(
        &self,
        id: String,
        incoming: Option<crate::native_incoming::NativeIncoming>,
    ) -> Result<NativeSipCall, SipHostError> {
        let mut state = self.0.state.lock().unwrap();
        if state.stopping {
            return Err(SipHostError::Stopping);
        }
        // A retained socket alone is insufficient: setup/registration creation
        // may have failed after bind. Only a successful saved configuration
        // grants authority to construct any call media.
        if state
            .configuring
            .as_ref()
            .and_then(NativeSipOperation::result)
            .is_none_or(|r| r.is_err())
        {
            return Err(SipHostError::NotReady);
        }
        let configuration = state.configuration.clone().ok_or(SipHostError::NotReady)?;
        // Host disposal completes before its native completion is published.
        // Preserve this owner's ID until the same retained work retires it.
        if state.calls.len() >= usize::from(configuration.binding().max_concurrent_calls())
            || state.calls.iter().any(|call| call.observation().id == id)
            || incoming
                .as_ref()
                .is_some_and(|i| state.calls.iter().any(|c| c.sip_call_id() == i.sip_call_id))
        {
            return Err(SipHostError::OperationConflict);
        }
        let factory = self.0.media.clone();
        let media = Arc::new(OnceLock::new());
        let saved = media.clone();
        let original_id = id.clone();
        let owner = Arc::downgrade(&self.0);
        let original_slot = Arc::new(OnceLock::<std::sync::Weak<Call>>::new());
        let hangup_slot = original_slot.clone();
        let failure_slot = original_slot.clone();
        let features = PhoneCallFeatures::new(
            self.0.limiter.clone(),
            self.0.feature_time.clone(),
            move || {
                if let (Some(owner), Some(call)) = (
                    owner.upgrade(),
                    hangup_slot.get().and_then(std::sync::Weak::upgrade),
                ) {
                    let _ = NativeSipOperations(owner).hangup(&NativeSipCall(call));
                }
            },
        )
        .map_err(|_| SipHostError::NotReady)?;
        let media_features = features.clone();
        let create = move || async move {
            let session = factory.create(original_id, configuration).await?;
            // Publish the exact session even if policy attachment is uncertain;
            // it must remain fenced for explicit original-call cleanup.
            let _ = saved.set(session.clone());
            session.on_failure(move || {
                if let Some(call) = failure_slot.get().and_then(std::sync::Weak::upgrade) {
                    NativeSipCall(call).media_failed();
                }
            });
            let needed = media_features.clone();
            if session
                .enable_dtmf(
                    move |digit, source| {
                        media_features
                            .digit(digit, source)
                            .map_err(|_| crate::media::SipMediaError::InvalidRequest)
                    },
                    move || needed.needs_inband_dtmf(),
                )
                .is_err()
            {
                session.request_close();
                return Err(SipHostError::CleanupUnconfirmed);
            }
            Ok(Arc::new(session) as Arc<dyn HostedSipMedia>)
        };
        let original = match &incoming {
            Some(context) => {
                self.0
                    .host
                    .prepare_incoming_async(id, context.sip_call_id.clone(), create)
            }
            None => self.0.host.prepare_async(id, create),
        }?;
        let call = NativeSipCall(Arc::new(Call {
            original,
            media,
            features,
            incoming,
            result: Mutex::new(CallResult::default()),
            releasing: Mutex::new(None),
        }));
        let _ = original_slot.set(Arc::downgrade(&call.0));
        // Construction can finish before its native weak handle is published.
        if call
            .media()
            .is_some_and(|m| crate::sip::DelayedInviteMedia::failed(&m))
        {
            call.media_failed();
        }
        state.calls.push(call.clone());
        Ok(call)
    }
    async fn receive_incoming(
        owner: std::sync::Weak<Owner>,
        mut transaction: rsipstack::transaction::Transaction,
        source: Option<rsipstack::transport::SipAddr>,
    ) -> Result<(), rsipstack::transaction::Transaction> {
        use rsipstack::sip::{StatusCode, prelude::HeadersExt};
        let Some(owner) = owner.upgrade().map(Self) else {
            return Err(transaction);
        };
        transaction.destination = source.clone();
        let configuring = owner.0.state.lock().unwrap().configuring.clone();
        let configured = match configuring {
            Some(work) => work.wait().await.is_ok(),
            None => false,
        };
        let wire = transaction
            .original
            .call_id_header()
            .ok()
            .map(|v| v.value().to_owned());
        let mut context =
            crate::native_incoming::NativeIncoming::parse(&transaction.original, source.as_ref());
        let (duplicate, authorized, stopping, ring_seconds) = {
            let state = owner.0.state.lock().unwrap();
            let duplicate = wire
                .as_ref()
                .is_some_and(|id| state.calls.iter().any(|c| c.sip_call_id() == id));
            let config = state.configuration.as_ref();
            let authorized = configured
                && context.as_mut().is_some_and(|incoming| {
                    incoming.registered = owner.0.host.matches_registered_peer(incoming.peer);
                    config.is_some_and(|c| {
                        c.incoming
                            .addresses()
                            .iter()
                            .any(|address| crate::sip::same_ip(*address, incoming.peer.address))
                            || c.incoming.uses_registered_peer() && incoming.registered
                    })
                });
            (
                duplicate,
                authorized,
                state.stopping
                    || config.is_some_and(|c| {
                        state.calls.len() >= usize::from(c.binding.max_concurrent_calls())
                    }),
                config
                    .map(|c| c.binding.incoming_ring_seconds())
                    .unwrap_or(30),
            )
        };
        if duplicate {
            return Ok(());
        }
        let mut status = if authorized {
            StatusCode::BusyHere
        } else {
            StatusCode::Forbidden
        };
        if authorized && !stopping {
            if let Some(id) = crate::native_incoming::call_id() {
                match owner.prepare_call(id, context.take()) {
                    Ok(call) => match owner.0.host.adopt_incoming(
                        &call.0.original,
                        transaction,
                        ring_seconds,
                    ) {
                        Ok(()) => {
                            owner.observe_incoming(call);
                            return Ok(());
                        }
                        Err(original) => {
                            transaction = original;
                            let _ = owner.release(&call);
                            status = StatusCode::Forbidden;
                        }
                    },
                    Err(SipHostError::OperationConflict) => {
                        if wire.as_ref().is_some_and(|id| {
                            owner
                                .0
                                .state
                                .lock()
                                .unwrap()
                                .calls
                                .iter()
                                .any(|c| c.sip_call_id() == id)
                        }) {
                            return Ok(());
                        }
                    }
                    Err(_) => status = StatusCode::Forbidden,
                }
            } else {
                status = StatusCode::Forbidden;
            }
        }
        if transaction.reply(status).await.is_ok() {
            Ok(())
        } else {
            Err(transaction)
        }
    }
    fn observe_incoming(&self, call: NativeSipCall) {
        let owner = Arc::downgrade(&self.0);
        let mut changes = call
            .0
            .original
            .incoming_observations()
            .expect("adopted original");
        tokio::spawn(async move {
            loop {
                let observed = *changes.borrow_and_update();
                if observed.ending
                    || observed.uncertain
                    || observed.outcome.is_some() && !observed.active
                {
                    if let Some(media) = call.media() {
                        media.request_close();
                    }
                    if let Some(owner) = owner.upgrade().map(Self) {
                        owner.retire_offer(&call);
                    }
                    break;
                }
                if changes.changed().await.is_err() {
                    if let Some(owner) = owner.upgrade().map(Self) {
                        let _ = owner.hangup(&call);
                        owner.retire_offer(&call);
                    }
                    break;
                }
            }
        });
    }
    fn retire_offer(&self, call: &NativeSipCall) {
        {
            let state = self.0.state.lock().unwrap();
            if Self::original(&state, call).is_err() || !call.observation().ending {
                return;
            }
            let mut result = call.0.result.lock().unwrap();
            if result.claimed || result.attempted || result.retiring {
                return;
            }
            result.retiring = true;
        }
        let _ = self.release(call);
    }
    pub(crate) fn claim(&self, call: &NativeSipCall) -> Result<(), NativeSipError> {
        let state = self.0.state.lock().unwrap();
        if Self::original(&state, call).is_err()
            || call.incoming().is_none()
            || call.observation().ending
        {
            return Err(NativeSipError::OfferExpired);
        }
        let mut result = call.0.result.lock().unwrap();
        if result.retiring || call.0.releasing.lock().unwrap().is_some() {
            return Err(NativeSipError::OfferExpired);
        }
        result.claimed = true;
        Ok(())
    }
    pub fn find(&self, id: &str) -> Option<NativeSipCall> {
        self.0
            .state
            .lock()
            .unwrap()
            .calls
            .iter()
            .find(|c| c.observation().id == id)
            .cloned()
    }
    pub fn reconnect_registration(
        &self,
    ) -> Result<NativeSipOperation<RegistrationObservation>, SipHostError> {
        let state = self.0.state.lock().unwrap();
        if state.stopping {
            return Err(SipHostError::Stopping);
        }
        if state
            .configuring
            .as_ref()
            .and_then(NativeSipOperation::result)
            .is_none_or(|r| r.is_err())
        {
            return Err(SipHostError::NotReady);
        }
        if !state
            .configuration
            .as_ref()
            .is_some_and(|c| c.has_registration())
        {
            return Err(SipHostError::NotReady);
        }
        // Native metadata retires only after its original release completes.
        // A host slot drained before that publication is still not idle here.
        if !state.calls.is_empty() {
            return Err(SipHostError::OperationConflict);
        }
        tokio::runtime::Handle::try_current().map_err(|_| SipHostError::NotReady)?;
        let work = self.0.host.begin_registration_reconnect()?;
        Ok(retained(work))
    }
    fn original(state: &State, expected: &NativeSipCall) -> Result<(), SipHostError> {
        if state.calls.iter().any(|c| Arc::ptr_eq(&c.0, &expected.0)) {
            Ok(())
        } else {
            Err(SipHostError::NotReady)
        }
    }
    pub fn hangup(&self, expected: &NativeSipCall) -> Result<SipHostCallObservation, SipHostError> {
        let state = self.0.state.lock().unwrap();
        Self::original(&state, expected)?;
        expected.0.result.lock().unwrap().ending = true;
        self.0.host.hangup(&expected.0.original)?;
        Ok(expected.observation())
    }
    pub(crate) fn prepare_screening(
        &self,
        expected: &NativeSipCall,
        settings: Option<NativeScreeningSettings>,
    ) -> Result<String, NativeSipError> {
        use crate::media::{ScreeningPreparationError, WavError};
        let state = self.0.state.lock().unwrap();
        Self::original(&state, expected)?;
        let observed = expected.observation();
        let result = expected.0.result.lock().unwrap();
        if observed.attempted
            || observed.ending
            || result.ending
            || expected.incoming().is_some()
            || result.screening_timeout.is_some()
        {
            return Err(NativeConfigError::OutcomeUnknown.into());
        }
        drop(result);
        let settings = settings.ok_or(NativeConfigError::InvalidRequest)?;
        let (timeout, repeats) = settings.bounds()?;
        let features = expected.features();
        expected
            .media()
            .ok_or(SipHostError::NotReady)?
            .prepare_screening_policy(
                &settings.path,
                &settings.hash,
                timeout,
                repeats,
                move || features.observation().screening == "waiting",
            )
            .map_err(|error| match error {
                ScreeningPreparationError::Announcement(WavError::Io)
                | ScreeningPreparationError::Media(_) => {
                    NativeSipError::from(NativeConfigError::OutcomeUnknown)
                }
                ScreeningPreparationError::Announcement(_) => {
                    NativeConfigError::InvalidRequest.into()
                }
            })?;
        expected.0.result.lock().unwrap().screening_timeout = Some(timeout);
        Ok("{\"prepared\":true}".into())
    }
    fn connected(&self, call: &NativeSipCall, result: &mut CallResult) {
        call.0.features.connected();
        let Some(seconds) = result.screening_timeout else {
            return;
        };
        if call.0.features.start_screening().is_err() {
            result.unknown = true;
            result.ending = true;
            result.error = Some("sip_operation_outcome_unknown");
            return;
        }
        let deadline = self
            .0
            .feature_time
            .delay(std::time::Duration::from_secs(u64::from(seconds)));
        let stop = tokio_util::sync::CancellationToken::new();
        result.screening_deadline = Some(stop.clone());
        let original = Arc::downgrade(&call.0);
        tokio::spawn(async move {
            tokio::select! { biased;
                _ = stop.cancelled() => return,
                _ = deadline => {},
            }
            if let Some(call) = original.upgrade() {
                call.features.screening_timeout();
            }
        });
    }
    pub(crate) fn connect_audio(
        &self,
        expected: &NativeSipCall,
        settings: Option<NativeAudioSettings>,
        bridge: bool,
    ) -> Result<NativeSipOperation<Result<String, NativeSipError>>, NativeSipError> {
        let state = self.0.state.lock().unwrap();
        Self::original(&state, expected)?;
        let observed = expected.observation();
        let result = expected.0.result.lock().unwrap();
        if bridge && expected.features().observation().screening != "accepted"
            || observed.ending
            || result.ending
            || observed.phase != SipHostCallPhase::Connected
            || !expected.features().authenticated()
            || result.windows_connected
        {
            return Err(NativeConfigError::OutcomeUnknown.into());
        }
        drop(result);
        drop(state);
        let media = expected.media().ok_or(SipHostError::NotReady)?;
        // Media lifetime is checked before settings.Validate in the original.
        if crate::sip::DelayedInviteMedia::is_closed(&media)
            || crate::sip::DelayedInviteMedia::failed(&media)
        {
            return Err(SipHostError::NotReady.into());
        }
        let settings = settings
            .ok_or(NativeConfigError::InvalidRequest)?
            .validated()?;
        let capture = settings.capture_endpoint.clone();
        let preparing = media
            .prepare_audio(settings, None, tokio_util::sync::CancellationToken::new())
            .map_err(native_audio_error)?;
        let call = expected.clone();
        Ok(retained(async move {
            let outcome = async {
                preparing.wait().await.map_err(|error| {
                    native_audio_error(crate::media::SipMediaError::Audio(error))
                })?;
                call.observation();
                let mut result = call.0.result.lock().unwrap();
                if result.ending {
                    return Err(NativeConfigError::OutcomeUnknown.into());
                }
                let port = media.call_audio();
                port.bind_authority(&capture).map_err(|error| {
                    native_audio_error(crate::media::SipMediaError::Audio(error))
                })?;
                result.windows_connected = true;
                port.grant_authority().map_err(|error| {
                    native_audio_error(crate::media::SipMediaError::Audio(error))
                })?;
                let json = crate::media::native_view::audio_json(&port.status())
                    .map_err(|_| NativeConfigError::OutcomeUnknown)?;
                if bridge {
                    call.features().bridge_screening()?;
                }
                let deadline = result.screening_deadline.clone();
                drop(result);
                if bridge {
                    media.bridge_screening();
                    if let Some(deadline) = deadline {
                        deadline.cancel();
                    }
                }
                Ok(json)
            }
            .await;
            Ok(outcome)
        }))
    }
    pub(crate) fn end_waiting(&self, expected: &NativeSipCall) -> Result<String, NativeSipError> {
        self.connected_feedback(expected, None)?;
        Ok(format!(
            "{{\"callId\":\"{}\",\"ended\":true}}",
            expected.0.original.id()
        ))
    }
    pub(crate) fn command_feedback(
        &self,
        expected: &NativeSipCall,
        sequence: i64,
        success: bool,
    ) -> Result<String, NativeSipError> {
        self.connected_feedback(expected, Some((sequence, success)))?;
        Ok(format!(
            "{{\"callId\":\"{}\",\"commandSequence\":{sequence},\"success\":{success}}}",
            expected.0.original.id()
        ))
    }
    fn connected_feedback(
        &self,
        expected: &NativeSipCall,
        feedback: Option<(i64, bool)>,
    ) -> Result<(), NativeSipError> {
        let state = self.0.state.lock().unwrap();
        Self::original(&state, expected)?;
        let observed = expected.observation();
        let mut result = expected.0.result.lock().unwrap();
        if observed.phase != SipHostCallPhase::Connected
            || observed.ending
            || result.ending
            || result.unknown
        {
            return Err(SipHostError::NotReady.into());
        }
        let media = expected.media().ok_or(SipHostError::NotReady)?;
        let action = match feedback {
            None => media.end_waiting(),
            Some((sequence, success)) => {
                if !expected
                    .features()
                    .observation()
                    .commands
                    .iter()
                    .any(|command| command.sequence == sequence)
                    || !result.feedback_sequences.insert(sequence)
                {
                    return Err(SipHostError::NotReady.into());
                }
                // Consumption precedes audio feedback, as in the original.
                media.command_feedback(success)
            }
        };
        action.map_err(|_| NativeConfigError::OutcomeUnknown.into())
    }
    pub(crate) fn dial(
        &self,
        expected: &NativeSipCall,
        input: NativeDial,
    ) -> Result<NativeSipOperation<String>, NativeSipError> {
        let state = self.0.state.lock().unwrap();
        Self::original(&state, expected)?;
        let config = state.configuration.as_ref().ok_or(SipHostError::NotReady)?;
        let descriptor = input.descriptor(config.registration.as_ref())?;
        let observation = expected.observation();
        if observation.attempted || observation.ending || expected.incoming().is_some() {
            return Err(NativeConfigError::OutcomeUnknown.into());
        }
        expected.0.result.lock().unwrap().attempted = true;
        let Some(descriptor) = descriptor else {
            return self.failed_dial_dispatch(expected);
        };
        let media = expected.media().ok_or(SipHostError::NotReady)?;
        if input.waiting && media.begin_waiting().is_err() {
            return self.failed_dial_dispatch(expected);
        }
        let delayed = config.binding.offer_mode() == crate::native_config::OfferMode::Delayed
            || config.binding.offer_mode() == crate::native_config::OfferMode::Auto
                && config.codecs.preferences()[0] == crate::media::Codec::Evs;
        let body = if delayed {
            String::new()
        } else {
            match media.create_offer() {
                Some(offer) => offer.to_sdp(),
                None => return self.failed_dial_dispatch(expected),
            }
        };
        let request = descriptor.request(
            &observation.id,
            self.0
                .host
                .observation()
                .endpoint
                .ok_or(SipHostError::NotReady)?,
            config.binding.transport(),
            body,
        );
        // The original BeginWaiting precedes dispatch; it does not grant live
        // caller audio or start a replacement sender.
        let pending = self.0.host.begin_dial(
            &expected.0.original,
            request,
            descriptor.next_hop,
            descriptor.credentials,
            input.ring_seconds as u32,
        );
        let pending = match pending {
            Ok(pending) => pending,
            Err(_) => return self.failed_dial_dispatch(expected),
        };
        let owner = self.clone();
        let call = expected.clone();
        Ok(retained(async move {
            let completed = tokio::time::timeout(
                std::time::Duration::from_secs(input.ring_seconds as u64 + 15),
                pending,
            )
            .await;
            let original = call.observation();
            {
                let mut result = call.0.result.lock().unwrap();
                match completed {
                    Ok(Ok(CallOutcome::Unknown)) => {
                        result.unknown = true;
                        result.ending = true;
                        result.error = Some("sip_operation_outcome_unknown");
                    }
                    Ok(Ok(CallOutcome::Connected))
                        if !result.ending
                            && !result.unknown
                            && original.phase == SipHostCallPhase::Connected =>
                    {
                        owner.connected(&call, &mut result)
                    }
                    Ok(Ok(CallOutcome::Ended | CallOutcome::Connected))
                        if !result.ending && !result.unknown =>
                    {
                        result.ending = true;
                        result.error = Some("sip_dial_not_connected");
                    }
                    Ok(Ok(_)) if result.ending || result.unknown => (),
                    _ => {
                        result.unknown = true;
                        result.ending = true;
                        result.error = Some("sip_operation_outcome_unknown");
                    }
                }
            }
            if call.0.result.lock().unwrap().ending {
                let _ = owner.0.host.hangup(&call.0.original);
            }
            crate::native_sip_status::call_json(&call).map_err(|_| SipHostError::CleanupUnconfirmed)
        }))
    }
    pub(crate) fn answer(
        &self,
        expected: &NativeSipCall,
        waiting: bool,
    ) -> Result<NativeSipOperation<String>, NativeSipError> {
        let state = self.0.state.lock().unwrap();
        Self::original(&state, expected)?;
        let observation = expected.observation();
        if observation.attempted || observation.ending || expected.incoming().is_none() {
            return Err(NativeConfigError::OutcomeUnknown.into());
        }
        expected.0.result.lock().unwrap().attempted = true;
        let media = expected.media().ok_or(SipHostError::NotReady)?;
        let pending = match self.0.host.begin_answer(&expected.0.original, move || {
            if waiting {
                media
                    .begin_waiting()
                    .map_err(|_| SipHostError::InitializationFailed)?;
            }
            Ok(())
        }) {
            Ok(pending) => pending,
            Err(_) => return self.failed_dial_dispatch(expected),
        };
        let owner = self.clone();
        let call = expected.clone();
        Ok(retained(async move {
            let completed = tokio::time::timeout(std::time::Duration::from_secs(15), pending).await;
            let original = call.observation();
            {
                let mut result = call.0.result.lock().unwrap();
                match completed {
                    Ok(Ok(CallOutcome::Connected))
                        if !result.ending
                            && !result.unknown
                            && original.phase == SipHostCallPhase::Connected =>
                    {
                        owner.connected(&call, &mut result)
                    }
                    Ok(Ok(CallOutcome::Ended | CallOutcome::Connected))
                        if !result.ending && !result.unknown =>
                    {
                        result.ending = true;
                        result.error = Some("sip_answer_not_connected");
                    }
                    Ok(Ok(_)) if result.ending || result.unknown => (),
                    _ => {
                        result.unknown = true;
                        result.ending = true;
                        result.error = Some("sip_operation_outcome_unknown");
                    }
                }
            }
            if call.0.result.lock().unwrap().ending {
                let _ = owner.0.host.hangup(&call.0.original);
            }
            crate::native_sip_status::call_json(&call).map_err(|_| SipHostError::CleanupUnconfirmed)
        }))
    }
    fn failed_dial_dispatch(
        &self,
        expected: &NativeSipCall,
    ) -> Result<NativeSipOperation<String>, NativeSipError> {
        let mut result = expected.0.result.lock().unwrap();
        result.unknown = true;
        result.ending = true;
        result.error = Some("sip_operation_outcome_unknown");
        drop(result);
        self.0.host.hangup(&expected.0.original)?;
        let snapshot = crate::native_sip_status::call_json(expected)?;
        Ok(retained(async move { Ok(snapshot) }))
    }
    pub fn configure_features(
        &self,
        expected: &NativeSipCall,
        challenge: Option<CallAccessSettings>,
    ) -> Result<CallFeatureObservation, NativeSipError> {
        let state = self.0.state.lock().unwrap();
        Self::original(&state, expected)?;
        let observation = expected.observation();
        if observation.attempted || observation.ending {
            return Err(FeatureError::OutcomeUnknown.into());
        }
        expected
            .0
            .features
            .configure(challenge, expected.incoming().is_some())?;
        Ok(expected.0.features.observation())
    }
    pub fn release(
        &self,
        expected: &NativeSipCall,
    ) -> Result<NativeSipOperation<()>, SipHostError> {
        let state = self.0.state.lock().unwrap();
        Self::original(&state, expected)?;
        let mut releasing = expected.0.releasing.lock().unwrap();
        if let Some(operation) = &*releasing {
            return Ok(operation.clone());
        }
        let owner = self.clone();
        let call = expected.clone();
        // Revoke even if no observer polls the retained disposal operation.
        expected.0.result.lock().unwrap().ending = true;
        if let Some(deadline) = &expected.0.result.lock().unwrap().screening_deadline {
            deadline.cancel();
        }
        self.0.host.hangup(&expected.0.original)?;
        expected.0.features.dispose();
        let operation = retained(async move {
            owner.0.host.release(&call.0.original).await?;
            owner
                .0
                .state
                .lock()
                .unwrap()
                .calls
                .retain(|c| !Arc::ptr_eq(&c.0, &call.0));
            Ok(())
        });
        *releasing = Some(operation.clone());
        Ok(operation)
    }
    pub fn close(&self) -> NativeSipOperation<()> {
        let mut state = self.0.state.lock().unwrap();
        if let Some(operation) = &state.closing {
            return operation.clone();
        }
        state.stopping = true;
        for call in state.calls.iter() {
            call.0.result.lock().unwrap().ending = true;
            if let Some(deadline) = &call.0.result.lock().unwrap().screening_deadline {
                deadline.cancel();
            }
            let _ = self.0.host.hangup(&call.0.original);
            call.0.features.dispose();
        }
        let owner = self.clone();
        let configuring = state.configuring.clone();
        let operation = retained(async move {
            // Failed optional setup can still own native resources. Its
            // uncertainty must not become a confirmed close or free binding.
            if let Some(configuring) = configuring {
                if configuring.wait().await == Err(SipHostError::CleanupUnconfirmed) {
                    return Err(SipHostError::CleanupUnconfirmed);
                }
            }
            owner.0.host.close().await?;
            owner.0.state.lock().unwrap().calls.clear();
            Ok(())
        });
        state.closing = Some(operation.clone());
        operation
    }
}
fn native_audio_error(error: crate::media::SipMediaError) -> NativeSipError {
    match error {
        crate::media::SipMediaError::Closed => SipHostError::NotReady.into(),
        crate::media::SipMediaError::Audio(crate::media::CallAudioError::InvalidRequest) => {
            NativeConfigError::InvalidRequest.into()
        }
        _ => NativeConfigError::OutcomeUnknown.into(),
    }
}
