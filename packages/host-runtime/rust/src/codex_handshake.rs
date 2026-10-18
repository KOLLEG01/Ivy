//! Initialization and identity checks shared by the owned and proxy connections.
use super::codex_rpc::{NativeRpc, RequestOptions};
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString};
use std::path::Path;

fn whitespace(unit: u16) -> bool {
    matches!(unit,0x9..=0xd|0x20|0xa0|0x1680|0x2000..=0x200a|0x2028|0x2029|0x202f|0x205f|0x3000|0xfeff)
}

/// Exact ECMAScript user-agent grammar used by the official daemon handshake.
pub fn initialized_server_version(value: &IvyJson) -> IvyResult<JsString> {
    let unavailable = || {
        IvyError::new(
            "native_server_version_unavailable",
            "The server handshake must identify its actual exact version.",
        )
    };
    let units = value.as_js_string().ok_or_else(unavailable)?.0.as_slice();
    let slash = units
        .iter()
        .position(|c| *c == 47)
        .ok_or_else(unavailable)?;
    if slash == 0 {
        return Err(unavailable());
    }
    for (index, unit) in units[..slash].iter().copied().enumerate() {
        if whitespace(unit)
            && (unit != 32 || index == 0 || index + 1 == slash || whitespace(units[index - 1]))
        {
            return Err(unavailable());
        }
    }
    let start = slash + 1;
    let mut end = start;
    for part in 0..3 {
        let before = end;
        while units.get(end).is_some_and(|c| (48..=57).contains(c)) {
            end += 1;
        }
        if end == before {
            return Err(unavailable());
        }
        if part < 2 {
            if units.get(end) != Some(&46) {
                return Err(unavailable());
            }
            end += 1;
        }
    }
    if units.get(end).is_some_and(|c| !whitespace(*c)) {
        return Err(unavailable());
    }
    Ok(JsString(units[start..end].to_vec()))
}

pub struct Handshake<'a> {
    pub client_name: &'a JsString,
    pub client_version: &'a JsString,
    pub native_version: &'a JsString,
    /// Canonical home selected and checked by the connection owner before launch.
    pub expected_home: &'a Path,
}

/// Authenticate the server identity before declaring initialized. The connection
/// owner must close and retire its process if this fails; this helper owns no process.
pub async fn initialize(rpc: &NativeRpc, settings: Handshake<'_>) -> IvyResult<()> {
    let reply = rpc
        .request(
            "initialize".into(),
            IvyJson::object([
                (
                    "clientInfo",
                    IvyJson::object([
                        ("name", IvyJson::String(settings.client_name.clone())),
                        ("version", IvyJson::String(settings.client_version.clone())),
                    ]),
                ),
                (
                    "capabilities",
                    IvyJson::object([
                        ("experimentalApi", true.into()),
                        ("requestAttestation", false.into()),
                    ]),
                ),
            ]),
            RequestOptions {
                timeout_ms: 30_000,
                ..Default::default()
            },
        )
        .await?;
    let result = reply.get("result").ok_or_else(|| {
        IvyError::new(
            "native_initialization_failed",
            "Codex initialization did not succeed.",
        )
    })?;
    let mismatch = || {
        IvyError::new(
            "native_home_mismatch",
            "Connected app-server belongs to a different Codex home.",
        )
    };
    // The original property access on a null result throws, unlike an absent
    // codexHome property. Preserve its generic internal-error boundary.
    if matches!(result, IvyJson::Null) {
        return Err(IvyError::internal());
    }
    let observed = result
        .get("codexHome")
        .and_then(IvyJson::as_js_string)
        .ok_or_else(mismatch)?;
    #[cfg(windows)]
    let observed = {
        use std::os::windows::ffi::OsStringExt;
        std::ffi::OsString::from_wide(&observed.0)
    };
    #[cfg(not(windows))]
    let observed = std::ffi::OsString::from(String::from_utf16_lossy(&observed.0));
    let actual = tokio::fs::canonicalize(observed)
        .await
        .map_err(|_| IvyError::internal())?;
    if actual != settings.expected_home {
        return Err(mismatch());
    }
    if initialized_server_version(result.get("userAgent").unwrap_or(&IvyJson::Null))?
        != *settings.native_version
    {
        return Err(IvyError::new(
            "native_server_version_mismatch",
            "Connected app-server does not match the pinned protocol version.",
        ));
    }
    rpc.notify("initialized".into(), IvyJson::Null).await
}
