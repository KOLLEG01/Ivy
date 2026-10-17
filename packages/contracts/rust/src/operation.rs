use crate::{IvyError, IvyJson, IvyResult, json::canonical, limits::MANAGEMENT_FRAME_BYTES};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationIdentity {
    pub runtime_epoch: String,
    pub issued_at_unix_ms: u64,
    pub nonce: String,
}
pub fn operation_id(epoch: &str, issued_at_unix_ms: u64, nonce: &str) -> IvyResult<String> {
    if epoch.is_empty()
        || epoch.contains(':')
        || issued_at_unix_ms > 9_007_199_254_740_991
        || nonce.is_empty()
        || nonce.encode_utf16().count() > 128
        || nonce.contains(':')
    {
        return Err(IvyError::new(
            "invalid_arguments",
            "Invalid operation identity fields.",
        ));
    }
    Ok(format!("{epoch}:{issued_at_unix_ms}:{nonce}"))
}
pub fn parse_operation_id(value: &str) -> IvyResult<OperationIdentity> {
    let p: Vec<_> = value.split(':').collect();
    if p.len() != 3
        || p[0].is_empty()
        || p[1].is_empty()
        || p[1].len() > 16
        || !p[1].bytes().all(|b| b.is_ascii_digit())
        || p[2].is_empty()
        || p[2].encode_utf16().count() > 128
    {
        return Err(IvyError::new(
            "invalid_arguments",
            "Operation identity must be <runtimeEpoch>:<issuedAtUnixMs>:<nonce>.",
        ));
    }
    let time = p[1].parse::<u64>().unwrap();
    if time > 9_007_199_254_740_991 {
        return Err(IvyError::new(
            "invalid_arguments",
            "Operation identity has an invalid issue time.",
        ));
    }
    Ok(OperationIdentity {
        runtime_epoch: p[0].into(),
        issued_at_unix_ms: time,
        nonce: p[2].into(),
    })
}
pub fn derive_operation_id(parent: &str, scope: &IvyJson) -> IvyResult<String> {
    let identity = parse_operation_id(parent)?;
    let text = canonical(
        &IvyJson::object([("nonce", identity.nonce.into()), ("scope", scope.clone())]),
        MANAGEMENT_FRAME_BYTES,
    )?;
    let (mut first, mut second) = (2166136261_u32, 2246822519_u32);
    for code in text.encode_utf16() {
        first = (first ^ u32::from(code)).wrapping_mul(16777619);
        second = (second ^ u32::from(code)).wrapping_mul(3266489917);
    }
    operation_id(
        &identity.runtime_epoch,
        identity.issued_at_unix_ms,
        &format!("{first:08x}{second:08x}"),
    )
}
