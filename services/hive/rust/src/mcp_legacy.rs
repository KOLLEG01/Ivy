//! Gates for a fresh stateless legacy MCP transport, after entry classification
//! and Accept/Content-Type checks. Factory/connect and close remain caller-owned.
// Adapted from the TypeScript MCP SDK 2.0.0; translated to Rust with IvyJson.
// See NOTICE and licenses/mcp-sdk.txt in this crate.
use crate::{
    mcp_inbound::{self, Request, Route},
    mcp_parameters,
    mcp_protocol::{self, MessageKind, Reply},
};
use ivy_contracts::{IvyJson, IvyResult, JsString};

pub const VERSIONS: &[&str] = &[
    "2025-11-25",
    "2025-06-18",
    "2025-03-26",
    "2024-11-05",
    "2024-10-07",
];
fn error(message: JsString, code: i32) -> IvyResult<Option<Reply>> {
    Reply::transport_error(
        400,
        IvyJson::Null,
        IvyJson::object([
            ("code", IvyJson::Number(code as f64)),
            ("message", IvyJson::String(message)),
        ]),
    )
    .map(Some)
}
/// Some is a terminal transport reply; None continues protocol dispatch. This
/// models the source's per-request sessionIdGenerator=undefined configuration,
/// without inventing persistent initialization or session state.
pub fn admission(request: &Request<'_>, route: &Route) -> IvyResult<Option<Reply>> {
    if !request.http_method.eq_ignore_ascii_case("POST") || !matches!(route, Route::Legacy { .. }) {
        return Ok(None);
    }
    let messages = match request.body {
        IvyJson::Array(values) => values.as_slice(),
        _ => std::slice::from_ref(request.body),
    };
    let initializes = messages.iter().any(mcp_parameters::legacy_initialize);
    if initializes && messages.len() > 1 {
        return error(
            "Invalid Request: Only one initialization request is allowed".into(),
            -32600,
        );
    }
    if !initializes {
        if let Some(version) = request.protocol_version.map(mcp_inbound::ows) {
            if !VERSIONS.iter().any(|v| version == JsString::from(*v)) {
                let mut message = JsString::from("Bad Request: Unsupported protocol version: ").0;
                message.extend(&version.0);
                message.extend(
                    format!(" (supported versions: {})", VERSIONS.join(", ")).encode_utf16(),
                );
                return error(JsString(message), -32000);
            }
        }
    }
    if !messages
        .iter()
        .any(|message| mcp_protocol::message_kind(message) == Some(MessageKind::Request))
    {
        return Ok(Some(Reply::accepted()));
    }
    Ok(None)
}
