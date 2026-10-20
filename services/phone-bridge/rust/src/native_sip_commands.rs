//! Native SIP lifecycle commands composed with process-local replay receipts.
//! The caller retains the complete call view and the remaining native runtime.
use crate::{
    native_config::NativeMicroSettings,
    native_pipe::{NativeRuntime, NativeShutdown, NativeShutdownError},
    native_rpc::{NativeHandler, NativeInvocation, NativeOutcome, NativeRequest},
    native_sip::{
        NativeSipCall, NativeSipError, NativeSipOperation, NativeSipOperations, retained,
    },
    sip::SipHostError,
};
use ivy_contracts::{IvyJson, json::stringify};
use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
};

pub type NativeMicroSetup =
    Pin<Box<dyn Future<Output = Result<(), SipHostError>> + Send + 'static>>;
type Setup = dyn Fn(Option<NativeMicroSettings>) -> NativeMicroSetup + Send + Sync;
type CallView = dyn Fn(&NativeSipCall) -> Result<IvyJson, NativeSipError> + Send + Sync;

/// This owns configure/prepare/hangup/release. Status, diagnostics, features,
/// dial/answer and platform commands remain with the required original runtime.
/// The view must project the supplied original handle; it must not find a call
/// again by ID or expose credentials. There is no placeholder platform runtime
/// or default call/feature/status projection.
pub struct NativeSipCommands {
    owner: NativeSipOperations,
    remaining: Arc<dyn NativeRuntime>,
    setup: Arc<Setup>,
    view: Arc<CallView>,
    shutdown: Mutex<Option<NativeSipOperation<()>>>,
}
impl NativeSipCommands {
    pub fn new(
        owner: NativeSipOperations,
        remaining: Arc<dyn NativeRuntime>,
        setup: impl Fn(Option<NativeMicroSettings>) -> NativeMicroSetup + Send + Sync + 'static,
        view: impl Fn(&NativeSipCall) -> Result<IvyJson, NativeSipError> + Send + Sync + 'static,
    ) -> Self {
        Self {
            owner,
            remaining,
            setup: Arc::new(setup),
            view: Arc::new(view),
            shutdown: Mutex::new(None),
        }
    }
    fn lifecycle(&self, request: NativeRequest) -> Result<NativeInvocation, NativeSipError> {
        let state = self.owner.observation();
        if state.stopping {
            return Err(SipHostError::Stopping.into());
        }
        // The original handler selects a call before method-specific DTO
        // validation. Null-conditional host lookup does not decode callId
        // until the host exists. GetString on another JSON kind throws a
        // generic native error, independently of exact-field validation.
        let selected = if state.endpoint.is_some() {
            request
                .params()
                .get("callId")
                .map(selected_id)
                .transpose()?
        } else {
            None
        };
        if request.method() == "configure" {
            let setup = self.setup.clone();
            let work = self.owner.configure(&request, move |micro| setup(micro))?;
            return Ok(Box::pin(async move {
                match work.wait().await {
                    Ok(endpoint) => result(IvyJson::object([
                        ("configured", true.into()),
                        ("endpoint", endpoint.to_string().into()),
                    ])),
                    Err(error) => failed(error.into()),
                }
            }));
        }
        // A partial/failed setup cannot grant authority to construct media.
        if self
            .owner
            .configuration_result()
            .is_none_or(|result| result.is_err())
        {
            return Err(SipHostError::NotReady.into());
        }
        let fields = request
            .params()
            .as_object()
            .expect("validated native params");
        if fields.len() != 1 || request.params().get("callId").is_none() {
            return Err(SipHostError::InvalidRequest.into());
        }
        let view = self.view.clone();
        if request.method() == "call.prepare" {
            let id = selected.ok_or(SipHostError::InvalidRequest)?;
            // Reserve synchronously under NativeDispatcher admission, before
            // returning the observer-independent construction future.
            let call = self.owner.prepare(id)?;
            return Ok(Box::pin(async move {
                match call.prepared().await {
                    Ok(()) => match view(&call) {
                        Ok(value) => result(value),
                        Err(error) => failed(error),
                    },
                    Err(error) => failed(error.into()),
                }
            }));
        }
        let call = selected
            .and_then(|id| self.owner.find(&id))
            .ok_or(SipHostError::NotReady)?;
        if request.method() == "call.hangup" {
            self.owner.hangup(&call)?;
            let outcome = match view(&call) {
                Ok(value) => result(value),
                Err(error) => failed(error),
            };
            return Ok(Box::pin(async move { outcome }));
        }
        let id = call.observation().id;
        let release = self.owner.release(&call)?;
        Ok(Box::pin(async move {
            match release.wait().await {
                Ok(()) => result(IvyJson::object([
                    ("callId", id.into()),
                    ("released", true.into()),
                ])),
                Err(error) => failed(error.into()),
            }
        }))
    }
}
fn selected_id(value: &IvyJson) -> Result<String, NativeSipError> {
    if value.is_null() {
        // GetString accepts null, but the original dictionary lookup rejects it.
        return Err(SipHostError::InvalidRequest.into());
    }
    value
        .as_str()
        .map_err(|_| crate::native_config::NativeConfigError::OutcomeUnknown.into())
}
fn result(value: IvyJson) -> NativeOutcome {
    match stringify(&value, crate::ipc::FRAME_BYTES) {
        Ok(value) => NativeOutcome::result(value),
        Err(_) => NativeOutcome::error("operation_outcome_unknown"),
    }
}
fn failed(error: NativeSipError) -> NativeOutcome {
    NativeOutcome::error(error.code())
}
impl NativeHandler for NativeSipCommands {
    fn cleanup_slots(&self) -> usize {
        (self.owner.observation().calls.len() * 4).max(4)
    }
    fn invoke(&self, request: NativeRequest) -> NativeInvocation {
        if !matches!(
            request.method(),
            "configure" | "call.prepare" | "call.hangup" | "call.release"
        ) {
            return self.remaining.invoke(request);
        }
        match self.lifecycle(request) {
            Ok(work) => work,
            Err(error) => Box::pin(async move { failed(error) }),
        }
    }
}
impl NativeRuntime for NativeSipCommands {
    fn shutdown(&self) -> NativeShutdown {
        let mut saved = self.shutdown.lock().unwrap();
        let original = saved
            .get_or_insert_with(|| {
                // Revoke SIP synchronously, even if the returned observer leaves.
                let sip = self.owner.close();
                let remaining = self.remaining.clone();
                retained(async move {
                    let sip_result = sip.wait().await;
                    // The original finally disposes Desktop/Micro even if SIP
                    // cleanup fails. Completion of either fence is never inferred.
                    let other_result = remaining.shutdown().await;
                    sip_result.and(other_result.map_err(|_| SipHostError::CleanupUnconfirmed))
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
