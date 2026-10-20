//! Native media diagnostics projected from the original session owners.
//! The caller supplies the observation of that call's realtime owner; absence
//! means no realtime attachment, not an inferred successful setup/cleanup.
use super::{
    AudioAttachmentStatus, AudioFormat, RtcpReceiveObservation, RtpReceiveStatus,
    SipMediaObservation, TelephoneEventFormat,
};
use ivy_contracts::{IvyJson, json::stringify};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaViewError {
    Encoding,
    InvalidTimestamp,
}
type Result<T> = std::result::Result<T, MediaViewError>;
fn text(value: &str) -> Result<String> {
    stringify(&value.into(), crate::ipc::FRAME_BYTES).map_err(|_| MediaViewError::Encoding)
}
fn optional(value: Option<&str>) -> Result<String> {
    value.map(text).unwrap_or(Ok("null".into()))
}
fn value(value: Option<&IvyJson>) -> Result<String> {
    value
        .map(|v| stringify(v, crate::ipc::FRAME_BYTES).map_err(|_| MediaViewError::Encoding))
        .unwrap_or(Ok("null".into()))
}
fn array<T>(values: &[T], project: impl Fn(&T) -> Result<String>) -> Result<String> {
    Ok(format!(
        "[{}]",
        values
            .iter()
            .map(project)
            .collect::<Result<Vec<_>>>()?
            .join(",")
    ))
}
fn count(value: u64) -> i64 {
    value as i64
}
fn number(value: f64) -> Result<String> {
    if !value.is_finite() {
        return Err(MediaViewError::Encoding);
    }
    stringify(&IvyJson::Number(value), crate::ipc::FRAME_BYTES)
        .map_err(|_| MediaViewError::Encoding)
}
pub fn receive_json(v: &RtpReceiveStatus) -> Result<String> {
    Ok(format!(
        "{{\"queuedPackets\":{},\"maximumPackets\":{},\"reorderMs\":{},\"duplicatePackets\":{},\"latePackets\":{},\"overflowPackets\":{},\"foreignPackets\":{},\"gatedPackets\":{},\"missingPackets\":{},\"maximumResidenceMs\":{}}}",
        v.queued_packets,
        v.maximum_packets,
        v.reorder_ms,
        count(v.duplicate_packets),
        count(v.late_packets),
        count(v.overflow_packets),
        count(v.foreign_packets),
        count(v.gated_packets),
        count(v.missing_packets),
        number(v.maximum_residence_ms)?
    ))
}
pub fn audio_json(v: &AudioAttachmentStatus) -> Result<String> {
    Ok(format!(
        "{{\"callId\":{},\"state\":{},\"route\":{}}}",
        text(&v.call_id)?,
        text(v.state)?,
        value(v.route.as_ref())?
    ))
}
fn format_json(v: &AudioFormat) -> Result<String> {
    Ok(format!(
        "{{\"codec\":{},\"payload\":{},\"clockRate\":{},\"pcmRate\":{},\"channels\":{},\"parameters\":{}}}",
        text(v.name())?,
        v.payload(),
        v.rtp_rate(),
        v.pcm_rate(),
        v.channels(),
        optional(v.parameters())?
    ))
}
fn events_json(v: &[TelephoneEventFormat]) -> Result<String> {
    array(v, |v| {
        Ok(format!(
            "{{\"payload\":{},\"clockRate\":{}}}",
            v.payload, v.clock_rate
        ))
    })
}
fn rtcp_json(v: &RtcpReceiveObservation) -> Result<String> {
    let last = if let Some(at) = v.last_report_at {
        text(
            &crate::native_registration::utc_time(at)
                .map_err(|_| MediaViewError::InvalidTimestamp)?,
        )?
    } else {
        "null".into()
    };
    let reports = array(&v.peer_reports, |r| {
        Ok(format!(
            "{{\"ssrc\":{},\"fractionLost256\":{},\"packetsLost\":{},\"extendedHighestSequence\":{},\"jitterTimestampUnits\":{}}}",
            r.source,
            r.fraction_lost,
            r.packets_lost(),
            r.highest_sequence,
            r.jitter
        ))
    })?;
    Ok(format!(
        "{{\"reportsReceived\":{},\"reportsSent\":{},\"lastReportAt\":{},\"peerReceptionReports\":{}}}",
        count(v.reports_received),
        count(v.reports_sent),
        last,
        reports
    ))
}
/// Raw signed counters and native timestamps never pass through f64/IvyJson.
/// Route/realtime values remain owned, already-safe observations from callers.
pub fn diagnostics_json(v: &SipMediaObservation, realtime: Option<&IvyJson>) -> Result<String> {
    diagnostics_raw_json(v, &value(realtime)?)
}
/// The native caller validates this raw object/null before composition.
pub(crate) fn diagnostics_raw_json(v: &SipMediaObservation, realtime: &str) -> Result<String> {
    let r = &v.receive;
    let send = v
        .send_format
        .as_ref()
        .map(format_json)
        .unwrap_or(Ok("null".into()))?;
    let codec = optional(v.send_format.as_ref().map(AudioFormat::name))?;
    let payload = v
        .send_format
        .as_ref()
        .map_or_else(|| "null".into(), |f| f.payload().to_string());
    let mut offered = r.event_payloads.clone();
    offered.sort_unstable();
    offered.dedup();
    let offered_count = offered.len();
    let offered = offered
        .iter()
        .map(u8::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let inband = if let Some(d) = r.inband {
        format!(
            "{{\"analyzedWindows\":{},\"audibleWindows\":{},\"pairedToneWindows\":{},\"candidateToneWindows\":{},\"maximumPairedToneEnergyRatio\":{}}}",
            d.analyzed_windows,
            d.audible_windows,
            d.paired_tone_windows,
            d.candidate_tone_windows,
            number(d.maximum_paired_tone_energy_ratio)?
        )
    } else {
        "null".into()
    };
    let raw = format!(
        "[{}]",
        r.raw_payloads
            .iter()
            .filter(|(_, packets)| count(*packets) > 0)
            .map(|(payload, packets)| {
                format!(
                    "{{\"payload\":{},\"packets\":{}}}",
                    payload,
                    count(*packets)
                )
            })
            .collect::<Vec<_>>()
            .join(",")
    );
    let dtmf = format!(
        "{{\"offeredEventPayloads\":{},\"offeredEventPayloadIds\":[{}],\"remoteEventFormats\":{},\"localEventFormats\":{},\"negotiatedEventPayloadId\":{},\"rawPayloads\":{},\"inBand\":{},\"rawRtpPackets\":{},\"rawOfferedEventPackets\":{},\"parsedEventPackets\":{},\"libraryEventPackets\":{},\"unsupportedPayloadPackets\":{},\"inBandAnalyzedPackets\":{},\"inBandDetectedDigits\":{},\"rfc4733DetectedDigits\":{}}}",
        offered_count,
        offered,
        events_json(&v.remote_event_formats)?,
        events_json(&v.local_event_formats)?,
        r.selected_event_payload,
        raw,
        inband,
        count(r.raw_rtp_packets),
        count(r.raw_offered_event_packets),
        count(r.parsed_event_packets),
        count(r.library_event_packets),
        count(r.native_unsupported_payload_packets),
        count(r.inband_analyzed_packets),
        count(r.inband_detected_digits),
        count(r.detected_events)
    );
    let result = format!(
        "{{\"sendCodec\":{},\"sendPayload\":{},\"sendFormat\":{},\"receiveFormats\":{},\"rtcp\":{},\"localRtp\":{},\"remoteRtp\":{},\"sentPackets\":{},\"receivedPackets\":{},\"receive\":{},\"audio\":{},\"realtime\":{},\"dtmf\":{}}}",
        codec,
        payload,
        send,
        array(&r.formats, format_json)?,
        rtcp_json(&r.rtcp)?,
        text(&r.transport.local_rtp.to_string())?,
        optional(v.remote_rtp.as_ref().map(|a| a.to_string()).as_deref())?,
        count(v.sent_frames),
        count(r.received_packets),
        receive_json(&r.receive)?,
        audio_json(&v.audio)?,
        realtime,
        dtmf
    );
    if result.len() > crate::ipc::FRAME_BYTES {
        return Err(MediaViewError::Encoding);
    }
    Ok(result)
}
