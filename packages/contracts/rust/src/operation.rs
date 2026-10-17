use crate::{IvyError, IvyJson, IvyResult, json::canonical, limits::MANAGEMENT_FRAME_BYTES};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsOperationIdentity {
    pub runtime_epoch: crate::JsString,
    pub issued_at_unix_ms: u64,
    pub nonce: crate::JsString,
}
pub fn operation_id_js(
    epoch: &crate::JsString,
    issued_at_unix_ms: u64,
    nonce: &crate::JsString,
) -> IvyResult<crate::JsString> {
    if epoch.0.is_empty()
        || epoch.0.contains(&58)
        || issued_at_unix_ms > 9_007_199_254_740_991
        || nonce.0.is_empty()
        || nonce.0.len() > 128
        || nonce.0.contains(&58)
    {
        return Err(IvyError::new(
            "invalid_arguments",
            "Invalid operation identity fields.",
        ));
    }
    let mut units = epoch.0.clone();
    units.push(58);
    units.extend(issued_at_unix_ms.to_string().encode_utf16());
    units.push(58);
    units.extend_from_slice(&nonce.0);
    Ok(crate::JsString(units))
}
pub fn parse_operation_id_js(value: &crate::JsString) -> IvyResult<JsOperationIdentity> {
    let parts: Vec<_> = value.0.split(|u| *u == 58).collect();
    if parts.len() != 3
        || parts[0].is_empty()
        || parts[1].is_empty()
        || parts[1].len() > 16
        || !parts[1].iter().all(|u| (48..=57).contains(u))
        || parts[2].is_empty()
        || parts[2].len() > 128
    {
        return Err(IvyError::new(
            "invalid_arguments",
            "Operation identity must be <runtimeEpoch>:<issuedAtUnixMs>:<nonce>.",
        ));
    }
    let time = parts[1]
        .iter()
        .fold(0u64, |value, u| value * 10 + u64::from(*u - 48));
    if time > 9_007_199_254_740_991 {
        return Err(IvyError::new(
            "invalid_arguments",
            "Operation identity has an invalid issue time.",
        ));
    }
    Ok(JsOperationIdentity {
        runtime_epoch: crate::JsString(parts[0].to_vec()),
        issued_at_unix_ms: time,
        nonce: crate::JsString(parts[2].to_vec()),
    })
}
pub fn derive_operation_id_js(
    parent: &crate::JsString,
    scope: &IvyJson,
) -> IvyResult<crate::JsString> {
    let identity = parse_operation_id_js(parent)?;
    let text = canonical(
        &IvyJson::object([
            ("nonce", IvyJson::String(identity.nonce)),
            ("scope", scope.clone()),
        ]),
        MANAGEMENT_FRAME_BYTES,
    )?;
    let (mut first, mut second) = (2166136261_u32, 2246822519_u32);
    for code in text.encode_utf16() {
        first = (first ^ u32::from(code)).wrapping_mul(16777619);
        second = (second ^ u32::from(code)).wrapping_mul(3266489917);
    }
    operation_id_js(
        &identity.runtime_epoch,
        identity.issued_at_unix_ms,
        &format!("{first:08x}{second:08x}").into(),
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationIdentity {
    pub runtime_epoch: String,
    pub issued_at_unix_ms: u64,
    pub nonce: String,
}
pub fn operation_id(epoch: &str, issued_at_unix_ms: u64, nonce: &str) -> IvyResult<String> {
    operation_id_js(&epoch.into(), issued_at_unix_ms, &nonce.into())?.to_utf8()
}
pub fn parse_operation_id(value: &str) -> IvyResult<OperationIdentity> {
    let identity = parse_operation_id_js(&value.into())?;
    Ok(OperationIdentity {
        runtime_epoch: identity.runtime_epoch.to_utf8()?,
        issued_at_unix_ms: identity.issued_at_unix_ms,
        nonce: identity.nonce.to_utf8()?,
    })
}
pub fn derive_operation_id(parent: &str, scope: &IvyJson) -> IvyResult<String> {
    derive_operation_id_js(&parent.into(), scope)?.to_utf8()
}
