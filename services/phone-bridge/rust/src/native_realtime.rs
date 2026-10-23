//! Original digital-audio DTO/admission prefix. The call port retains every
//! asynchronous route action; NativeRpc continues to own duplicate receipts.
use super::*;
use crate::sip::SipHostCallPhase;

pub(super) fn method(method: &str) -> bool {
    matches!(
        method,
        "call.realtime.prepare" | "call.realtime.answer" | "call.realtime.stop"
    )
}
impl NativeSipCommands {
    pub(super) fn realtime(
        &self,
        request: &NativeRequest,
        selected: Option<String>,
    ) -> Result<NativeInvocation, NativeSipError> {
        let params = request.params();
        let prepare = request.method() == "call.realtime.prepare";
        let answer = request.method() == "call.realtime.answer";
        let optional = prepare && params.get("playbackPrebufferMs").is_some();
        let names: &[&str] = if optional {
            &["callId", "generation", "queueMs", "playbackPrebufferMs"]
        } else if prepare {
            &["callId", "generation", "queueMs"]
        } else if answer {
            &["callId", "generation", "sdp"]
        } else {
            &["callId", "generation"]
        };
        if params.as_object().unwrap().len() != names.len()
            || names.iter().any(|name| params.get(name).is_none())
        {
            return Err(NativeConfigError::InvalidRequest.into());
        }
        let integer = |name: &str| {
            crate::native_desktop::raw_field(params, request.raw_params(), name)
                .parse::<i32>()
                .map_err(|_| NativeSipError::from(NativeConfigError::InvalidRequest))
        };
        // All typed DTO conversion precedes call/connection/access guards.
        let generation = integer("generation")?;
        let call_id = params.get("callId").unwrap();
        if !call_id.is_null() && call_id.as_str().is_err() {
            return Err(NativeConfigError::InvalidRequest.into());
        }
        let queue = if prepare {
            Some(integer("queueMs")?)
        } else {
            None
        };
        let prebuffer = if optional && !params.get("playbackPrebufferMs").unwrap().is_null() {
            Some(integer("playbackPrebufferMs")?)
        } else {
            None
        };
        let sdp = if answer {
            let value = params.get("sdp").unwrap();
            Some(if value.is_null() {
                String::new()
            } else {
                value
                    .as_str()
                    .map_err(|_| NativeConfigError::InvalidRequest)?
            })
        } else {
            None
        };
        let call = selected
            .and_then(|id| self.owner.find(&id))
            .ok_or(SipHostError::NotReady)?;
        if answer
            && (call.observation().phase != SipHostCallPhase::Connected
                || !call.features().authenticated())
        {
            return Err(SipHostError::NotReady.into());
        }
        let port = call
            .media()
            .ok_or(SipHostError::NotReady)?
            .call_audio()
            .clone();
        // Keep these synchronous prefixes inside NativeDispatcher admission.
        // Dropping the returned invocation only abandons its observer.
        let action = if prepare {
            port.prepare_realtime(generation, queue.unwrap(), prebuffer)
                .map(|work| {
                    Box::pin(async move {
                        work.wait().await?;
                        let sdp = port.realtime_offer(generation)?;
                        stringify(
                            &IvyJson::object([
                                ("callId", port.call_id().into()),
                                ("generation", IvyJson::Number(generation as f64)),
                                ("sdp", sdp.into()),
                            ]),
                            crate::ipc::FRAME_BYTES,
                        )
                        .map_err(|_| crate::media::realtime::RealtimeError::PreparationFailed)
                    })
                        as Pin<
                            Box<dyn Future<Output = crate::media::realtime::Result<String>> + Send>,
                        >
                })
        } else if answer {
            port.accept_realtime(generation, sdp.unwrap()).map(|work| {
                Box::pin(async move {
                    work.wait().await?;
                    Ok(format!(
                        "{{\"callId\":\"{}\",\"connected\":true}}",
                        port.call_id()
                    ))
                })
                    as Pin<Box<dyn Future<Output = crate::media::realtime::Result<String>> + Send>>
            })
        } else {
            port.stop_realtime(generation).map(|work| {
                Box::pin(async move {
                    work.wait()
                        .await
                        .map_err(|_| crate::media::realtime::RealtimeError::CleanupUnconfirmed)?;
                    Ok(format!(
                        "{{\"callId\":\"{}\",\"stopped\":true}}",
                        port.call_id()
                    ))
                })
                    as Pin<Box<dyn Future<Output = crate::media::realtime::Result<String>> + Send>>
            })
        };
        Ok(Box::pin(async move {
            match action {
                Ok(action) => match action.await {
                    Ok(value) => NativeOutcome::result(value),
                    Err(error) => NativeOutcome::error(error.code()),
                },
                Err(error) => NativeOutcome::error(error.code()),
            }
        }))
    }
}
