//! Linux SIP and digital Voice. Windows device/Desktop routes remain unavailable.
use super::{CodecLibrary, NativeRuntime};
use crate::{
    dtmf::InputSource,
    media::{
        AudioDesktopIdentity, AudioRoutePermit, AudioSettings, CallAudioError, CallAudioFactory,
        CallAudioRoute, LinuxRtcpRuntime, SipReceiveEvents,
    },
    native_pipe::NativeShutdown,
    native_rpc::{NativeHandler, NativeInvocation, NativeOutcome, NativeRequest},
    native_sip::{
        NativeCallConfiguration, NativeSipMediaFactory, NativeSipMediaFuture, NativeSipOperations,
    },
    native_sip_commands::NativeSipCommands,
    native_sip_media::NativeSipSessionFactory,
    native_sip_status::NativeSipViews,
    sip::SipHostError,
};
use std::{
    future::Future,
    net::SocketAddr,
    pin::Pin,
    sync::{Arc, OnceLock},
};
use tokio_util::sync::CancellationToken;

struct DesktopAudio;
impl CallAudioFactory for DesktopAudio {
    fn open<'a>(
        &'a self,
        _: &'a AudioSettings,
        _: Option<&'a AudioDesktopIdentity>,
        _: AudioRoutePermit,
        _: CancellationToken,
    ) -> Pin<Box<dyn Future<Output = Result<Arc<dyn CallAudioRoute>, CallAudioError>> + Send + 'a>>
    {
        // No device was opened. Digital realtime uses its own existing route.
        Box::pin(async { Err(CallAudioError::PreparationReleased) })
    }
}
struct Media(Arc<CodecLibrary>);
impl NativeSipMediaFactory for Media {
    fn create(&self, id: String, config: Arc<NativeCallConfiguration>) -> NativeSipMediaFuture {
        let library = self.0.clone();
        Box::pin(async move {
            let evs = tokio::task::spawn_blocking(move || library.load())
                .await
                .map_err(|_| SipHostError::InitializationFailed)?
                .ok();
            NativeSipSessionFactory::new(
                Arc::new(DesktopAudio),
                evs,
                Arc::new(LinuxRtcpRuntime),
                |_| Arc::new(ReceiveEvents),
            )
            .create(id, config)
            .await
        })
    }
}
struct ReceiveEvents;
impl SipReceiveEvents for ReceiveEvents {
    fn digit(&self, _: char, _: InputSource) {}
    fn failed(&self) {
        eprintln!("{{\"event\":\"phone_media_failed\"}}");
    }
    fn rtcp(&self, _: SocketAddr, _: &[u8]) {}
}
struct PlatformCommands;
impl NativeHandler for PlatformCommands {
    fn invoke(&self, _: NativeRequest) -> NativeInvocation {
        Box::pin(async { NativeOutcome::error("invalid_request") })
    }
    fn cleanup_slots(&self) -> usize {
        0
    }
}
impl NativeRuntime for PlatformCommands {
    fn shutdown(&self) -> NativeShutdown {
        Box::pin(async { Ok(()) })
    }
}

pub(super) fn runtime() -> std::io::Result<Arc<dyn NativeRuntime>> {
    let directory = std::env::current_exe()?
        .parent()
        .ok_or_else(|| std::io::Error::other("Native package directory is unavailable."))?
        .to_path_buf();
    let library = Arc::new(CodecLibrary {
        directory,
        loaded: OnceLock::new(),
    });
    let diagnostics = library.clone();
    let owner = NativeSipOperations::new(Arc::new(Media(library)));
    let views = NativeSipViews::new(
        || Ok("null".into()),
        |call| match call.media() {
            Some(media) => media
                .call_audio()
                .realtime_observation()
                .map(|value| value.unwrap_or_else(|| "null".into()))
                .map_err(|_| SipHostError::CleanupUnconfirmed.into()),
            None => Ok("null".into()),
        },
    );
    Ok(Arc::new(
        NativeSipCommands::new(
            owner,
            Arc::new(PlatformCommands),
            |micro| {
                Box::pin(async move {
                    if micro.is_some() {
                        Err(SipHostError::InvalidRequest)
                    } else {
                        Ok(())
                    }
                })
            },
            views,
        )
        .with_diagnostics(
            move || diagnostics.load(),
            || Err(crate::native_inventory::InventoryError),
        ),
    ))
}
