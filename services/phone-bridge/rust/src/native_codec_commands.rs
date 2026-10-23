//! Native codec.test admission and one retained diagnostic per process.
use crate::{
    media::EvsLibrary,
    native_codec::{CodecLibraryError, codec_test},
    native_desktop::native_result_json,
    native_pipe::{NativeRuntime, NativeShutdown, NativeShutdownError},
    native_rpc::{
        NativeHandler, NativeInvocation, NativeInvocationStart, NativeOutcome, NativeRequest,
    },
    native_sip::{NativeSipOperation, NativeSipOperations, retained},
    sip::SipHostError,
};
use std::sync::{Arc, Mutex};

type Library = dyn Fn() -> Result<Arc<EvsLibrary>, CodecLibraryError> + Send + Sync;
#[derive(Default)]
struct State {
    closed: bool,
    work: Option<NativeSipOperation<String>>,
}
pub struct NativeCodecCommands {
    sip: NativeSipOperations,
    remaining: Arc<dyn NativeRuntime>,
    library: Arc<Library>,
    state: Mutex<State>,
    closing: Mutex<Option<NativeSipOperation<()>>>,
}
impl NativeCodecCommands {
    /// The loader authenticates package code before returning a library. It runs
    /// only for the first admitted diagnostic on the blocking worker, not at setup.
    pub fn new(
        sip: NativeSipOperations,
        remaining: Arc<dyn NativeRuntime>,
        library: impl Fn() -> Result<Arc<EvsLibrary>, CodecLibraryError> + Send + Sync + 'static,
    ) -> Self {
        Self {
            sip,
            remaining,
            library: Arc::new(library),
            state: Mutex::new(State::default()),
            closing: Mutex::new(None),
        }
    }
    fn diagnostic(&self, request: NativeRequest) -> NativeInvocation {
        if !request
            .params()
            .as_object()
            .is_some_and(|params| params.is_empty())
        {
            return Box::pin(async { NativeOutcome::error("invalid_request") });
        }
        let mut state = self.state.lock().unwrap();
        if state.closed {
            return Box::pin(async { NativeOutcome::error("runtime_stopping") });
        }
        let original = self.sip.with_idle_diagnostic(|| {
            state
                .work
                .get_or_insert_with(|| {
                    let library = self.library.clone();
                    // Like Task.Run, publication of the observer never owns the run.
                    let work = tokio::task::spawn_blocking(move || {
                        native_result_json(&codec_test(library()))
                            .map_err(|_| SipHostError::CleanupUnconfirmed)
                    });
                    retained(
                        async move { work.await.map_err(|_| SipHostError::CleanupUnconfirmed)? },
                    )
                })
                .clone()
        });
        let original = match original {
            Ok(work) => work,
            Err(error) => {
                let code = if error == SipHostError::Stopping {
                    "runtime_stopping"
                } else {
                    "runtime_not_ready"
                };
                return Box::pin(async move { NativeOutcome::error(code) });
            }
        };
        Box::pin(async move {
            match original.wait().await {
                Ok(json) => NativeOutcome::result(json),
                Err(_) => NativeOutcome::error("operation_outcome_unknown"),
            }
        })
    }
}
impl NativeHandler for NativeCodecCommands {
    fn cleanup_slots(&self) -> usize {
        self.remaining.cleanup_slots()
    }
    fn start(&self, request: NativeRequest) -> NativeInvocationStart {
        if request.method() != "codec.test" {
            return self.remaining.start(request);
        }
        let work = self.diagnostic(request);
        Box::pin(async move { work })
    }
    fn invoke(&self, request: NativeRequest) -> NativeInvocation {
        if request.method() == "codec.test" {
            self.diagnostic(request)
        } else {
            self.remaining.invoke(request)
        }
    }
}
impl NativeRuntime for NativeCodecCommands {
    fn shutdown(&self) -> NativeShutdown {
        self.state.lock().unwrap().closed = true;
        let mut saved = self.closing.lock().unwrap();
        let original = saved
            .get_or_insert_with(|| {
                let remaining = self.remaining.clone();
                // Original disposal does not await its bounded CPU diagnostic.
                retained(async move {
                    remaining
                        .shutdown()
                        .await
                        .map_err(|_| SipHostError::CleanupUnconfirmed)
                })
            })
            .clone();
        Box::pin(async move {
            original
                .wait()
                .await
                .map_err(|_| NativeShutdownError::OutcomeUnknown)
        })
    }
}
