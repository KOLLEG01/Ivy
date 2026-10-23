//! Native status assembled from the selected original call, never a later ID lookup.
use crate::{
    ipc::FRAME_BYTES,
    media::native_view,
    native_registration::{registration_json, utc_time},
    native_sip::{NativeSipCall, NativeSipError, NativeSipStatusSnapshot},
    sip::{SipHostCallPhase, SipHostError, SipSignallingObservation},
};
use ivy_contracts::json::{parse, stringify};
use std::sync::Arc;
type Micro = dyn Fn() -> Result<String, NativeSipError> + Send + Sync;
type Realtime = dyn Fn(&NativeSipCall) -> Result<String, NativeSipError> + Send + Sync;
/// Both observers are required from the original caller's platform owner. A
/// raw JSON value preserves native integer lexemes. Returning null asserts no
/// attachment; errors must not be turned into invented unavailable/ready data.
#[derive(Clone)]
pub struct NativeSipViews {
    micro: Arc<Micro>,
    realtime: Arc<Realtime>,
}
impl NativeSipViews {
    pub fn new(
        micro: impl Fn() -> Result<String, NativeSipError> + Send + Sync + 'static,
        realtime: impl Fn(&NativeSipCall) -> Result<String, NativeSipError> + Send + Sync + 'static,
    ) -> Self {
        Self {
            micro: Arc::new(micro),
            realtime: Arc::new(realtime),
        }
    }
}
fn unknown() -> NativeSipError {
    SipHostError::CleanupUnconfirmed.into()
}
fn quote(s: &str) -> Result<String, NativeSipError> {
    stringify(&s.into(), FRAME_BYTES).map_err(|_| unknown())
}
fn optional(s: Option<&str>) -> Result<String, NativeSipError> {
    s.map(quote).unwrap_or(Ok("null".into()))
}
fn caller_value(raw: String) -> Result<String, NativeSipError> {
    let v = parse(&raw, FRAME_BYTES).map_err(|_| unknown())?;
    if !v.is_null() && v.as_object().is_none() {
        return Err(unknown());
    }
    Ok(raw)
}
pub(crate) fn micro_json(
    status: Option<&crate::micro::MicroAttachmentStatus>,
) -> Result<String, NativeSipError> {
    let Some(status) = status else {
        return Ok("null".into());
    };
    // The native record carries Int64. Keep the integer token exact even when
    // a supplied observation lies outside JavaScript's safe integer range.
    Ok(format!(
        "{{\"state\":{},\"generation\":{},\"errorCode\":{}}}",
        quote(&status.state)?,
        status
            .generation
            .map_or_else(|| "null".into(), |n| n.to_string()),
        optional(status.error_code.as_deref())?,
    ))
}
pub fn call_json(call: &NativeSipCall) -> Result<String, NativeSipError> {
    let s = call.observation();
    let state = match s.phase {
        SipHostCallPhase::Prepared => "prepared",
        SipHostCallPhase::Ringing => "ringing",
        SipHostCallPhase::Dialing => "dialing",
        SipHostCallPhase::Connected => "connected",
        SipHostCallPhase::LocalEnded => "local_ended",
        SipHostCallPhase::OutcomeUnknown => "outcome_unknown",
    };
    let error = call.error();
    Ok(format!(
        "{{\"id\":{},\"direction\":{},\"state\":{},\"sipCallId\":{},\"error\":{},\"incoming\":{},\"features\":{}}}",
        quote(&s.id)?,
        quote(if call.incoming().is_some() {
            "incoming"
        } else {
            "outgoing"
        })?,
        quote(state)?,
        quote(call.sip_call_id())?,
        optional(error)?,
        call.incoming().map_or("null".into(), |i| i.json()),
        call.features().observation().json()
    ))
}
pub fn signalling_json(s: &SipSignallingObservation) -> Result<String, NativeSipError> {
    let events = s
        .events
        .iter()
        .map(|e| {
            Ok(format!(
                "{{\"at\":{},\"direction\":{},\"method\":{},\"status\":{}}}",
                quote(&utc_time(e.at).map_err(|_| unknown())?)?,
                quote(e.direction)?,
                quote(&e.method)?,
                e.status.map_or_else(|| "null".into(), |v| v.to_string())
            ))
        })
        .collect::<Result<Vec<_>, NativeSipError>>()?
        .join(",");
    Ok(format!(
        "{{\"sent\":{},\"received\":{},\"events\":[{}]}}",
        s.sent, s.received, events
    ))
}
pub(crate) fn status_json(
    s: NativeSipStatusSnapshot,
    views: &NativeSipViews,
) -> Result<String, NativeSipError> {
    let call = s
        .call
        .as_ref()
        .map(call_json)
        .unwrap_or(Ok("null".into()))?;
    let registration = s
        .registration
        .as_ref()
        .map(registration_json)
        .transpose()
        .map_err(|_| unknown())?
        .unwrap_or_else(|| "null".into());
    let micro = caller_value((views.micro)()?)?;
    let ids = s
        .call_ids
        .iter()
        .map(|id| quote(id))
        .collect::<Result<Vec<_>, _>>()?
        .join(",");
    let endpoint = optional(s.endpoint.as_ref().map(ToString::to_string).as_deref())?;
    let media = if let Some(call) = &s.call {
        let m = call.media().ok_or_else(unknown)?.observation();
        let realtime = caller_value(
            match call
                .media()
                .ok_or_else(unknown)?
                .call_audio()
                .realtime_observation()
                .map_err(|_| unknown())?
            {
                Some(value) => value,
                None => (views.realtime)(call)?,
            },
        )?;
        format!(
            "{{\"closed\":{},\"failed\":{},\"sentPackets\":{},\"receivedPackets\":{},\"receive\":{},\"audio\":{},\"diagnostics\":{},\"signalling\":{}}}",
            m.closed,
            m.failed,
            m.sent_frames as i64,
            m.receive.received_packets as i64,
            native_view::receive_json(&m.receive.receive).map_err(|_| unknown())?,
            native_view::audio_json(&m.audio).map_err(|_| unknown())?,
            native_view::diagnostics_raw_json(&m, &realtime).map_err(|_| unknown())?,
            signalling_json(&call.signalling().ok_or_else(unknown)?)?
        )
    } else {
        "null".into()
    };
    Ok(format!(
        "{{\"configured\":{},\"call\":{},\"registration\":{},\"micro\":{},\"callIds\":[{}],\"endpoint\":{},\"media\":{}}}",
        s.configured, call, registration, micro, ids, endpoint, media
    ))
}
