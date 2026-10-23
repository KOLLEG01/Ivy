//! The native executable's platform composition. Construction starts no media,
//! device, desktop or provider activity; each owner keeps its existing admission.
use crate::native_pipe::NativeRuntime;
use std::sync::Arc;

pub fn system_runtime() -> std::io::Result<Arc<dyn NativeRuntime>> {
    #[cfg(all(windows, target_pointer_width = "64"))]
    {
        windows::runtime()
    }
    #[cfg(not(all(windows, target_pointer_width = "64")))]
    {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "Native Phone runtime is unavailable on this platform.",
        ))
    }
}

#[cfg(all(windows, target_pointer_width = "64"))]
mod windows {
    use super::*;
    use crate::{
        dtmf::InputSource,
        media::{EvsLibrary, SipReceiveEvents, WindowsAudioFactory, WindowsRtcpRuntime},
        native_codec::CodecLibraryError,
        native_pipe::NativeShutdown,
        native_rpc::{NativeHandler, NativeInvocation, NativeOutcome, NativeRequest},
        native_sip::{NativeCallConfiguration, NativeSipMediaFactory, NativeSipMediaFuture},
        native_sip_commands::NativeSipCommands,
        native_sip_media::NativeSipSessionFactory,
        sip::SipHostError,
    };
    use std::{net::SocketAddr, path::PathBuf, sync::OnceLock};

    struct CodecLibrary {
        directory: PathBuf,
        loaded: OnceLock<Result<Arc<EvsLibrary>, CodecLibraryError>>,
    }
    impl CodecLibrary {
        fn load(&self) -> Result<Arc<EvsLibrary>, CodecLibraryError> {
            self.loaded
                .get_or_init(|| {
                    if !self.directory.join(EvsLibrary::FILE_NAME).is_file() {
                        return Err(CodecLibraryError::Missing);
                    }
                    // SAFETY: This is the same immutable installed package directory
                    // as the verified native executable, admitted by the Host owner.
                    // Never load a caller-supplied path or a library from PATH/CWD.
                    unsafe { EvsLibrary::load_packaged(&self.directory) }
                        .map_err(|_| CodecLibraryError::Incompatible)
                })
                .clone()
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
                    Arc::new(WindowsAudioFactory),
                    evs,
                    Arc::new(WindowsRtcpRuntime),
                    |_| Arc::new(ReceiveEvents),
                )
                .create(id, config)
                .await
            })
        }
    }
    // The session applies its call-policy digit hook and records failures/RTCP
    // before these optional telemetry notifications. Do not log access digits.
    struct ReceiveEvents;
    impl SipReceiveEvents for ReceiveEvents {
        fn digit(&self, _: char, _: InputSource) {}
        fn failed(&self) {
            eprintln!("{{\"event\":\"phone_media_failed\"}}");
        }
        fn rtcp(&self, _: SocketAddr, _: &[u8]) {}
    }
    // All supported commands are handled by the composed owners. Unknown names
    // get the established refusal; this terminal owns no resources to drain.
    struct UnsupportedCommands;
    impl NativeHandler for UnsupportedCommands {
        fn invoke(&self, _: NativeRequest) -> NativeInvocation {
            Box::pin(async { NativeOutcome::error("invalid_request") })
        }
        fn cleanup_slots(&self) -> usize {
            0
        }
    }
    impl NativeRuntime for UnsupportedCommands {
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
        Ok(Arc::new(NativeSipCommands::windows_with_diagnostics(
            Arc::new(Media(library)),
            Arc::new(UnsupportedCommands),
            |call| match call.media() {
                Some(media) => media
                    .call_audio()
                    .realtime_observation()
                    .map(|value| value.unwrap_or_else(|| "null".into()))
                    .map_err(|_| SipHostError::CleanupUnconfirmed.into()),
                None => Ok("null".into()),
            },
            move || diagnostics.load(),
        )))
    }
}
