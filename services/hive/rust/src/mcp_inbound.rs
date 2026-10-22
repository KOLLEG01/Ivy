//! Body-primary MCP era classification and HTTP entry rejections. Application
//! dispatch, capability gates and transport lifetimes remain with their owners.
// MCP compatibility logic adapted from the TypeScript SDK 2.0.0 and translated
// to Rust with IvyJson. See NOTICE and licenses/mcp-sdk.txt in this crate.
use crate::mcp_protocol::{self, MessageKind, Reply};
use base64::{
    Engine, alphabet,
    engine::{GeneralPurpose, GeneralPurposeConfig},
};
use ivy_contracts::{IvyJson, IvyResult, JsString};

pub const VERSION: &str = "2026-07-28";
pub const VERSION_META: &str = "io.modelcontextprotocol/protocolVersion";
pub const CAPABILITIES_META: &str = "io.modelcontextprotocol/clientCapabilities";
pub(crate) const INFO_META: &str = "io.modelcontextprotocol/clientInfo";
pub(crate) const LOG_META: &str = "io.modelcontextprotocol/logLevel";

/// Header values and method have already passed the owning HTTP parser.
pub struct Request<'a> {
    pub http_method: &'a str,
    pub body: &'a IvyJson,
    pub protocol_version: Option<&'a JsString>,
    pub method: Option<&'a JsString>,
    pub name: Option<&'a JsString>,
    pub content_type: Option<&'a JsString>,
    pub accept: Option<&'a JsString>,
}
#[derive(Debug, Clone, PartialEq)]
pub struct Rejection {
    pub rung: &'static str,
    pub cell: &'static str,
    pub status: u16,
    pub code: i32,
    pub message: JsString,
    pub data: Option<IvyJson>,
}
impl Rejection {
    pub fn error(&self) -> IvyJson {
        let mut error = IvyJson::object([
            ("code", IvyJson::Number(self.code as f64)),
            ("message", IvyJson::String(self.message.clone())),
        ]);
        if let Some(data) = &self.data {
            error.insert("data", data.clone());
        }
        error
    }
    pub fn to_json(&self) -> IvyJson {
        let mut result = IvyJson::object([
            ("kind", "reject".into()),
            ("rung", self.rung.into()),
            ("cell", self.cell.into()),
            ("httpStatus", IvyJson::Number(self.status as f64)),
            ("code", IvyJson::Number(self.code as f64)),
            ("message", IvyJson::String(self.message.clone())),
        ]);
        if let Some(data) = &self.data {
            result.insert("data", data.clone());
        }
        result.insert("settled", true.into());
        result
    }
    pub fn reply(&self, body: &IvyJson) -> IvyResult<Reply> {
        Reply::transport_error(self.status, mcp_protocol::response_id(body), self.error())
    }
}
#[derive(Debug, Clone, PartialEq)]
pub enum Route {
    Legacy {
        reason: &'static str,
        requested_version: Option<JsString>,
    },
    Modern {
        notification: bool,
        modern: bool,
        revision: Option<JsString>,
    },
    Reject(Rejection),
}
impl Route {
    pub fn to_json(&self, body: &IvyJson) -> IvyJson {
        match self {
            Self::Reject(error) => error.to_json(),
            Self::Legacy {
                reason,
                requested_version,
            } => {
                let mut route =
                    IvyJson::object([("kind", "legacy".into()), ("reason", (*reason).into())]);
                if let Some(version) = requested_version {
                    route.insert("requestedVersion", IvyJson::String(version.clone()));
                }
                route
            }
            Self::Modern {
                notification,
                modern,
                revision,
            } => {
                let mut classification =
                    IvyJson::object([("era", if *modern { "modern" } else { "legacy" }.into())]);
                if let Some(version) = revision {
                    classification.insert("revision", IvyJson::String(version.clone()));
                }
                IvyJson::object([
                    ("kind", "modern".into()),
                    (
                        "messageKind",
                        if *notification {
                            "notification"
                        } else {
                            "request"
                        }
                        .into(),
                    ),
                    ("message", body.clone()),
                    ("classification", classification),
                ])
            }
        }
    }
}
fn cat(parts: impl IntoIterator<Item = JsString>) -> JsString {
    JsString(parts.into_iter().flat_map(|v| v.0).collect())
}
fn object(value: &IvyJson) -> bool {
    matches!(value, IvyJson::Object(_))
}
fn modern(version: &JsString) -> bool {
    version.0.as_slice() >= JsString::from(VERSION).0.as_slice()
}
fn text<'a>(value: &'a IvyJson, key: &str) -> Option<&'a JsString> {
    value.get(key).and_then(IvyJson::as_js_string)
}
fn meta(body: &IvyJson) -> Option<&IvyJson> {
    body.get("params")
        .and_then(|v| v.get("_meta"))
        .filter(|v| object(v))
}
fn claim(body: &IvyJson) -> bool {
    meta(body).is_some_and(|v| v.get(VERSION_META).is_some())
}
fn version(body: &IvyJson) -> Option<&JsString> {
    meta(body).and_then(|v| text(v, VERSION_META))
}
pub(crate) fn ows(value: &JsString) -> JsString {
    let start = value
        .0
        .iter()
        .position(|v| ![9, 32].contains(v))
        .unwrap_or(value.0.len());
    let end = value
        .0
        .iter()
        .rposition(|v| ![9, 32].contains(v))
        .map_or(start, |v| v + 1);
    JsString(value.0[start..end].to_vec())
}
fn rejection(
    rung: &'static str,
    cell: &'static str,
    code: i32,
    message: impl Into<JsString>,
    data: Option<IvyJson>,
) -> Route {
    Route::Reject(Rejection {
        rung,
        cell,
        status: 400,
        code,
        message: message.into(),
        data,
    })
}
fn mismatch(rung: &'static str, cell: &'static str, header: JsString, body: JsString) -> Route {
    rejection(
        rung,
        cell,
        -32020,
        cat([
            "Bad Request: the request headers and body disagree: ".into(),
            body.clone(),
        ]),
        Some(IvyJson::object([(
            "mismatch",
            IvyJson::object([
                ("header", IvyJson::String(header)),
                ("body", IvyJson::String(body)),
            ]),
        )])),
    )
}
fn legacy(reason: &'static str, requested_version: Option<&JsString>) -> Route {
    Route::Legacy {
        reason,
        requested_version: requested_version.cloned(),
    }
}
fn route(notification: bool, revision: Option<&JsString>) -> Route {
    Route::Modern {
        notification,
        modern: revision.is_none_or(modern),
        revision: revision.cloned(),
    }
}

struct Issue {
    key: JsString,
    problem: JsString,
}
impl Issue {
    fn json(&self) -> IvyJson {
        IvyJson::object([
            ("key", IvyJson::String(self.key.clone())),
            ("problem", IvyJson::String(self.problem.clone())),
        ])
    }
}
/// Only the first issue is exposed by request classification. Missing required
/// keys precede typed fields; nested fields retain the pinned schema's order.
fn envelope_issue(meta: &IvyJson) -> Option<Issue> {
    for key in [VERSION_META, CAPABILITIES_META] {
        if meta.get(key).is_none() {
            return Some(Issue {
                key: key.into(),
                problem: "missing".into(),
            });
        }
    }
    schema_issue(meta)
}
fn schema_issue(meta: &IvyJson) -> Option<Issue> {
    crate::mcp_parameters::envelope_issues(meta)
        .into_iter()
        .next()
        .map(|issue| Issue {
            key: issue.key(),
            problem: issue.message,
        })
}
fn bad_envelope(cell: &'static str, issue: Issue) -> Route {
    rejection(
        "envelope",
        cell,
        -32602,
        cat([
            "Invalid _meta envelope for protocol revision 2026-07-28: ".into(),
            issue.key.clone(),
            ": ".into(),
            issue.problem.clone(),
        ]),
        Some(IvyJson::object([("envelope", issue.json())])),
    )
}

pub fn classify(request: &Request<'_>) -> Route {
    if !request.http_method.eq_ignore_ascii_case("POST") {
        return legacy("http-method", None);
    }
    let body = request.body;
    if let IvyJson::Array(batch) = body {
        if batch.is_empty() {
            return rejection(
                "jsonrpc-shape",
                "empty-batch",
                -32600,
                "Bad Request: empty JSON-RPC batch",
                None,
            );
        }
        for message in batch {
            if claim(message) {
                return rejection(
                    "jsonrpc-shape",
                    "batch-with-modern-element",
                    -32600,
                    "Bad Request: JSON-RPC batches may not contain requests for protocol revision 2026-07-28 or later",
                    None,
                );
            }
            if mcp_protocol::message_kind(message).is_none() {
                return rejection(
                    "jsonrpc-shape",
                    "batch-with-invalid-element",
                    -32600,
                    "Bad Request: JSON-RPC batch contains an invalid message",
                    None,
                );
            }
        }
        return legacy("batch", None);
    }
    let notification = match mcp_protocol::message_kind(body) {
        Some(MessageKind::Request) => false,
        Some(MessageKind::Notification) => true,
        Some(MessageKind::Result | MessageKind::Error) => return legacy("response", None),
        None => {
            return rejection(
                "jsonrpc-shape",
                "invalid-json-rpc-body",
                -32600,
                "Bad Request: the request body is not a valid JSON-RPC message",
                None,
            );
        }
    };
    let method = text(body, "method").unwrap();
    let header = request.protocol_version.map(ows);
    let header_modern = header.as_ref().is_some_and(modern);
    let method_header = request.method.map(ows);
    if !notification
        && method == &JsString::from("initialize")
        && !(version(body).is_some_and(modern)
            && meta(body).is_some_and(|m| envelope_issue(m).is_none()))
    {
        if header_modern {
            return mismatch("era-classification","initialize-with-modern-header",header.unwrap(),"an initialize request (legacy handshake) was sent with a modern MCP-Protocol-Version header".into());
        }
        return legacy(
            "initialize",
            body.get("params").and_then(|v| text(v, "protocolVersion")),
        );
    }
    if claim(body) {
        if notification {
            if version(body).is_none() {
                return bad_envelope(
                    "notification-envelope-invalid",
                    schema_issue(meta(body).unwrap()).unwrap(),
                );
            }
        } else if let Some(issue) = meta(body).and_then(envelope_issue) {
            return bad_envelope("envelope-invalid", issue);
        }
        let claimed = version(body);
        if let (Some(header), Some(claimed)) = (&header, claimed) {
            if header != claimed {
                return mismatch(
                    "era-classification",
                    if notification {
                        "notification-header-body-version-mismatch"
                    } else {
                        "header-body-version-mismatch"
                    },
                    header.clone(),
                    cat([
                        if notification {
                            "the notification envelope names protocol version "
                        } else {
                            "the body envelope names protocol version "
                        }
                        .into(),
                        claimed.clone(),
                        " but the MCP-Protocol-Version header names ".into(),
                        header.clone(),
                    ]),
                );
            }
        }
        if !notification || claimed.is_some_and(modern) {
            if let Some(header) = method_header {
                if &header != method {
                    return mismatch(
                        "era-classification",
                        if notification {
                            "notification-method-header-mismatch"
                        } else {
                            "method-header-mismatch"
                        },
                        header.clone(),
                        cat([
                            if notification {
                                "the notification body names method "
                            } else {
                                "the body names method "
                            }
                            .into(),
                            method.clone(),
                            " but the Mcp-Method header names ".into(),
                            header,
                        ]),
                    );
                }
            }
        }
        return route(notification, claimed);
    }
    if header_modern {
        if notification {
            if let Some(method_header) = method_header {
                if &method_header != method {
                    return mismatch(
                        "era-classification",
                        "notification-method-header-mismatch",
                        method_header.clone(),
                        cat([
                            "the notification body names method ".into(),
                            method.clone(),
                            " but the Mcp-Method header names ".into(),
                            method_header,
                        ]),
                    );
                }
            }
            return route(true, header.as_ref());
        }
        let missing: Vec<JsString> = if let Some(meta) = meta(body) {
            [VERSION_META, CAPABILITIES_META]
                .into_iter()
                .filter(|key| meta.get(key).is_none())
                .map(Into::into)
                .collect()
        } else {
            vec!["_meta".into()]
        };
        let joined = cat(missing.iter().enumerate().flat_map(|(i, v)| {
            if i == 0 {
                vec![v.clone()]
            } else {
                vec![", ".into(), v.clone()]
            }
        }));
        return rejection(
            "envelope",
            "modern-header-without-claim",
            -32602,
            cat([
                "Invalid params: the MCP-Protocol-Version header names protocol revision ".into(),
                header.unwrap(),
                ", but the request is missing the required per-request envelope key(s): ".into(),
                joined,
            ]),
            Some(IvyJson::object([(
                "envelope",
                IvyJson::object([(
                    "missing",
                    IvyJson::Array(missing.into_iter().map(IvyJson::String).collect()),
                )]),
            )])),
        );
    }
    legacy(
        if notification {
            "notification"
        } else {
            "no-claim"
        },
        header.as_ref(),
    )
}

fn decode_name(value: &JsString) -> Option<JsString> {
    if !value
        .0
        .starts_with(&"=?base64?".encode_utf16().collect::<Vec<_>>())
        || !value.0.ends_with(&[63, 61])
    {
        return Some(value.clone());
    }
    // String.slice returns empty when the sentinel's prefix/suffix overlap.
    let raw = if value.0.len() < 11 {
        &[][..]
    } else {
        &value.0[9..value.0.len() - 2]
    };
    if raw.iter().any(|v| *v > 127) {
        return None;
    }
    let bytes: Vec<u8> = raw.iter().map(|v| *v as u8).collect();
    let engine = GeneralPurpose::new(
        &alphabet::STANDARD,
        GeneralPurposeConfig::new().with_decode_allow_trailing_bits(true),
    );
    let decoded = String::from_utf8(engine.decode(bytes).ok()?).ok()?;
    Some(decoded.strip_prefix('\u{feff}').unwrap_or(&decoded).into())
}
pub fn standard_headers(request: &Request<'_>, route: &Route) -> Option<Rejection> {
    if !matches!(
        route,
        Route::Modern {
            notification: false,
            ..
        }
    ) {
        return None;
    }
    let method = text(request.body, "method").unwrap();
    let failure = if request.method.is_none() {
        Some(mismatch(
            "standard-header-validation",
            "method-header-missing",
            "(missing)".into(),
            cat([
                "the body names method ".into(),
                method.clone(),
                " but the required Mcp-Method header is absent".into(),
            ]),
        ))
    } else {
        let source = if method == &JsString::from("tools/call")
            || method == &JsString::from("prompts/get")
        {
            Some("name")
        } else if method == &JsString::from("resources/read") {
            Some("uri")
        } else {
            None
        };
        source.and_then(|source| {
            let body = request.body.get("params").and_then(|p| text(p, source));
            let missing = || {
                cat([
                    "the body carries params.".into(),
                    source.into(),
                    "=\"".into(),
                    body.unwrap().clone(),
                    "\" but the required Mcp-Name header is absent".into(),
                ])
            };
            match request.name {
                None => body.map(|_| {
                    mismatch(
                        "standard-header-validation",
                        "name-header-missing",
                        "(missing)".into(),
                        missing(),
                    )
                }),
                Some(header) => {
                    let normalized = ows(header);
                    match decode_name(&normalized) {
                        None => Some(mismatch(
                            "standard-header-validation",
                            "name-header-invalid-encoding",
                            normalized,
                            "the Mcp-Name header carries an invalid Base64 sentinel value".into(),
                        )),
                        Some(decoded) => body.filter(|body| *body != &decoded).map(|body| {
                            mismatch(
                                "standard-header-validation",
                                "name-header-mismatch",
                                normalized,
                                cat([
                                    "the body carries params.".into(),
                                    source.into(),
                                    "=\"".into(),
                                    body.clone(),
                                    "\" but the Mcp-Name header names \"".into(),
                                    decoded,
                                    "\"".into(),
                                ]),
                            )
                        }),
                    }
                }
            }
        })
    };
    failure.and_then(|failure| {
        if let Route::Reject(error) = failure {
            Some(error)
        } else {
            None
        }
    })
}
/// These are entry/legacy Accept gates only. None delegates to the protocol
/// adapter; it does not establish method, capability, params or lifetime admission.
pub fn entry_rejection(request: &Request<'_>, route: &Route) -> IvyResult<Option<Reply>> {
    if !request.http_method.eq_ignore_ascii_case("POST") {
        return Ok(None);
    }
    let content_type = request.content_type.map(|v| {
        let end = v.0.iter().position(|v| *v == 59).unwrap_or(v.0.len());
        let raw = JsString(v.0[..end].to_vec()).trim();
        raw.0
            .into_iter()
            .map(|v| if (65..=90).contains(&v) { v + 32 } else { v })
            .collect::<Vec<_>>()
    });
    if content_type.as_deref() != Some(JsString::from("application/json").0.as_slice()) {
        return Reply::transport_error(
            415,
            IvyJson::Null,
            IvyJson::object([
                ("code", IvyJson::Number(-32000.0)),
                (
                    "message",
                    "Unsupported Media Type: Content-Type must be application/json".into(),
                ),
            ]),
        )
        .map(Some);
    }
    match route {
        Route::Reject(error) => error.reply(request.body).map(Some),
        Route::Modern { revision, .. } => {
            if revision.as_ref() != Some(&VERSION.into()) {
                let requested = revision.clone().unwrap_or_else(|| "unknown".into());
                let error = Rejection {
                    rung: "era-classification",
                    cell: "unsupported-version",
                    status: 400,
                    code: -32022,
                    message: cat(["Unsupported protocol version: ".into(), requested.clone()]),
                    data: Some(IvyJson::object([
                        ("supported", IvyJson::Array(vec![VERSION.into()])),
                        ("requested", IvyJson::String(requested)),
                    ])),
                };
                return error.reply(request.body).map(Some);
            }
            standard_headers(request, route)
                .map(|e| e.reply(request.body))
                .transpose()
        }
        Route::Legacy { .. } => {
            let accepts = |kind: &str| {
                let needle: Vec<_> = kind.encode_utf16().collect();
                request
                    .accept
                    .is_some_and(|v| v.0.windows(needle.len()).any(|w| w == needle))
            };
            if !accepts("application/json") || !accepts("text/event-stream") {
                Reply::transport_error(406,IvyJson::Null,IvyJson::object([("code",IvyJson::Number(-32000.0)),("message","Not Acceptable: Client must accept both application/json and text/event-stream".into())])).map(Some)
            } else {
                Ok(None)
            }
        }
    }
}
