//! Lossless MCP JSON-RPC framing. Version/envelope policy, method dispatch and
//! transport ownership stay with their adapters; this boundary supplies none.
use ivy_contracts::{IvyJson, IvyResult, JsString, json::stringify};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MessageKind {
    Request,
    Notification,
    Result,
    Error,
}
impl MessageKind {
    pub fn name(self) -> &'static str {
        match self {
            Self::Request => "request",
            Self::Notification => "notification",
            Self::Result => "result",
            Self::Error => "error",
        }
    }
}
fn object(value: &IvyJson) -> bool {
    matches!(value, IvyJson::Object(_))
}
fn keys(value: &IvyJson, allowed: &[&str]) -> bool {
    let IvyJson::Object(pairs) = value else {
        return false;
    };
    pairs
        .iter()
        .all(|(key, _)| allowed.iter().any(|name| key == &JsString::from(*name)))
}
fn optional(value: Option<&IvyJson>, predicate: impl FnOnce(&IvyJson) -> bool) -> bool {
    value.is_none_or(predicate)
}
fn integer(value: &IvyJson) -> bool {
    value
        .as_f64()
        .is_some_and(|n| n.is_finite() && n.fract() == 0.0 && n.abs() <= 9_007_199_254_740_991.0)
}
fn string(value: &IvyJson) -> bool {
    value.as_js_string().is_some()
}
fn request_id(value: &IvyJson) -> bool {
    string(value) || integer(value)
}
fn request_meta(value: &IvyJson) -> bool {
    object(value)
        && optional(value.get("progressToken"), request_id)
        && optional(value.get("io.modelcontextprotocol/related-task"), |task| {
            object(task) && task.get("taskId").is_some_and(string)
        })
}
fn params(value: &IvyJson) -> bool {
    object(value) && optional(value.get("_meta"), request_meta)
}
fn result(value: &IvyJson) -> bool {
    // Result metadata is open. The SDK catches invalid self-reported
    // serverInfo rather than rejecting the result, unlike request metadata.
    object(value) && optional(value.get("_meta"), object)
}
fn error(value: &IvyJson) -> bool {
    object(value)
        && value.get("code").is_some_and(integer)
        && value.get("message").is_some_and(string)
}
/// The pinned SDK's strict outer message shapes. Keep UTF-16 string IDs and
/// arbitrary open params intact; admitting a shape does not admit a method.
pub fn message_kind(value: &IvyJson) -> Option<MessageKind> {
    if value.get("jsonrpc") != Some(&"2.0".into()) {
        return None;
    }
    let id = value.get("id");
    if keys(value, &["jsonrpc", "id", "result"])
        && id.is_some_and(request_id)
        && value.get("result").is_some_and(result)
    {
        return Some(MessageKind::Result);
    }
    if keys(value, &["jsonrpc", "id", "error"])
        && optional(id, request_id)
        && value.get("error").is_some_and(error)
    {
        return Some(MessageKind::Error);
    }
    if value.get("method").is_some_and(string) && optional(value.get("params"), params) {
        if keys(value, &["jsonrpc", "id", "method", "params"]) && id.is_some_and(request_id) {
            return Some(MessageKind::Request);
        }
        if keys(value, &["jsonrpc", "method", "params"]) {
            return Some(MessageKind::Notification);
        }
    }
    None
}
/// A string method identifies entry-error correlation. Its ID is broader than
/// a valid RequestId: even a fractional number that invalidated the frame echoes.
pub fn response_id(body: &IvyJson) -> IvyJson {
    if body.get("method").and_then(IvyJson::as_js_string).is_none() {
        return IvyJson::Null;
    }
    match body.get("id") {
        Some(value @ (IvyJson::String(_) | IvyJson::Number(_))) => value.clone(),
        _ => IvyJson::Null,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Encoding {
    Json,
    Sse,
}
#[derive(Debug, Clone, PartialEq)]
pub struct Reply {
    pub status: u16,
    pub headers: IvyJson,
    pub body: String,
}
impl Reply {
    fn encode(status: u16, encoding: Encoding, message: IvyJson) -> IvyResult<Self> {
        let json = stringify(&message, usize::MAX)?;
        let (headers, body) = match encoding {
            Encoding::Json => (
                IvyJson::object([("content-type", "application/json".into())]),
                json,
            ),
            Encoding::Sse => (
                IvyJson::object([
                    ("content-type", "text/event-stream".into()),
                    ("cache-control", "no-cache, no-transform".into()),
                ]),
                format!("event: message\ndata: {json}\n\n"),
            ),
        };
        Ok(Self {
            status,
            headers,
            body,
        })
    }
    pub fn accepted() -> Self {
        Self {
            status: 202,
            headers: IvyJson::object([] as [(&str, IvyJson); 0]),
            body: String::new(),
        }
    }
    /// Result projection/cache metadata belongs to the application/SDK adapter.
    pub fn success(encoding: Encoding, id: IvyJson, result: IvyJson) -> IvyResult<Self> {
        Self::encode(
            200,
            encoding,
            IvyJson::object([("result", result), ("jsonrpc", "2.0".into()), ("id", id)]),
        )
    }
    pub fn protocol_error(
        status: u16,
        encoding: Encoding,
        id: IvyJson,
        error: IvyJson,
    ) -> IvyResult<Self> {
        Self::encode(
            status,
            encoding,
            IvyJson::object([("jsonrpc", "2.0".into()), ("id", id), ("error", error)]),
        )
    }
    pub fn transport_error(status: u16, id: IvyJson, error: IvyJson) -> IvyResult<Self> {
        Self::encode(
            status,
            Encoding::Json,
            IvyJson::object([("jsonrpc", "2.0".into()), ("error", error), ("id", id)]),
        )
    }
    pub fn invalid_message(body: &IvyJson) -> IvyResult<Self> {
        Self::transport_error(
            400,
            response_id(body),
            IvyJson::object([
                ("code", IvyJson::Number(-32600.0)),
                (
                    "message",
                    "Bad Request: the request body is not a valid JSON-RPC message".into(),
                ),
            ]),
        )
    }
}
