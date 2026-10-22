//! Common HTTP transitions before route-specific authentication and body work.
use crate::{
    admission::{RequestBudget, RequestLease},
    http_response::{self, Response},
};
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString,
    json::parse,
    limits::CONNECTION_IN_FLIGHT_REQUESTS,
    whatwg_url::{UrlProjection, parse_url_with_base, url_components},
};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

#[derive(Clone, Default)]
pub struct Requests {
    budget: RequestBudget,
    active: Arc<Mutex<HashMap<u64, usize>>>,
}
impl Requests {
    pub fn new(budget: RequestBudget) -> Self {
        Self {
            budget,
            active: Arc::default(),
        }
    }
    /// The transport supplies a stable identity for the actual socket. Header
    /// failure precedes the active-count change and the HTTP try/finally scope.
    pub fn begin(&self, socket: u64, response: &mut impl Response) -> IvyResult<Request> {
        http_response::security_headers(response)?;
        let mut active = self.active.lock().unwrap();
        let count = active.entry(socket).or_default();
        let previous = *count;
        *count += 1;
        Ok(Request {
            owner: self.clone(),
            socket,
            previous,
            lease: None,
            released: false,
        })
    }
    pub fn active(&self, socket: u64) -> usize {
        self.active
            .lock()
            .unwrap()
            .get(&socket)
            .copied()
            .unwrap_or(0)
    }
}
pub struct Request {
    owner: Requests,
    socket: u64,
    previous: usize,
    lease: Option<RequestLease>,
    released: bool,
}
impl Request {
    /// Resolve clientAddress first, then acquire the shared request budget.
    /// Keep this scope alive through the error response and release in finally.
    pub fn admit(&mut self, client: &JsString) -> IvyResult<()> {
        let mut key = JsString::from("http:");
        key.0.extend(&client.0);
        self.lease = Some(self.owner.budget.acquire(&key, None)?);
        if self.previous >= CONNECTION_IN_FLIGHT_REQUESTS {
            return Err(IvyError::new(
                "limit_exceeded",
                "Too many requests on this connection.",
            ));
        }
        Ok(())
    }
    pub fn body_lease(&mut self) -> Option<&mut RequestLease> {
        self.lease.as_mut()
    }
    pub fn release(&mut self) {
        if self.released {
            return;
        }
        self.released = true;
        if let Some(lease) = &mut self.lease {
            lease.release();
        }
        let mut active = self.owner.active.lock().unwrap();
        let remaining = active.get(&self.socket).copied().unwrap_or(1) - 1;
        if remaining == 0 {
            active.remove(&self.socket);
        } else {
            active.insert(self.socket, remaining);
        }
    }
}
impl Drop for Request {
    fn drop(&mut self) {
        self.release();
    }
}

pub struct Route {
    pub url: UrlProjection,
    pub pathname: JsString,
    /// The top-level OAuth metadata route precedes installation path admission.
    pub authorization_metadata: bool,
    pub path: JsString,
}
pub fn route(
    method: Option<&JsString>,
    raw_url: Option<&JsString>,
    origin: &JsString,
    base_path: &JsString,
) -> IvyResult<Route> {
    let fallback = JsString::from("/");
    let url = parse_url_with_base(raw_url.unwrap_or(&fallback), origin)?;
    let pathname: JsString = url_components(&url.href.clone().into())?.pathname.into();
    if method == Some(&JsString::from("GET"))
        && pathname == JsString::from("/.well-known/oauth-authorization-server")
    {
        return Ok(Route {
            url,
            pathname,
            authorization_metadata: true,
            path: JsString::default(),
        });
    }
    let mut prefix = base_path.clone();
    prefix.0.push(47);
    if pathname != *base_path && !pathname.0.starts_with(&prefix.0) {
        return Err(IvyError::new(
            "not_found",
            "Path is outside this Hive installation.",
        ));
    }
    let path = if pathname.0.len() == base_path.0.len() {
        "/".into()
    } else {
        JsString(pathname.0[base_path.0.len()..].to_vec())
    };
    Ok(Route {
        url,
        pathname,
        authorization_metadata: false,
        path,
    })
}
/// Body byte limits belong to BodyBuffers. TextDecoder removes one UTF-8 BOM;
/// malformed UTF-8 and every JSON parse failure share the original public error.
pub fn json_body(bytes: &[u8]) -> IvyResult<IvyJson> {
    std::str::from_utf8(bytes)
        .ok()
        .and_then(|text| parse(text.strip_prefix('\u{feff}').unwrap_or(text), usize::MAX).ok())
        .ok_or_else(|| IvyError::new("invalid_frame", "Malformed JSON input."))
}
/// None is an opaque non-Ivy exception. Preserve MCP challenge effects before
/// the common response transition, and OAuth's separate status/body contract.
pub fn error(
    origin: &JsString,
    base_path: &JsString,
    path: &JsString,
    id: IvyJson,
    failure: Option<&IvyError>,
    response: &mut impl Response,
) -> IvyResult<()> {
    if (path == &JsString::from("/mcp") || path == &JsString::from("/mcp-dev"))
        && failure.is_some_and(|e| e.code_js() == JsString::from("unauthenticated"))
    {
        let mut value = JsString::from("Bearer resource_metadata=\"");
        value.0.extend(&origin.0);
        value.0.extend(&base_path.0);
        value
            .0
            .extend("/.well-known/oauth-protected-resource".encode_utf16());
        if path == &JsString::from("/mcp-dev") {
            value.0.extend("/mcp-dev".encode_utf16());
        }
        value.0.extend("\", scope=\"hive\"".encode_utf16());
        response.set_header("WWW-Authenticate", &value)?;
    }
    let opaque = IvyError::internal();
    let failure = failure.unwrap_or(&opaque);
    if path == &JsString::from("/oauth/token") || path == &JsString::from("/oauth/register") {
        let unauthenticated = failure.code_js() == JsString::from("unauthenticated");
        let capacity = failure.code_js() == JsString::from("limit_exceeded");
        return http_response::json(
            response,
            if unauthenticated {
                401
            } else if capacity {
                429
            } else {
                400
            },
            &IvyJson::object([
                (
                    "error",
                    if unauthenticated {
                        "invalid_grant"
                    } else if capacity {
                        "temporarily_unavailable"
                    } else {
                        "invalid_request"
                    }
                    .into(),
                ),
                ("error_description", IvyJson::String(failure.message_js())),
            ]),
        );
    }
    http_response::json(
        response,
        failure.http_status(),
        &IvyJson::object([
            ("jsonrpc", "2.0".into()),
            ("id", id),
            ("error", failure.to_wire()),
        ]),
    )
}
