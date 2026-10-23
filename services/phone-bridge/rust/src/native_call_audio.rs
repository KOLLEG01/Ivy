//! Original call-bound audio preparation and Voice recovery. Audio work remains
//! retained by its own port; command observers never own its lifetime.
use super::*;
use crate::{
    media::{AudioDesktopIdentity, AudioOperation},
    sip::DelayedInviteMedia,
};
use tokio_util::sync::CancellationToken;

impl NativeSipOperations {
    pub(crate) fn prepare_audio(
        &self,
        call: &NativeSipCall,
        settings: NativeAudioSettings,
        desktop: AudioDesktopIdentity,
    ) -> Result<NativeSipOperation<Result<String, NativeSipError>>, NativeSipError> {
        Self::original(&self.0.state.lock().unwrap(), call)?;
        let media = call.media().ok_or(SipHostError::NotReady)?;
        // SipMediaSession checks its lifetime before settings.Validate.
        if media.is_closed() || media.failed() {
            return Err(SipHostError::NotReady.into());
        }
        let settings = settings.validated()?;
        let work = media
            .prepare_audio(settings, Some(desktop), CancellationToken::new())
            .map_err(native_audio_error)?;
        Ok(audio_result(media, work, false))
    }
    pub(crate) fn rebind_audio(
        &self,
        call: &NativeSipCall,
        desktop: AudioDesktopIdentity,
    ) -> Result<NativeSipOperation<Result<String, NativeSipError>>, NativeSipError> {
        Self::original(&self.0.state.lock().unwrap(), call)?;
        let media = call.media().ok_or(SipHostError::NotReady)?;
        let observation = call.observation();
        if observation.ending
            || observation.phase != SipHostCallPhase::Connected
            || media.is_closed()
            || media.failed()
        {
            return Err(SipHostError::NotReady.into());
        }
        let selected = call
            .0
            .resources
            .selected_desktop()
            .ok_or(SipHostError::NotReady)?;
        let original = Arc::downgrade(&call.0);
        let identity = desktop.clone();
        let same_voice = Arc::new(move || {
            let Some(call) = original.upgrade().map(NativeSipCall) else {
                return false;
            };
            let observed = call.observation();
            if observed.ending
                || observed.phase != SipHostCallPhase::Connected
                || !call.0.resources.retains_desktop(&selected)
                || call.media().is_none_or(|m| m.is_closed() || m.failed())
            {
                return false;
            }
            selected.has_original_voice_capture(&identity)
        });
        let work = media
            .call_audio()
            .rebind(desktop, same_voice)
            .map_err(NativeSipError::AudioRecovery)?;
        Ok(audio_result(media, work, true))
    }
}
fn audio_result(
    media: SipMediaSession,
    work: AudioOperation,
    recovery: bool,
) -> NativeSipOperation<Result<String, NativeSipError>> {
    retained(async move {
        Ok(match work.wait().await {
            Ok(()) => crate::media::native_view::audio_json(&media.call_audio().status())
                .map_err(|_| NativeConfigError::OutcomeUnknown.into()),
            Err(error) => Err(if recovery {
                NativeSipError::AudioRecovery(error)
            } else {
                native_audio_error(error.into())
            }),
        })
    })
}
