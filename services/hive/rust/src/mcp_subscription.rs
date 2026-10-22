//! Hive's modern subscription wire policy. The transport must stream the opening
//! frame without ending the response, schedule ticks and own abort/cleanup.
// Adapted from the TypeScript MCP SDK 2.0.0; translated to Rust with IvyJson.
// See NOTICE and licenses/mcp-sdk.txt in this crate.
use crate::mcp_protocol::Reply;
use ivy_contracts::{IvyError, IvyJson, IvyResult, json::stringify};

pub const KEEP_ALIVE_MS: u64 = 15_000;
const SUBSCRIPTION_ID: &str = "io.modelcontextprotocol/subscriptionId";
const SERVER_INFO: &str = "io.modelcontextprotocol/serverInfo";

pub struct Opening {
    pub status: u16,
    pub headers: IvyJson,
    pub first_frame: String,
}
pub enum Admission {
    Rejected(Reply),
    Stream(Opening, Subscription),
}
pub struct Subscription {
    id: IvyJson,
    server_info: IvyJson,
    closed: bool,
}
fn empty() -> IvyJson {
    IvyJson::object([] as [(&str, IvyJson); 0])
}
fn valid_filter(body: &IvyJson) -> bool {
    let Some(filter @ IvyJson::Object(_)) = body.get("params").and_then(|p| p.get("notifications"))
    else {
        return false;
    };
    for key in [
        "toolsListChanged",
        "promptsListChanged",
        "resourcesListChanged",
    ] {
        if filter.get(key).is_some_and(|v| v.as_bool().is_none()) {
            return false;
        }
    }
    filter.get("resourceSubscriptions").is_none_or(|value| {
        value
            .as_array()
            .is_some_and(|uris| uris.iter().all(|uri| uri.as_js_string().is_some()))
    })
}
fn frame(value: IvyJson) -> IvyResult<String> {
    Ok(format!(
        "event: message\ndata: {}\n\n",
        stringify(&value, usize::MAX)?
    ))
}
/// Called only after modern envelope/header admission for subscriptions/listen.
/// Hive advertises tools:{}; none of the four notification filters is honored.
/// An empty honored filter still opens a stream and sends its acknowledgement.
pub fn admit(body: &IvyJson, server_info: &IvyJson) -> IvyResult<Admission> {
    if body.get("method") != Some(&"subscriptions/listen".into()) {
        return Err(IvyError::internal());
    }
    let id = body.get("id").ok_or_else(IvyError::internal)?.clone();
    if !valid_filter(body) {
        return Ok(Admission::Rejected(Reply::transport_error(
            200,
            id,
            IvyJson::object([
                ("code", IvyJson::Number(-32602.0)),
                ("message", "Invalid params: 'notifications' is required and must be a valid SubscriptionFilter".into()),
            ]),
        )?));
    }
    let first_frame = frame(IvyJson::object([
        ("jsonrpc", "2.0".into()),
        ("method", "notifications/subscriptions/acknowledged".into()),
        (
            "params",
            IvyJson::object([
                ("notifications", empty()),
                ("_meta", IvyJson::object([(SUBSCRIPTION_ID, id.clone())])),
            ]),
        ),
    ]))?;
    Ok(Admission::Stream(
        Opening {
            status: 200,
            headers: IvyJson::object([
                ("content-type", "text/event-stream".into()),
                ("cache-control", "no-cache, no-transform".into()),
                ("connection", "keep-alive".into()),
                ("x-accel-buffering", "no".into()),
            ]),
            first_frame,
        },
        Subscription {
            id,
            server_info: server_info.clone(),
            closed: false,
        },
    ))
}
impl Subscription {
    pub fn closed(&self) -> bool {
        self.closed
    }
    /// The owning scheduler calls this only when the keep-alive interval fires.
    pub fn tick(&self) -> Option<&'static str> {
        (!self.closed).then_some(": keepalive\n\n")
    }
    /// Abort/cancel is abrupt; graceful shutdown writes one terminal result.
    /// Bus/timer/signal cleanup remains the transport owner's responsibility.
    pub fn close(&mut self, graceful: bool) -> IvyResult<Option<String>> {
        if self.closed {
            return Ok(None);
        }
        let terminal = if graceful {
            Some(frame(IvyJson::object([
                ("jsonrpc", "2.0".into()),
                ("id", self.id.clone()),
                (
                    "result",
                    IvyJson::object([
                        ("resultType", "complete".into()),
                        (
                            "_meta",
                            IvyJson::object([
                                (SUBSCRIPTION_ID, self.id.clone()),
                                (SERVER_INFO, self.server_info.clone()),
                            ]),
                        ),
                    ]),
                ),
            ]))?)
        } else {
            None
        };
        self.closed = true;
        Ok(terminal)
    }
}
