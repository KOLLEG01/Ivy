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
    native_rpc::NativeRequest,
    sip::{
        HostedSipMedia, RegistrationObservation, SipHost, SipHostCall, SipHostCallObservation,
        SipHostError, SipHostObservation,
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
    Features(FeatureError),
    Configuration(NativeConfigError),
    Host(SipHostError),
}
impl NativeSipError {
    pub fn code(self) -> &'static str {
        match self {
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
    registration: bool,
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
        self.registration
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
            registration: false,
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
    releasing: Mutex<Option<NativeSipOperation<()>>>,
}
#[derive(Clone)]
pub struct NativeSipCall(Arc<Call>);
impl NativeSipCall {
    pub fn features(&self) -> PhoneCallFeatures {
        self.0.features.clone()
    }
    pub fn observation(&self) -> SipHostCallObservation {
        self.0.original.observation()
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
    calls: Vec<NativeSipCall>,
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
impl NativeSipOperations {
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
                calls: Vec::new(),
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
        let registration = parsed.has_registration();
        let parts = parsed.into_parts()?;
        state.configuration = Some(Arc::new(NativeCallConfiguration {
            binding: parts.binding,
            codecs: parts.codecs,
            incoming: parts.incoming,
            registration,
        }));
        let host = self.0.host.clone();
        let operation = retained(async move {
            host.configure_with_setup(parts.host, move || setup(parts.micro))
                .await
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
        if state.calls.iter().any(|call| call.observation().id == id) {
            return Err(SipHostError::OperationConflict);
        }
        let factory = self.0.media.clone();
        let media = Arc::new(OnceLock::new());
        let saved = media.clone();
        let original_id = id.clone();
        let owner = Arc::downgrade(&self.0);
        let original_slot = Arc::new(OnceLock::<std::sync::Weak<Call>>::new());
        let hangup_slot = original_slot.clone();
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
        let original = self.0.host.prepare_async(id, move || async move {
            let session = factory.create(original_id, configuration).await?;
            // Publish the exact session even if policy attachment is uncertain;
            // it must remain fenced for explicit original-call cleanup.
            let _ = saved.set(session.clone());
            if session.enable_call_features(media_features).is_err() {
                session.request_close();
                return Err(SipHostError::CleanupUnconfirmed);
            }
            Ok(Arc::new(session) as Arc<dyn HostedSipMedia>)
        })?;
        let call = NativeSipCall(Arc::new(Call {
            original,
            media,
            features,
            releasing: Mutex::new(None),
        }));
        let _ = original_slot.set(Arc::downgrade(&call.0));
        state.calls.push(call.clone());
        Ok(call)
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
        self.0.host.hangup(&expected.0.original)
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
        // This admission owner currently prepares outgoing calls only. Incoming
        // admission must supply its original direction before gaining authority.
        expected.0.features.configure(challenge, false)?;
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
        for call in &state.calls {
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
