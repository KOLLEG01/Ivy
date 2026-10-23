//! Initialization and identity checks shared by the owned and proxy connections.
use super::{
    codex_rpc::{NativeRpc, RequestOptions},
    node_path, paths,
};
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString};
use std::path::Path;

fn whitespace(unit: u16) -> bool {
    char::from_u32(u32::from(unit)).is_some_and(char::is_whitespace)
}

/// Read the leading server name/version; client metadata cannot supply a version.
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
    let result = reply
        .get("result")
        .filter(|result| result.as_object().is_some())
        .ok_or_else(|| {
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
    let observed = result
        .get("codexHome")
        .and_then(IvyJson::as_js_string)
        .ok_or_else(mismatch)?;
    let observed = node_path::to_native(observed);
    if !observed.is_absolute() {
        return Err(mismatch());
    }
    let actual = tokio::task::spawn_blocking(move || paths::realpath(&observed))
        .await
        .map_err(|_| IvyError::internal())?
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
