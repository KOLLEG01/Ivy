//! Concrete media construction, invoked only after native call admission.
use crate::{
    media::{
        CallAudioFactory, CallAudioPort, EvsLibrary, RtcpRuntime, SipMediaEntropy, SipMediaError,
        SipMediaIdentity, SipMediaSession, SipReceiveEvents, SipSdpRoute, SystemSipMediaEntropy,
        SystemSipSdpRoute,
    },
    native_sip::{NativeCallConfiguration, NativeSipMediaFactory, NativeSipMediaFuture},
    sip::SipHostError,
};
use std::sync::Arc;

type Events = dyn Fn(&str) -> Arc<dyn SipReceiveEvents> + Send + Sync;

pub struct NativeSipSessionFactory {
    audio: Arc<dyn CallAudioFactory>,
    evs: Option<Arc<EvsLibrary>>,
    runtime: Arc<dyn RtcpRuntime>,
    entropy: Arc<dyn SipMediaEntropy>,
    routes: Arc<dyn SipSdpRoute>,
    events: Arc<Events>,
    #[cfg(test)]
    realtime: Option<Arc<crate::media::realtime::route::RouteFactory>>,
}
impl NativeSipSessionFactory {
    /// Constructing the factory opens no device, socket or entropy provider.
    /// Event creation is bounded and returns only the admitted call's observers.
    pub fn new(
        audio: Arc<dyn CallAudioFactory>,
        evs: Option<Arc<EvsLibrary>>,
        runtime: Arc<dyn RtcpRuntime>,
        events: impl Fn(&str) -> Arc<dyn SipReceiveEvents> + Send + Sync + 'static,
    ) -> Self {
        Self::with_providers(
            audio,
            evs,
            runtime,
            events,
            Arc::new(SystemSipMediaEntropy),
            Arc::new(SystemSipSdpRoute),
        )
    }
    pub fn with_providers(
        audio: Arc<dyn CallAudioFactory>,
        evs: Option<Arc<EvsLibrary>>,
        runtime: Arc<dyn RtcpRuntime>,
        events: impl Fn(&str) -> Arc<dyn SipReceiveEvents> + Send + Sync + 'static,
        entropy: Arc<dyn SipMediaEntropy>,
        routes: Arc<dyn SipSdpRoute>,
    ) -> Self {
        Self {
            audio,
            evs,
            runtime,
            entropy,
            routes,
            events: Arc::new(events),
            #[cfg(test)]
            realtime: None,
        }
    }
    #[cfg(test)]
    pub(crate) fn test_realtime_factory(
        mut self,
        factory: Arc<crate::media::realtime::route::RouteFactory>,
    ) -> Self {
        self.realtime = Some(factory);
        self
    }
}
impl NativeSipMediaFactory for NativeSipSessionFactory {
    fn create(
        &self,
        id: String,
        configuration: Arc<NativeCallConfiguration>,
    ) -> NativeSipMediaFuture {
        let (audio, evs, runtime, entropy, routes, events) = (
            self.audio.clone(),
            self.evs.clone(),
            self.runtime.clone(),
            self.entropy.clone(),
            self.routes.clone(),
            self.events.clone(),
        );
        #[cfg(test)]
        let realtime = self.realtime.clone();
        Box::pin(async move {
            let identity = SipMediaIdentity::generate(entropy.as_ref())
                .map_err(|_| SipHostError::InitializationFailed)?;
            let settings = configuration.media_settings(
                id.clone(),
                identity,
                configuration.binding().endpoint().ip(),
            );
            let events = events(&id);
            let audio = CallAudioPort::new(id, audio).map_err(|_| SipHostError::InvalidRequest)?;
            #[cfg(test)]
            let audio = match realtime {
                Some(factory) => audio.test_realtime_factory(factory),
                None => audio,
            };
            SipMediaSession::bind_with_route(settings, audio, evs, runtime, events, routes)
                .await
                .map_err(|error| {
                    if error == SipMediaError::CleanupUnconfirmed {
                        SipHostError::CleanupUnconfirmed
                    } else {
                        SipHostError::InitializationFailed
                    }
                })
        })
    }
}
