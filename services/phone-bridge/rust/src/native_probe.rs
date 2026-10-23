//! audio.probe shares only pending runs. SIP stop cancels before joining it;
//! original disposal ignores probe failure, then closes the remaining runtime.
use crate::{
    loopback_probe::{PhoneLoopbackProbe, ProbeFactory, ProbeOperation},
    native_desktop::native_result_json,
    native_pipe::{NativeRuntime, NativeShutdown, NativeShutdownError},
    native_rpc::{
        NativeHandler, NativeInvocation, NativeInvocationStart, NativeOutcome, NativeRequest,
    },
    native_sip::{NativeSipOperation, NativeSipOperations, retained},
    sip::SipHostError,
};
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;
struct State {
    closed: bool,
    work: Option<ProbeOperation>,
}
pub struct NativeProbeCommands {
    sip: NativeSipOperations,
    remaining: Arc<dyn NativeRuntime>,
    factory: Arc<dyn ProbeFactory>,
    stop: CancellationToken,
    state: Mutex<State>,
    closing: Mutex<Option<NativeSipOperation<()>>>,
}
impl NativeProbeCommands {
    pub fn new(
        sip: NativeSipOperations,
        remaining: Arc<dyn NativeRuntime>,
        factory: Arc<dyn ProbeFactory>,
    ) -> Self {
        Self {
            stop: sip.diagnostic_stop(),
            sip,
            remaining,
            factory,
            state: Mutex::new(State {
                closed: false,
                work: None,
            }),
            closing: Mutex::new(None),
        }
    }
    fn probe(&self, request: NativeRequest) -> NativeInvocation {
        if !request.params().as_object().is_some_and(|p| p.is_empty()) {
            return Box::pin(async { NativeOutcome::error("invalid_request") });
        }
        let mut state = self.state.lock().unwrap();
        if state.closed {
            return Box::pin(async { NativeOutcome::error("runtime_not_ready") });
        }
        let work = self.sip.with_idle_diagnostic(|| {
            if state.work.as_ref().is_none_or(ProbeOperation::finished) {
                let sip = self.sip.clone();
                state.work = Some(PhoneLoopbackProbe::start(
                    self.factory.clone(),
                    move || sip.diagnostic_idle(),
                    self.stop.clone(),
                ));
            }
            state.work.as_ref().unwrap().clone()
        });
        let Ok(work) = work else {
            return Box::pin(async { NativeOutcome::error("runtime_not_ready") });
        };
        Box::pin(async move {
            match work.wait().await.and_then(|r| {
                native_result_json(&r.json())
                    .map_err(|_| crate::loopback_probe::ProbeError::Unavailable)
            }) {
                Ok(json) => NativeOutcome::result(json),
                Err(_) => NativeOutcome::error("operation_outcome_unknown"),
            }
        })
    }
}
impl NativeHandler for NativeProbeCommands {
    fn cleanup_slots(&self) -> usize {
        self.remaining.cleanup_slots()
    }
    fn start(&self, request: NativeRequest) -> NativeInvocationStart {
        if request.method() != "audio.probe" {
            return self.remaining.start(request);
        }
        let work = self.probe(request);
        Box::pin(async move { work })
    }
    fn invoke(&self, request: NativeRequest) -> NativeInvocation {
        if request.method() == "audio.probe" {
            self.probe(request)
        } else {
            self.remaining.invoke(request)
        }
    }
}
impl NativeRuntime for NativeProbeCommands {
    fn shutdown(&self) -> NativeShutdown {
        let work = {
            let mut state = self.state.lock().unwrap();
            state.closed = true;
            self.stop.cancel();
            state.work.clone()
        };
        let mut saved = self.closing.lock().unwrap();
        let original = saved
            .get_or_insert_with(|| {
                let remaining = self.remaining.clone();
                retained(async move {
                    if let Some(work) = work {
                        let _ = work.wait().await;
                    }
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
