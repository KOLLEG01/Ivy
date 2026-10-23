//! Original-owner evidence checks; domain permissions remain with the caller.
use crate::{IvyError, IvyJson, IvyResult, JsString};
use ivy_contracts::{
    agent_schema::validate_agent, hash::hash_json, json::canonical, limits::MANAGEMENT_FRAME_BYTES,
    validation::equal,
};

#[derive(Clone, Debug)]
pub struct NativeOwnerIdentity {
    pub service_node_id: JsString,
    pub host_id: Option<JsString>,
    pub native_version: JsString,
    pub native_executable_hash: JsString,
    pub catalog_hash: JsString,
}
#[derive(Clone, Debug)]
pub struct NativeOperationIdentity {
    pub owner: NativeOwnerIdentity,
    pub caller_principal_id: JsString,
    pub operation_id: JsString,
    pub method: JsString,
    pub params: IvyJson,
}
#[derive(Clone, Debug)]
pub struct NativeReadIdentity {
    pub owner: NativeOwnerIdentity,
    pub caller_principal_id: JsString,
    pub epoch: JsString,
    pub method: JsString,
    pub params: IvyJson,
}
fn require(value: bool, code: &str, message: &str) -> IvyResult<()> {
    if value {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn text(value: &IvyJson, key: &str, expected: &JsString) -> bool {
    value.get(key).and_then(IvyJson::as_js_string) == Some(expected)
}
fn bytes(value: &IvyJson) -> IvyResult<String> {
    canonical(value, MANAGEMENT_FRAME_BYTES)
}
fn field<'a>(value: &'a IvyJson, key: &str) -> &'a IvyJson {
    value
        .get(key)
        .expect("structurally validated native evidence")
}

#[derive(Clone, Copy)]
pub struct NativePayloadLimits {
    pub request_bytes: usize,
    pub response_bytes: usize,
}
/// Check the original transport envelope before evaluating native parameters,
/// then the reply envelope before its result. This does not validate authority,
/// receipt identity or reply-error semantics.
pub fn validate_native_payload(
    contract: &ivy_contracts::native_contract::NativeContract,
    observation: &IvyJson,
    limits: NativePayloadLimits,
) -> IvyResult<()> {
    let method = observation.get("method").and_then(IvyJson::as_js_string)
        .ok_or_else(|| IvyError::new("native_method_unsupported", "The method is absent from the exact selected native catalog."))?;
    let params = observation.required("params")?;
    let fallback = IvyJson::from("00000000-0000-0000-0000-000000000000");
    let request_id = observation.get("requestId").filter(|id| !id.is_null()).unwrap_or(&fallback);
    let request = IvyJson::object([
        ("id", request_id.clone()), ("method", IvyJson::String(method.clone())), ("params", params.clone()),
    ]);
    canonical(&request, limits.request_bytes)?;
    contract.validate_input(method, params, limits.request_bytes)?;
    if let Some(reply) = observation.get("reply").filter(|reply| !reply.is_null()) {
        if !matches!(reply, IvyJson::Object(_)) {
            return Err(IvyError::new("invalid_frame", "Native reply must be an object."));
        }
        let mut response = reply.clone();
        if response.get("id").is_none() {
            if let Some(id) = observation.get("requestId") { response.insert("id", id.clone()); }
        }
        canonical(&response, limits.response_bytes)?;
        if let Some(result) = reply.get("result") { contract.validate_result(method, result, limits.response_bytes)?; }
    }
    Ok(())
}

/// Read a canonical UTC protocol timestamp with exactly three fractional digits.
/// Includes signed six-digit years and the full supported timestamp range.
pub fn native_instant(value: &JsString, code: Option<&str>) -> IvyResult<i64> {
    let parsed = (|| {
        let value = value.to_utf8().ok()?;
        let b = value.as_bytes();
        let (year, offset) = match b.len() {
            24 => (digits(&b[..4])?, 4),
            27 if matches!(b[0], b'+' | b'-') => {
                let magnitude = digits(&b[1..7])?;
                if (b[0] == b'+' && magnitude < 10_000) || (b[0] == b'-' && magnitude == 0) {
                    return None;
                }
                (if b[0] == b'-' { -magnitude } else { magnitude }, 7)
            }
            _ => return None,
        };
        let b = &b[offset..];
        if b[0] != b'-'
            || b[3] != b'-'
            || b[6] != b'T'
            || b[9] != b':'
            || b[12] != b':'
            || b[15] != b'.'
            || b[19] != b'Z'
        {
            return None;
        }
        let month = digits(&b[1..3])?;
        let day = digits(&b[4..6])?;
        let hour = digits(&b[7..9])?;
        let minute = digits(&b[10..12])?;
        let second = digits(&b[13..15])?;
        let millisecond = digits(&b[16..19])?;
        let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
        let days = match month {
            2 => {
                if leap {
                    29
                } else {
                    28
                }
            }
            4 | 6 | 9 | 11 => 30,
            1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
            _ => return None,
        };
        if day < 1 || day > days || hour > 23 || minute > 59 || second > 59 {
            return None;
        }
        let adjusted = year - i64::from(month <= 2);
        let era = adjusted.div_euclid(400);
        let y = adjusted - era * 400;
        let m = month + if month > 2 { -3 } else { 9 };
        let days = era * 146097 + y * 365 + y / 4 - y / 100 + (153 * m + 2) / 5 + day - 1 - 719468;
        let at =
            days * 86_400_000 + hour * 3_600_000 + minute * 60_000 + second * 1000 + millisecond;
        (at.abs() <= 8_640_000_000_000_000).then_some(at)
    })();
    parsed.ok_or_else(|| {
        IvyError::new(
            code.unwrap_or("native_evidence_mismatch"),
            "Native evidence needs its original valid timestamp.",
        )
    })
}
fn digits(value: &[u8]) -> Option<i64> {
    value.iter().try_fold(0i64, |n, b| {
        b.is_ascii_digit().then(|| n * 10 + i64::from(b - b'0'))
    })
}
fn instant(value: &IvyJson, key: &str, code: &str) -> IvyResult<i64> {
    native_instant(
        field(value, key)
            .as_js_string()
            .expect("validated timestamp"),
        Some(code),
    )
}

pub fn validate_native_status(
    value: &IvyJson,
    target: &NativeOwnerIdentity,
    code: Option<&str>,
) -> IvyResult<()> {
    validate_agent("Status", value)?;
    let code = code.unwrap_or("native_owner_mismatch");
    require(
        text(value, "serviceNodeId", &target.service_node_id)
            && target
                .host_id
                .as_ref()
                .is_none_or(|host| text(value, "hostId", host))
            && equal(value.get("state"), "ready")
            && !field(value, "epoch").is_null()
            && text(value, "nativeVersion", &target.native_version)
            && text(
                value,
                "nativeExecutableHash",
                &target.native_executable_hash,
            )
            && text(value, "catalogHash", &target.catalog_hash),
        code,
        "Native readiness requires the exact selected owner, executable and catalog.",
    )?;
    instant(value, "observedAt", code)?;
    Ok(())
}
fn request_hash(method: &JsString, params: &IvyJson) -> IvyResult<String> {
    hash_json(&IvyJson::object([
        ("method", IvyJson::String(method.clone())),
        ("params", params.clone()),
    ]))
}
pub fn validate_native_operation(
    value: &IvyJson,
    expected: &NativeOperationIdentity,
    code: Option<&str>,
) -> IvyResult<()> {
    validate_agent("Operation", value)?;
    let code = code.unwrap_or("native_evidence_mismatch");
    require(
        text(value, "operationId", &expected.operation_id)
            && text(value, "callerPrincipalId", &expected.caller_principal_id)
            && text(value, "serviceNodeId", &expected.owner.service_node_id)
            && text(value, "nativeVersion", &expected.owner.native_version)
            && text(
                value,
                "nativeExecutableHash",
                &expected.owner.native_executable_hash,
            )
            && text(value, "method", &expected.method)
            && bytes(field(value, "params"))? == bytes(&expected.params)?
            && equal(
                value.get("requestHash"),
                &request_hash(&expected.method, &expected.params)?,
            ),
        code,
        "Native evidence must identify the exact original caller, owner, executable, operation and request.",
    )?;
    require(
        instant(value, "updatedAt", code)? >= instant(value, "createdAt", code)?,
        code,
        "An original operation cannot finish before it was created.",
    )
}
pub fn validate_native_read(
    value: &IvyJson,
    expected: &NativeReadIdentity,
    code: Option<&str>,
) -> IvyResult<()> {
    validate_agent("ReadObservation", value)?;
    let code = code.unwrap_or("native_evidence_mismatch");
    require(
        text(value, "serviceNodeId", &expected.owner.service_node_id)
            && text(value, "callerPrincipalId", &expected.caller_principal_id)
            && text(value, "nativeVersion", &expected.owner.native_version)
            && text(
                value,
                "nativeExecutableHash",
                &expected.owner.native_executable_hash,
            )
            && text(value, "catalogHash", &expected.owner.catalog_hash)
            && text(value, "epoch", &expected.epoch)
            && text(value, "method", &expected.method)
            && bytes(field(value, "params"))? == bytes(&expected.params)?
            && equal(
                value.get("requestHash"),
                &request_hash(&expected.method, &expected.params)?,
            ),
        code,
        "Native reads must identify the exact original owner, caller, catalog, epoch and request.",
    )?;
    instant(value, "observedAt", code)?;
    Ok(())
}
pub fn native_operation_terminal(value: &IvyJson) -> bool {
    equal(value.get("phase"), "succeeded") || equal(value.get("phase"), "failed")
}
/// Timestamps are from the operation owner, never the caller's clock.
pub fn validate_native_progress(
    previous: &IvyJson,
    next: &IvyJson,
    code: Option<&str>,
) -> IvyResult<()> {
    validate_agent("Operation", previous)?;
    validate_agent("Operation", next)?;
    let code = code.unwrap_or("native_evidence_regressed");
    fn identity(value: &IvyJson) -> IvyJson {
        IvyJson::object(
            [
                "operationId",
                "callerPrincipalId",
                "serviceNodeId",
                "nativeVersion",
                "nativeExecutableHash",
                "method",
                "params",
                "requestHash",
                "createdAt",
            ]
            .map(|key| (key, field(value, key).clone())),
        )
    }
    fn rank(value: &IvyJson) -> usize {
        ["accepted", "dispatched", "outcome_unknown"]
            .iter()
            .position(|phase| equal(value.get("phase"), phase))
            .unwrap_or(3)
    }
    require(
        bytes(&identity(previous))? == bytes(&identity(next))?
            && instant(next, "updatedAt", code)? >= instant(previous, "updatedAt", code)?
            && rank(next) >= rank(previous)
            && (field(previous, "epoch").is_null() || previous.get("epoch") == next.get("epoch"))
            && (field(previous, "requestId").is_null()
                || previous.get("requestId") == next.get("requestId"))
            && (!native_operation_terminal(previous) || bytes(previous)? == bytes(next)?),
        code,
        "The original operation cannot change identity, regress or replace its terminal receipt.",
    )
}
