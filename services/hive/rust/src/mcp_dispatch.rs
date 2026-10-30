//! Registered Hive MCP exchanges. The HTTP owner supplies headers,
//! authentication, request lifetime and the prepared application.
// Adapted from the TypeScript MCP SDK 2.0.0; translated to Rust with IvyJson.
// See NOTICE and licenses/mcp-sdk.txt in this crate.
use crate::{
    http_response::Response,
    mcp, mcp_inbound as inbound, mcp_legacy, mcp_parameters,
    mcp_protocol::{Encoding, Reply},
    mcp_request::{self, Error, ProtocolError},
    server_mcp,
};
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString};

/// Results come from Hive's application constructors, not arbitrary SDK handler
/// extensions. Tool content is the text/PNG output of success/failure helpers.
pub trait Application {
    fn info(&self) -> &IvyJson;
    fn instructions(&self) -> &IvyJson;
    fn list_tools(
        &self,
        cursor: Option<&JsString>,
    ) -> impl std::future::Future<Output = Result<IvyJson, ProtocolError>>;
    fn call_tool(
        &self,
        name: &JsString,
        arguments: Option<&IvyJson>,
    ) -> impl std::future::Future<Output = mcp_request::Result<IvyJson>>;
}
fn empty() -> IvyJson {
    IvyJson::object([] as [(&str, IvyJson); 0])
}
fn error(code: i32, message: JsString, data: Option<IvyJson>) -> IvyJson {
    let mut result = IvyJson::object([
        (
            "code",
            IvyJson::Number(if code == -32002 { -32602 } else { code } as f64),
        ),
        ("message", IvyJson::String(message)),
    ]);
    if let Some(data) = data {
        result.insert("data", data);
    }
    result
}
fn protocol_error(
    modern: bool,
    dispatch_window: bool,
    body: &IvyJson,
    code: i32,
    message: JsString,
    data: Option<IvyJson>,
) -> IvyResult<Reply> {
    // Only dispatch-window failures get the modern ladder status. An unknown
    // tool raised by a handler, for example, remains HTTP 200.
    Reply::protocol_error(
        if modern && dispatch_window && code == -32601 {
            404
        } else {
            200
        },
        if modern {
            Encoding::Json
        } else {
            Encoding::Sse
        },
        body.get("id").cloned().unwrap_or(IvyJson::Null),
        error(code, message, data),
    )
}
fn ordered(value: IvyJson, fields: &[&str]) -> IvyJson {
    let mut pairs = value.into_object().unwrap_or_default();
    let mut result = empty();
    for name in fields {
        if let Some(index) = pairs.iter().position(|(key, _)| key.eq_str(name)) {
            let (key, value) = pairs.remove(index);
            result.insert(key, value);
        }
    }
    for (name, value) in pairs {
        if result.get_key(&name).is_none() {
            result.insert(name, value);
        }
    }
    result
}
fn encode_result(method: &JsString, mut value: IvyJson, modern: bool, info: &IvyJson) -> IvyJson {
    // The SDK projects application tool results through its declared schema order.
    if method == &JsString::from("tools/call") {
        if let Some(IvyJson::Array(content)) = value.get_mut("content") {
            for item in content {
                let image = item.get("type") == Some(&"image".into());
                *item = ordered(
                    std::mem::replace(item, IvyJson::Null),
                    if image {
                        &["type", "data", "mimeType", "annotations", "_meta"]
                    } else {
                        &["type", "text", "annotations", "_meta"]
                    },
                );
            }
        }
        value = ordered(
            value,
            &["_meta", "content", "structuredContent", "isError"],
        );
    }
    if !modern {
        return value;
    }
    if method == &JsString::from("tools/list") {
        if let Some(IvyJson::Array(items)) = value.get_mut("tools") {
            for tool in items {
                tool.remove("execution");
            }
        }
    }
    if let Some(capabilities) = value.get_mut("capabilities") {
        capabilities.remove("tasks");
    }
    if value.get("resultType").is_none() {
        value.insert("resultType", "complete".into());
    }
    if method == &JsString::from("tools/list") || method == &JsString::from("server/discover") {
        if !value
            .get("ttlMs")
            .and_then(IvyJson::as_f64)
            .is_some_and(|n| n.is_finite() && n >= 0.0 && n.fract() == 0.0)
        {
            value.insert("ttlMs", IvyJson::Number(0.0));
        }
        if !value
            .get("cacheScope")
            .is_some_and(|v| v == &"public".into() || v == &"private".into())
        {
            value.insert("cacheScope", "private".into());
        }
    }
    let key = "io.modelcontextprotocol/serverInfo";
    match value.get_mut("_meta") {
        Some(meta @ IvyJson::Object(_)) => {
            if meta.get(key).is_none() {
                meta.insert(key, info.clone());
            }
        }
        None => {
            value.insert("_meta", IvyJson::object([(key, info.clone())]));
        }
        _ => {}
    }
    value
}

#[cfg(test)]
#[path = "../../../../tests/rust/hive_mcp_response_ownership.rs"]
mod ownership_tests;
/// Serve a POST after the owning HTTP parser and Hive single-frame guard.
pub async fn reply(
    request: &inbound::Request<'_>,
    application: &impl Application,
) -> mcp_request::Result<Reply> {
    if request.http_method != "POST" {
        return Err(IvyError::internal().into());
    }
    let route = inbound::classify(request);
    if let Some(reply) = inbound::entry_rejection(request, &route)? {
        return Ok(reply);
    }
    if let Some(reply) = mcp_legacy::admission(request, &route)? {
        return Ok(reply);
    }
    let modern = matches!(route, inbound::Route::Modern { .. });
    if matches!(
        route,
        inbound::Route::Modern {
            notification: true,
            ..
        }
    ) {
        return Ok(Reply::accepted());
    }
    let body = request.body;
    let Some(parameters) = mcp_parameters::request(body, modern) else {
        return Ok(protocol_error(
            modern,
            true,
            body,
            -32601,
            "Method not found".into(),
            None,
        )?);
    };
    let parsed = match parameters {
        Ok(parsed) => parsed,
        Err(issues) => {
            let tool = body.get("method") == Some(&"tools/call".into());
            let mut message = if tool {
                JsString::from("Invalid tools/call request: ").0
            } else {
                vec![]
            };
            message.extend(mcp_parameters::diagnostic(&issues)?.0);
            return Ok(protocol_error(
                modern,
                false,
                body,
                if tool { -32602 } else { -32603 },
                JsString(message),
                None,
            )?);
        }
    };
    let method = parsed
        .get("method")
        .and_then(IvyJson::as_js_string)
        .ok_or_else(IvyError::internal)?;
    let params = parsed.get("params");
    let outcome = if method == &JsString::from("ping") {
        Ok(empty())
    } else if method == &JsString::from("initialize") {
        let requested = params
            .and_then(|p| p.get("protocolVersion"))
            .and_then(IvyJson::as_js_string)
            .unwrap();
        let version = mcp_legacy::VERSIONS
            .iter()
            .find(|v| requested == &JsString::from(**v))
            .copied()
            .unwrap_or(mcp_legacy::VERSIONS[0]);
        let mut result = IvyJson::object([
            ("protocolVersion", version.into()),
            ("capabilities", IvyJson::object([("tools", empty())])),
            ("serverInfo", application.info().clone()),
        ]);
        if application
            .instructions()
            .as_js_string()
            .is_some_and(|s| !s.0.is_empty())
        {
            result.insert("instructions", application.instructions().clone());
        }
        Ok(result)
    } else if method == &JsString::from("server/discover") {
        let mut result = IvyJson::object([
            (
                "supportedVersions",
                IvyJson::Array(vec![inbound::VERSION.into()]),
            ),
            ("capabilities", IvyJson::object([("tools", empty())])),
        ]);
        if application
            .instructions()
            .as_js_string()
            .is_some_and(|s| !s.0.is_empty())
        {
            result.insert("instructions", application.instructions().clone());
        }
        Ok(result)
    } else if method == &JsString::from("tools/list") {
        application
            .list_tools(
                params
                    .and_then(|p| p.get("cursor"))
                    .and_then(IvyJson::as_js_string),
            )
            .await
            .map_err(Error::Protocol)
    } else {
        // Hive has no requestState verification hook. Strings resolve to
        // undefined; present non-strings reject before the application callback.
        if body
            .get("params")
            .and_then(|p| p.get("requestState"))
            .is_some_and(|v| v.as_js_string().is_none())
        {
            return Ok(protocol_error(
                modern,
                false,
                body,
                -32602,
                "Invalid or expired requestState".into(),
                Some(IvyJson::object([(
                    "reason",
                    "invalid_request_state".into(),
                )])),
            )?);
        }
        let Some(name) = params
            .and_then(|p| p.get("name"))
            .and_then(IvyJson::as_js_string)
        else {
            return Ok(protocol_error(
                modern,
                false,
                body,
                -32603,
                "Tool invocation parameters are unavailable.".into(),
                None,
            )?);
        };
        application
            .call_tool(name, params.and_then(|p| p.get("arguments")))
            .await
    };
    match outcome {
        Ok(result) => Ok(Reply::success(
            if modern {
                Encoding::Json
            } else {
                Encoding::Sse
            },
            body.get("id").unwrap().clone(),
            encode_result(method, result, modern, application.info()),
        )?),
        Err(Error::Protocol(failure)) => Ok(protocol_error(
            modern,
            false,
            body,
            failure.code,
            failure.message,
            Some(failure.data),
        )?),
        Err(Error::Ivy(failure)) => Ok(protocol_error(
            modern,
            false,
            body,
            -32603,
            failure.message_js(),
            None,
        )?),
    }
}

/// Already parsed HTTP header values. Construct a factory per request; no
/// protocol headers are inferred from authentication or application settings.
#[derive(Clone, Default)]
pub struct Headers {
    pub protocol_version: Option<JsString>,
    pub method: Option<JsString>,
    pub name: Option<JsString>,
    pub content_type: Option<JsString>,
    pub accept: Option<JsString>,
}
pub struct Factory<'a> {
    pub headers: Headers,
    pub runtime: &'a dyn mcp::Runtime,
}
pub struct Handler<'a> {
    headers: Headers,
    runtime: &'a dyn mcp::Runtime,
    closed: bool,
}
struct Prepared<'a> {
    application: &'a server_mcp::Application,
    runtime: &'a dyn mcp::Runtime,
}
impl Application for Prepared<'_> {
    fn info(&self) -> &IvyJson {
        &self.application.info
    }
    fn instructions(&self) -> &IvyJson {
        &self.application.instructions
    }
    async fn list_tools(&self, cursor: Option<&JsString>) -> Result<IvyJson, ProtocolError> {
        self.application.list_tools(cursor, self.runtime).await
    }
    async fn call_tool(
        &self,
        name: &JsString,
        arguments: Option<&IvyJson>,
    ) -> mcp_request::Result<IvyJson> {
        self.application
            .call_tool(name, arguments, self.runtime)
            .await
    }
}
impl<'a> server_mcp::Factory for Factory<'a> {
    type Handler = Handler<'a>;
    fn create(&mut self, _: &server_mcp::Application) -> mcp_request::Result<Self::Handler> {
        Ok(Handler {
            headers: self.headers.clone(),
            runtime: self.runtime,
            closed: false,
        })
    }
}
impl server_mcp::Handler for Handler<'_> {
    async fn handle(
        &mut self,
        application: &server_mcp::Application,
        response: &mut impl Response,
    ) -> mcp_request::Result<()> {
        if self.closed {
            return Err(IvyError::internal().into());
        }
        let request = inbound::Request {
            http_method: "POST",
            body: &application.body,
            protocol_version: self.headers.protocol_version.as_ref(),
            method: self.headers.method.as_ref(),
            name: self.headers.name.as_ref(),
            content_type: self.headers.content_type.as_ref(),
            accept: self.headers.accept.as_ref(),
        };
        let reply = reply(
            &request,
            &Prepared {
                application,
                runtime: self.runtime,
            },
        )
        .await?;
        if !response.writable_ended() && !response.destroyed() {
            response.write_head(reply.status, reply.headers)?;
            response.end(reply.body)?;
        }
        Ok(())
    }
    async fn close(&mut self) -> mcp_request::Result<()> {
        // No protocol tasks, timers, streams or sessions outlive this fully
        // awaited exchange. The HTTP owner retains the socket and lease.
        self.closed = true;
        Ok(())
    }
}
