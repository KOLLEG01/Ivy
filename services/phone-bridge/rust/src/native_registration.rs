//! Original registration DTO over the retained SIP lease observation.
use crate::sip::{
    RegistrationKeepaliveError, RegistrationObservation, RegistrationPeer, RegistrationPhase,
    RegistrationTransport,
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
/// Socket errors and time are retained by the original registration owner.
pub fn registration_json(view: &RegistrationObservation) -> Result<String, RegistrationViewError> {
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
    let keepalive = match view.last_keepalive_utc_ticks {
        Some(ticks) => quote(&utc_ticks(ticks)?)?,
        None => "null".into(),
    };
    let error = match view.last_keepalive_error {
        None => "null".into(),
        Some(RegistrationKeepaliveError::SendFailed) => quote("keepalive_send_failed")?,
        Some(RegistrationKeepaliveError::SocketError(code)) => quote(&socket_error(code))?,
        Some(RegistrationKeepaliveError::UnmappedSocketError(_)) => {
            return Err(RegistrationViewError::UnavailableKeepaliveError);
        }
    };
    Ok(format!(
        "{{\"state\":\"{state}\",\"responseCode\":{response},\"remoteRemoved\":{removed},\"localEndpoint\":{local},\"remoteEndpoint\":{remote},\"grantedExpirySeconds\":{expiry},\"lastKeepAliveAt\":{keepalive},\"lastKeepAliveError\":{error}}}"
    ))
}
fn socket_error(code: i32) -> String {
    // Public System.Net.Sockets.SocketError contract. Unknown enum values
    // stringify as their invariant decimal integer, never as OS error text.
    match code {
        0 => "Success",
        -1 => "SocketError",
        995 => "OperationAborted",
        997 => "IOPending",
        10004 => "Interrupted",
        10013 => "AccessDenied",
        10014 => "Fault",
        10022 => "InvalidArgument",
        10024 => "TooManyOpenSockets",
        10035 => "WouldBlock",
        10036 => "InProgress",
        10037 => "AlreadyInProgress",
        10038 => "NotSocket",
        10039 => "DestinationAddressRequired",
        10040 => "MessageSize",
        10041 => "ProtocolType",
        10042 => "ProtocolOption",
        10043 => "ProtocolNotSupported",
        10044 => "SocketNotSupported",
        10045 => "OperationNotSupported",
        10046 => "ProtocolFamilyNotSupported",
        10047 => "AddressFamilyNotSupported",
        10048 => "AddressAlreadyInUse",
        10049 => "AddressNotAvailable",
        10050 => "NetworkDown",
        10051 => "NetworkUnreachable",
        10052 => "NetworkReset",
        10053 => "ConnectionAborted",
        10054 => "ConnectionReset",
        10055 => "NoBufferSpaceAvailable",
        10056 => "IsConnected",
        10057 => "NotConnected",
        10058 => "Shutdown",
        10060 => "TimedOut",
        10061 => "ConnectionRefused",
        10064 => "HostDown",
        10065 => "HostUnreachable",
        10067 => "ProcessLimit",
        10091 => "SystemNotReady",
        10092 => "VersionNotSupported",
        10093 => "NotInitialized",
        10101 => "Disconnecting",
        10109 => "TypeNotFound",
        11001 => "HostNotFound",
        11002 => "TryAgain",
        11003 => "NoRecovery",
        11004 => "NoData",
        _ => return code.to_string(),
    }
    .into()
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
pub(crate) fn utc_time(at: std::time::SystemTime) -> Result<String, RegistrationViewError> {
    let nanos = match at.duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => {
            i128::try_from(d.as_nanos()).map_err(|_| RegistrationViewError::InvalidTimestamp)?
        }
        Err(e) => -i128::try_from(e.duration().as_nanos())
            .map_err(|_| RegistrationViewError::InvalidTimestamp)?,
    };
    let ticks = i64::try_from(621_355_968_000_000_000_i128 + nanos / 100)
        .map_err(|_| RegistrationViewError::InvalidTimestamp)?;
    utc_ticks(ticks)
}
pub(crate) fn utc_ticks(ticks: i64) -> Result<String, RegistrationViewError> {
    if !(0..=3_155_378_975_999_999_999).contains(&ticks) {
        return Err(RegistrationViewError::InvalidTimestamp);
    }
    const SECOND: i64 = 10_000_000;
    let seconds = ticks / SECOND - 62_135_596_800;
    // Gregorian civil date from an epoch day; division is Euclidean for dates
    // before 1970. All intermediates are bounded by the DateTimeOffset range.
    let z = seconds.div_euclid(86_400) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    let year = year + if month <= 2 { 1 } else { 0 };
    let time = seconds.rem_euclid(86_400);
    let hour = time / 3_600;
    let minute = time / 60 % 60;
    let second = time % 60;
    let fraction = if ticks % SECOND == 0 {
        String::new()
    } else {
        format!(".{:07}", ticks % SECOND)
            .trim_end_matches('0')
            .into()
    };
    Ok(format!(
        "{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}{fraction}+00:00"
    ))
}
