//! Native SIP lifecycle commands composed with process-local replay receipts.
//! Status uses the original admitted calls; the caller retains platform owners.
use crate::{
    call_features::CallAccessSettings,
    native_config::NativeMicroSettings,
    native_pipe::{NativeRuntime, NativeShutdown, NativeShutdownError},
    native_registration::registration_json,
    native_rpc::{NativeHandler, NativeInvocation, NativeOutcome, NativeRequest},
    native_sip::{NativeSipError, NativeSipOperation, NativeSipOperations, retained},
    native_sip_status::{NativeSipViews, call_json, status_json},
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

/// Lifecycle and status compose the original SIP/media/feature handles. The
/// remaining runtime supplies platform commands and its required Micro/realtime
/// observations; it cannot replace the SIP call projection.
pub struct NativeSipCommands {
    owner: NativeSipOperations,
    remaining: Arc<dyn NativeRuntime>,
    setup: Arc<Setup>,
    views: NativeSipViews,
    shutdown: Mutex<Option<NativeSipOperation<()>>>,
}
impl NativeSipCommands {
    pub fn new(
        owner: NativeSipOperations,
        remaining: Arc<dyn NativeRuntime>,
        setup: impl Fn(Option<NativeMicroSettings>) -> NativeMicroSetup + Send + Sync + 'static,
        views: NativeSipViews,
    ) -> Self {
        Self {
            owner,
            remaining,
            setup: Arc::new(setup),
            views,
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
        if matches!(request.method(), "status" | "heartbeat") {
            let fields = request
                .params()
                .as_object()
                .expect("validated native params");
            if fields.iter().any(|(key, _)| key != &"callId".into())
                || request.method() == "heartbeat" && !fields.is_empty()
            {
                return Err(SipHostError::InvalidRequest.into());
            }
            let snapshot = self.owner.status_snapshot(selected.as_deref())?;
            let views = self.views.clone();
            return Ok(Box::pin(async move {
                if let Some(call) = &snapshot.call {
                    if let Err(e) = call.constructed().await {
                        return failed(e.into());
                    }
                }
                match status_json(snapshot, &views) {
                    Ok(json) => NativeOutcome::result(json),
                    Err(e) => failed(e),
                }
            }));
        }
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
        if request.method() == "registration.reconnect" {
            if !fields.is_empty() {
                return Err(SipHostError::InvalidRequest.into());
            }
            let work = self.owner.reconnect_registration()?;
            return Ok(Box::pin(async move {
                match work.wait().await {
                    Ok(observation) => match registration_json(&observation) {
                        Ok(json) => NativeOutcome::result(json),
                        Err(_) => NativeOutcome::error("operation_outcome_unknown"),
                    },
                    Err(error) => failed(error.into()),
                }
            }));
        }
        if request.method() == "call.features" {
            if fields.len() != 2
                || request.params().get("callId").is_none()
                || request.params().get("challenge").is_none()
            {
                return Err(SipHostError::InvalidRequest.into());
            }
            let call = selected
                .and_then(|id| self.owner.find(&id))
                .ok_or(SipHostError::NotReady)?;
            let raw = crate::native_rpc::root_values(request.raw_params());
            let index = fields
                .iter()
                .position(|(key, _)| key == &"challenge".into())
                .expect("exact challenge");
            let challenge =
                CallAccessSettings::parse(request.params().get("challenge").unwrap(), raw[index])?;
            let snapshot = self.owner.configure_features(&call, challenge)?.json();
            return Ok(Box::pin(async move { NativeOutcome::result(snapshot) }));
        }
        if request.method() == "call.dial" {
            let input = crate::native_dial::NativeDial::parse(&request)?;
            let call = selected
                .and_then(|id| self.owner.find(&id))
                .ok_or(SipHostError::NotReady)?;
            let work = self.owner.dial(&call, input)?;
            return Ok(Box::pin(async move {
                match work.wait().await {
                    Ok(json) => NativeOutcome::result(json),
                    Err(error) => failed(error.into()),
                }
            }));
        }
        if fields.len() != 1 || request.params().get("callId").is_none() {
            return Err(SipHostError::InvalidRequest.into());
        }
        if request.method() == "call.prepare" {
            let id = selected.ok_or(SipHostError::InvalidRequest)?;
            // Reserve synchronously under NativeDispatcher admission, before
            // returning the observer-independent construction future.
            let call = self.owner.prepare(id)?;
            return Ok(Box::pin(async move {
                match call.prepared().await {
                    Ok(()) => match call_json(&call) {
                        Ok(value) => NativeOutcome::result(value),
                        Err(error) => failed(error),
                    },
                    Err(error) => failed(error.into()),
                }
            }));
        }
        let call = selected.and_then(|id| self.owner.find(&id)).ok_or(
            if request.method() == "call.claim" {
                NativeSipError::OfferExpired
            } else {
                SipHostError::NotReady.into()
            },
        )?;
        if request.method() == "call.claim" {
            self.owner.claim(&call)?;
            let snapshot = call_json(&call)?;
            return Ok(Box::pin(async move { NativeOutcome::result(snapshot) }));
        }
        if request.method() == "call.hangup" {
            self.owner.hangup(&call)?;
            let outcome = match call_json(&call) {
                Ok(value) => NativeOutcome::result(value),
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
            "configure"
                | "status"
                | "heartbeat"
                | "call.prepare"
                | "call.dial"
                | "call.claim"
                | "call.hangup"
                | "call.release"
                | "registration.reconnect"
                | "call.features"
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
