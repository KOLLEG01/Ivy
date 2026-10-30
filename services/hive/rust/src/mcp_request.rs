//! Hive MCP request policy. The HTTP/MCP protocol adapter owns admission and close;
//! the existing storage client and kernel invocation own their asynchronous work.
use crate::{
    discovery::Discovery, mcp, registry::Registry, store::Store, storage::RequestOptions,
    storage::StorageClient,
};
use ivy_contracts::{
    ErrorOutcome, IvyError, IvyJson, IvyResult, JsString, Outcome, hash::hash_json,
    json::stringify, limits::MCP_DISCOVERY_RESULT_BYTES,
};
use std::{
    future::Future,
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Debug, Clone, PartialEq)]
pub struct ProtocolError {
    pub code: i32,
    pub message: JsString,
    pub data: IvyJson,
}
impl ProtocolError {
    pub fn to_json(&self) -> IvyJson {
        IvyJson::object([
            ("code", IvyJson::Number(self.code as f64)),
            ("message", IvyJson::String(self.message.clone())),
            ("data", self.data.clone()),
        ])
    }
}
#[derive(Debug, Clone, PartialEq)]
pub enum Error {
    Ivy(IvyError),
    Protocol(ProtocolError),
}
impl From<IvyError> for Error {
    fn from(error: IvyError) -> Self {
        Self::Ivy(error)
    }
}
pub type Result<T> = std::result::Result<T, Error>;

/// Already admitted surface and application settings, without credentials.
pub struct Options {
    pub surface: JsString,
    pub version: JsString,
    pub public_base_url: JsString,
    pub resource_metadata_url: JsString,
}
pub fn server_info(options: &Options) -> IvyResult<IvyJson> {
    let base = ivy_contracts::whatwg_url::parse_url(&options.public_base_url)?;
    let components = ivy_contracts::whatwg_url::url_components(&options.public_base_url)?;
    let development = options.surface == JsString::from("ivy_dev");
    let mut icon = components.origin;
    icon.push_str(
        components
            .pathname
            .strip_suffix('/')
            .unwrap_or(&components.pathname),
    );
    icon.push_str(if development {
        "/mcp-dev-icon.svg"
    } else {
        "/mcp-icon.svg"
    });
    Ok(IvyJson::object([
        ("name", IvyJson::String(options.surface.clone())),
        (
            "title",
            if development {
                "Ivy Development"
            } else {
                "Ivy"
            }
            .into(),
        ),
        ("version", IvyJson::String(options.version.clone())),
        (
            "description",
            if development {
                "Development and maintenance tools for Ivy."
            } else {
                "Personal back office with a wiki and connected services."
            }
            .into(),
        ),
        ("websiteUrl", base.href.into()),
        (
            "icons",
            IvyJson::Array(vec![IvyJson::object([
                ("src", icon.into()),
                ("mimeType", "image/svg+xml".into()),
                ("sizes", IvyJson::Array(vec!["any".into()])),
            ])]),
        ),
    ]))
}
pub fn measured_list_result(result: &IvyJson, server_info: &IvyJson) -> IvyJson {
    let mut result = result.clone();
    result.insert("ttlMs", IvyJson::Number(0.0));
    result.insert("cacheScope", "private".into());
    result.insert(
        "_meta",
        IvyJson::object([("io.modelcontextprotocol/serverInfo", server_info.clone())]),
    );
    result
}
pub fn failure(error: &IvyError, resource_metadata_url: &JsString) -> IvyResult<IvyJson> {
    let envelope = IvyJson::object([("error", error.to_wire())]);
    let mut result = IvyJson::object([
        ("isError", true.into()),
        (
            "content",
            IvyJson::Array(vec![IvyJson::object([
                ("type", "text".into()),
                ("text", stringify(&envelope, usize::MAX)?.into()),
            ])]),
        ),
        ("structuredContent", envelope),
    ]);
    if error.code_js() == JsString::from("unauthenticated") {
        let mut challenge = JsString::from("Bearer resource_metadata=\"").0;
        challenge.extend_from_slice(&resource_metadata_url.0);
        challenge.extend("\", error=\"invalid_token\", error_description=\"Authentication expired or is no longer valid\"".encode_utf16());
        result.insert(
            "_meta",
            IvyJson::object([(
                "mcp/www_authenticate",
                IvyJson::Array(vec![IvyJson::String(JsString(challenge))]),
            )]),
        );
    }
    Ok(result)
}
fn empty() -> IvyJson {
    IvyJson::object(std::iter::empty::<(&str, IvyJson)>())
}
fn string<'a>(value: &'a IvyJson, key: &str) -> IvyResult<&'a JsString> {
    value
        .get(key)
        .and_then(IvyJson::as_js_string)
        .ok_or_else(IvyError::internal)
}
fn slice(value: &JsString, maximum: usize) -> JsString {
    JsString(value.0.iter().copied().take(maximum).collect())
}
fn require(condition: bool, code: &str, message: &str) -> IvyResult<()> {
    if condition {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn safe_boundary(value: Option<&IvyJson>) -> IvyResult<f64> {
    let number = value.and_then(IvyJson::as_f64);
    require(
        number.is_some_and(|v| {
            v.is_finite() && v.fract() == 0.0 && (0.0..=9_007_199_254_740_991.0).contains(&v)
        }),
        "invalid_cursor",
        "Invalid MCP discovery boundary.",
    )?;
    Ok(number.unwrap())
}
/// The three original private worker actions; other actions remain with their owner.
pub fn worker_action(
    store: &Store,
    registry: &mut Registry,
    discovery: &mut Discovery,
    message: &IvyJson,
    runtime: &dyn mcp::Runtime,
    now: f64,
) -> Option<IvyResult<IvyJson>> {
    let action = message.get("action").and_then(|v| v.as_str().ok());
    match action.as_deref()? {
        "mcp.catalog" => Some(
            discovery
                .mcp_catalog(
                    store,
                    registry,
                    message.get("name").and_then(IvyJson::as_js_string),
                    runtime,
                    now,
                )
                .map(IvyJson::Array),
        ),
        "mcp.cursor.create" => Some((|| {
            Ok(IvyJson::object([(
                "cursor",
                store
                    .cursor(message.required("identity")?, message.required("offset")?)?
                    .into(),
            )]))
        })()),
        "mcp.cursor.read" => Some((|| {
            let offset =
                store.read_cursor(message.required("identity")?, string(message, "cursor")?)?;
            Ok(IvyJson::object([(
                "offset",
                IvyJson::Number(safe_boundary(offset.as_ref())?),
            )]))
        })()),
        _ => None,
    }
}
pub trait Storage {
    fn request(&mut self, message: IvyJson) -> impl Future<Output = IvyResult<IvyJson>>;
}
impl Storage for StorageClient {
    async fn request(&mut self, message: IvyJson) -> IvyResult<IvyJson> {
        StorageClient::request(self, message, RequestOptions::default())
            .await
    }
}
/// In-worker access to the same actions, also useful for isolated domain checks.
pub struct StoreAccess<'a> {
    pub store: &'a Store,
    pub registry: &'a mut Registry,
    pub discovery: &'a mut Discovery,
    pub runtime: &'a dyn mcp::Runtime,
    pub now: f64,
}
impl Storage for StoreAccess<'_> {
    async fn request(&mut self, message: IvyJson) -> IvyResult<IvyJson> {
        worker_action(
            self.store,
            self.registry,
            self.discovery,
            &message,
            self.runtime,
            self.now,
        )
        .ok_or_else(IvyError::internal)?
    }
}
async fn catalog(storage: &mut impl Storage, name: Option<&JsString>) -> IvyResult<Vec<IvyJson>> {
    let mut request = IvyJson::object([("action", "mcp.catalog".into())]);
    // The server's original bridge omits an empty name.
    if let Some(name) = name.filter(|v| !v.0.is_empty()) {
        request.insert("name", IvyJson::String(name.clone()));
    }
    storage.request(request).await?.into_array().ok_or_else(IvyError::internal)
}
async fn create_cursor(
    storage: &mut impl Storage,
    identity: &IvyJson,
    offset: usize,
) -> IvyResult<JsString> {
    let result = storage
        .request(IvyJson::object([
            ("action", "mcp.cursor.create".into()),
            ("identity", identity.clone()),
            ("offset", IvyJson::Number(offset as f64)),
        ]))
        .await?;
    string(&result, "cursor").cloned()
}
async fn list(
    storage: &mut impl Storage,
    options: &Options,
    server_info: &IvyJson,
    cursor: Option<&JsString>,
    runtime: &dyn mcp::Runtime,
) -> IvyResult<IvyJson> {
    let mut descriptors = {
        let bindings = mcp::surface_bindings(catalog(storage, None).await?, &options.surface)?;
        mcp::direct_descriptors(&bindings, runtime)?
    };
    if options.surface == JsString::from("ivy") { mcp::sort_common_tools(&mut descriptors); }
    let catalog = IvyJson::Array(descriptors);
    let catalog_hash = hash_json(&catalog)?;
    let descriptors = catalog.into_array().unwrap();
    let total = descriptors.len();
    let identity = IvyJson::object([
        ("method", "tools/list".into()),
        ("surface", IvyJson::String(options.surface.clone())),
        (
            "catalog",
            catalog_hash.into(),
        ),
    ]);
    let offset = if let Some(cursor) = cursor.filter(|v| !v.0.is_empty()) {
        let result = storage
            .request(IvyJson::object([
                ("action", "mcp.cursor.read".into()),
                ("identity", identity.clone()),
                ("cursor", IvyJson::String(cursor.clone())),
            ]))
            .await?;
        safe_boundary(result.get("offset"))?
    } else {
        0.0
    };
    require(
        offset <= total as f64,
        "invalid_cursor",
        "MCP discovery cursor does not match the current catalog.",
    )?;
    let offset = offset as usize;
    let mut with_cursor = IvyJson::object([("tools", IvyJson::Array(vec![]))]);
    if offset < total {
        with_cursor.insert(
            "nextCursor",
            IvyJson::String(create_cursor(storage, &identity, total).await?),
        );
    }
    let fixed_with = stringify(&measured_list_result(&with_cursor, server_info), usize::MAX)?.len();
    let fixed_without = stringify(
        &measured_list_result(
            &IvyJson::object([("tools", IvyJson::Array(vec![]))]),
            server_info,
        ),
        usize::MAX,
    )?
    .len();
    let mut tools = vec![];
    let mut tool_bytes = 0;
    for (index, descriptor) in descriptors.into_iter().enumerate().skip(offset) {
        let candidate =
            tool_bytes + usize::from(!tools.is_empty()) + stringify(&descriptor, usize::MAX)?.len();
        let fixed = if index + 1 < total {
            fixed_with
        } else {
            fixed_without
        };
        if fixed + candidate > MCP_DISCOVERY_RESULT_BYTES {
            break;
        }
        tools.push(descriptor);
        tool_bytes = candidate;
    }
    require(
        !tools.is_empty() || offset == total,
        "result_too_large",
        "One MCP descriptor exceeds the discovery page budget.",
    )?;
    let next_offset = offset + tools.len();
    let mut result = IvyJson::object([("tools", IvyJson::Array(tools))]);
    if next_offset < total {
        let cursor = create_cursor(storage, &identity, next_offset).await?;
        if !cursor.0.is_empty() {
            result.insert("nextCursor", IvyJson::String(cursor));
        }
    }
    Ok(result)
}
pub async fn list_tools(
    storage: &mut impl Storage,
    options: &Options,
    server_info: &IvyJson,
    cursor: Option<&JsString>,
    runtime: &dyn mcp::Runtime,
) -> std::result::Result<IvyJson, ProtocolError> {
    list(storage, options, server_info, cursor, runtime)
        .await
        .map_err(|error| ProtocolError {
            code: -32602,
            message: error.message_js(),
            data: IvyJson::object([("code", IvyJson::String(error.code_js()))]),
        })
}
pub fn request_id(body: &IvyJson) -> IvyJson {
    match body.get("id") {
        Some(IvyJson::Number(v))
            if v.is_finite() && v.fract() == 0.0 && v.abs() <= 9_007_199_254_740_991.0 =>
        {
            IvyJson::Number(*v)
        }
        Some(IvyJson::String(v)) => IvyJson::String(slice(v, 128)),
        _ => IvyJson::Null,
    }
}
#[derive(Debug, Clone)]
pub struct Observation {
    pub request_id: IvyJson,
    pub internal_request_ids: Vec<JsString>,
    pub tool: JsString,
    pub code: JsString,
    pub outcome: ErrorOutcome,
    pub latency_ms: f64,
    pub operation_identity: Option<JsString>,
}
impl Observation {
    pub fn to_json(&self) -> IvyJson {
        let mut result = IvyJson::object([
            ("requestId", self.request_id.clone()),
            (
                "internalRequestIds",
                IvyJson::Array(
                    self.internal_request_ids
                        .iter()
                        .take(2)
                        .cloned()
                        .map(IvyJson::String)
                        .collect(),
                ),
            ),
            ("tool", IvyJson::String(self.tool.clone())),
            ("code", IvyJson::String(self.code.clone())),
            ("outcome", self.outcome.to_json()),
            ("latencyMs", IvyJson::Number(self.latency_ms)),
        ]);
        if let Some(value) = self.operation_identity.as_ref().filter(|v| !v.0.is_empty()) {
            result.insert("operationIdentity", IvyJson::String(value.clone()));
        }
        result
    }
}
pub trait Invocation {
    fn new_id(&mut self) -> JsString;
    fn now_ms(&mut self) -> f64;
    fn invoke(&mut self, envelope: IvyJson) -> impl Future<Output = Result<IvyJson>>;
    fn observe(&mut self, observation: Observation) -> Result<()>;
    fn observing(&self) -> bool {
        true
    }
}
/// The server binds its actual context/invocation and optional observation sink.
/// IDs and wall clock use native primitives; neither callback supplies MCP policy.
pub struct NativeInvocation<F, O> {
    pub invoke: F,
    pub observe: O,
}
impl<F, O, T> Invocation for NativeInvocation<F, O>
where
    F: FnMut(IvyJson) -> T,
    T: Future<Output = Result<IvyJson>>,
    O: FnMut(Observation) -> Result<()>,
{
    fn new_id(&mut self) -> JsString {
        uuid::Uuid::new_v4().to_string().into()
    }
    fn now_ms(&mut self) -> f64 {
        match SystemTime::now().duration_since(UNIX_EPOCH) {
            Ok(value) => value.as_millis() as f64,
            Err(value) => -(value.duration().as_millis() as f64),
        }
    }
    async fn invoke(&mut self, envelope: IvyJson) -> Result<IvyJson> {
        (self.invoke)(envelope).await
    }
    fn observe(&mut self, observation: Observation) -> Result<()> {
        (self.observe)(observation)
    }
}
fn envelope(id: JsString, method: &str, params: IvyJson) -> IvyJson {
    IvyJson::object([
        ("jsonrpc", "2.0".into()),
        ("id", IvyJson::String(id)),
        ("method", method.into()),
        ("params", params),
    ])
}
pub async fn instructions(
    options: &Options,
    body: &IvyJson,
    invocation: &mut impl Invocation,
) -> Result<IvyJson> {
    if options.surface == JsString::from("ivy_dev") {
        return Ok(IvyJson::String(
            JsString::from(include_str!(
                "../../../../instructions/ivy-dev-mcp-instructions.md"
            ))
            .trim(),
        ));
    }
    let method = body.get("method").and_then(IvyJson::as_js_string);
    if method == Some(&JsString::from("initialize"))
        || method == Some(&JsString::from("server/discover"))
    {
        let id = invocation.new_id();
        let result = invocation
            .invoke(envelope(id, "discovery.instructions", empty()))
            .await?;
        return result
            .get("instructions")
            .cloned()
            .ok_or_else(|| IvyError::internal().into());
    }
    Ok(IvyJson::String(
        JsString::from(include_str!(
            "../../../../instructions/hive-mcp-instructions.md"
        ))
        .trim(),
    ))
}
pub async fn call_tool(
    storage: &mut impl Storage,
    options: &Options,
    body: &IvyJson,
    name: &JsString,
    arguments: Option<&IvyJson>,
    runtime: &dyn mcp::Runtime,
    invocation: &mut impl Invocation,
) -> Result<IvyJson> {
    let args = arguments
        .filter(|v| !v.is_null())
        .cloned()
        .unwrap_or_else(empty);
    let started = invocation.now_ms();
    let operation = args
        .get("operationId")
        .filter(|v| !v.is_null())
        .or_else(|| args.get("mutationId").filter(|v| !v.is_null()))
        .or_else(|| args.get("input").and_then(|v| v.get("operationId")))
        .and_then(IvyJson::as_js_string)
        .map(|v| slice(v, 512));
    let mut code = JsString::from("internal_error");
    let mut outcome: ErrorOutcome = Outcome::Unknown.into();
    let mut internal = vec![];
    let result = async {
        let bindings =
            mcp::surface_bindings(catalog(storage, Some(name)).await?, &options.surface)?;
        if bindings.is_empty() {
            code = "method_not_found".into();
            outcome = Outcome::NotExecuted.into();
            return Err(Error::Protocol(ProtocolError {
                code: -32601,
                message: "Unknown Hive MCP tool.".into(),
                data: IvyJson::object([("tool", IvyJson::String(slice(name, 128)))]),
            }));
        }
        // Evaluate directCall before allocating the invocation ID, and evaluate
        // output projection only after the real discovery.call completes.
        let params = mcp::direct_call(&bindings, &args, runtime)?;
        let id = invocation.new_id();
        internal.push(id.clone());
        let result = invocation
            .invoke(envelope(id, "discovery.call", params))
            .await?;
        Ok(mcp::success(
            result,
            mcp::has_structured_output(&bindings)?,
        )?)
    }
    .await;
    let result = match result {
        Ok(value) => {
            code = "ok".into();
            outcome = Outcome::Completed.into();
            Ok(value)
        }
        Err(Error::Ivy(error)) => {
            code = error.code_js();
            outcome = error.outcome.clone();
            failure(&error, &options.resource_metadata_url).map_err(Error::from)
        }
        other => other,
    };
    if invocation.observing() {
        let latency = invocation.now_ms() - started;
        invocation.observe(Observation {
            request_id: request_id(body),
            internal_request_ids: internal,
            tool: slice(name, 128),
            code,
            outcome,
            latency_ms: latency.clamp(0.0, 86_400_000.0),
            operation_identity: operation,
        })?;
    }
    result
}

#[cfg(test)]
#[path = "../../../../tests/rust/hive_mcp_delivery_ownership.rs"]
mod ownership_tests;
