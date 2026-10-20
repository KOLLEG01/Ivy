//! Original registration DTO over the retained SIP lease observation.
use crate::sip::{
    RegistrationObservation, RegistrationPeer, RegistrationPhase, RegistrationTransport,
};
use ivy_contracts::{IvyJson, json::stringify};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegistrationViewError {
    InvalidExpiry,
    InvalidTimestamp,
    UnavailableKeepaliveError,
    Encoding,
}

/// Preserve integer lexemes, nullable fields, endpoint protocol and UTC offset.
/// The lease currently retains millisecond time and a coarse keepalive failure;
/// this cannot recover the original socket-error label or submillisecond ticks.
pub fn registration_json(view: &RegistrationObservation) -> Result<String, RegistrationViewError> {
    if view.keepalive_failed {
        return Err(RegistrationViewError::UnavailableKeepaliveError);
    }
    let state = match view.phase {
        RegistrationPhase::Registering => "registering",
        RegistrationPhase::Registered => "registered",
        RegistrationPhase::Failed => "failed",
        RegistrationPhase::Stopped => "stopped",
    };
    let expiry = match view.granted_expiry_seconds {
        Some(n) if n <= i64::MAX as u64 => n.to_string(),
        Some(_) => return Err(RegistrationViewError::InvalidExpiry),
        None => "null".into(),
    };
    let response = view
        .response_code
        .map(|n| n.to_string())
        .unwrap_or_else(|| "null".into());
    let removed = view
        .remote_removed
        .map(|v| v.to_string())
        .unwrap_or_else(|| "null".into());
    let local = endpoint(view.local)?;
    let remote = endpoint(view.remote)?;
    let keepalive = match view.last_keepalive_at {
        Some(ms) => quote(&utc_millis(ms)?)?,
        None => "null".into(),
    };
    Ok(format!(
        "{{\"state\":\"{state}\",\"responseCode\":{response},\"remoteRemoved\":{removed},\"localEndpoint\":{local},\"remoteEndpoint\":{remote},\"grantedExpirySeconds\":{expiry},\"lastKeepAliveAt\":{keepalive},\"lastKeepAliveError\":null}}"
    ))
}
fn quote(value: &str) -> Result<String, RegistrationViewError> {
    stringify(&IvyJson::String(value.into()), crate::ipc::FRAME_BYTES)
        .map_err(|_| RegistrationViewError::Encoding)
}
fn endpoint(peer: Option<RegistrationPeer>) -> Result<String, RegistrationViewError> {
    let Some(peer) = peer else {
        return Ok("null".into());
    };
    let protocol = match peer.transport {
        RegistrationTransport::Udp => "udp",
        RegistrationTransport::Tcp => "tcp",
        RegistrationTransport::Tls => "tls",
    };
    let address = match peer.address {
        std::net::SocketAddr::V6(v6) => v6
            .ip()
            .to_ipv4_mapped()
            .map(|ip| std::net::SocketAddr::new(ip.into(), v6.port()))
            .unwrap_or(peer.address),
        address => address,
    };
    quote(&format!("{protocol}:{address}"))
}
fn utc_millis(ms: i64) -> Result<String, RegistrationViewError> {
    // DateTimeOffset.FromUnixTimeMilliseconds range, including negative epochs.
    if !(-62_135_596_800_000..=253_402_300_799_999).contains(&ms) {
        return Err(RegistrationViewError::InvalidTimestamp);
    }
    // Gregorian civil date from an epoch day; division is Euclidean for dates
    // before 1970. All intermediates are bounded by the DateTimeOffset range.
    let z = ms.div_euclid(86_400_000) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    let year = year + if month <= 2 { 1 } else { 0 };
    let time = ms.rem_euclid(86_400_000);
    let hour = time / 3_600_000;
    let minute = time / 60_000 % 60;
    let second = time / 1000 % 60;
    let fraction = if ms.rem_euclid(1000) == 0 {
        String::new()
    } else {
        format!(".{:03}", ms.rem_euclid(1000))
            .trim_end_matches('0')
            .into()
    };
    Ok(format!(
        "{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}{fraction}+00:00"
    ))
}
