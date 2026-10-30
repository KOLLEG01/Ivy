//! Per-request MCP application bindings. Protocol machinery remains an explicit
//! adapter; no version, subscription, session or transport defaults are supplied.
use crate::{
    browser_push::Worker,
    http_auth::Headers as AuthHeaders,
    http_body::BodyStream,
    http_response::{self, Response},
    mcp,
    mcp_request::{self, Observation},
    oauth::HiveOAuth,
    rpc,
    server_http::Request,
    server_http_rpc,
    server_options::BaseUrl,
    server_peers::Ids,
    server_rpc::{Clock, Invocation},
};
use ivy_contracts::{IvyJson, IvyResult, JsString};
use std::sync::Arc;

pub type Observer = Arc<dyn Fn(Observation) -> mcp_request::Result<()> + Send + Sync>;
#[derive(Clone, Copy)]
pub enum Surface {
    Public,
    Development,
}
impl Surface {
    fn at(path: &JsString) -> Option<Self> {
        if path == &JsString::from("/mcp") {
            Some(Self::Public)
        } else if path == &JsString::from("/mcp-dev") {
            Some(Self::Development)
        } else {
            None
        }
    }
    fn name(self) -> JsString {
        match self {
            Self::Public => "ivy",
            Self::Development => "ivy_dev",
        }
        .into()
    }
}
struct Parts {
    worker: Arc<dyn Worker>,
    invocation: Invocation,
    oauth: HiveOAuth,
    version: JsString,
    public_base_url: JsString,
    base: BaseUrl,
    ids: Ids,
    now: Clock,
    observe: Option<Observer>,
}
#[derive(Clone)]
pub struct Mcp(Arc<Parts>);
pub struct Options {
    pub version: JsString,
    pub public_base_url: JsString,
    pub base: BaseUrl,
    pub ids: Ids,
    pub now: Clock,
    pub observe: Option<Observer>,
}
impl Mcp {
    pub(crate) fn new(
        worker: Arc<dyn Worker>,
        invocation: Invocation,
        oauth: HiveOAuth,
        options: Options,
    ) -> Self {
        Self(Arc::new(Parts {
            worker,
            invocation,
            oauth,
            version: options.version,
            public_base_url: options.public_base_url,
            base: options.base,
            ids: options.ids,
            now: options.now,
            observe: options.observe,
        }))
    }
    fn resource(&self, surface: Surface) -> JsString {
        match surface {
            Surface::Public => self.0.oauth.resource(),
            Surface::Development => self.0.oauth.development_resource(),
        }
        .clone()
    }
    async fn prepare(
        &self,
        surface: Surface,
        context: IvyJson,
        body: IvyJson,
        received_bytes: usize,
    ) -> mcp_request::Result<Application> {
        let mut metadata = self.0.base.origin.clone();
        metadata.0.extend(&self.0.base.base_path.0);
        metadata
            .0
            .extend("/.well-known/oauth-protected-resource".encode_utf16());
        if matches!(surface, Surface::Development) {
            metadata.0.extend("/mcp-dev".encode_utf16());
        }
        let mut app = Application {
            owner: self.clone(),
            body,
            context,
            received_bytes,
            options: mcp_request::Options {
                surface: surface.name(),
                version: self.0.version.clone(),
                public_base_url: self.0.public_base_url.clone(),
                resource_metadata_url: metadata,
            },
            info: IvyJson::Null,
            instructions: IvyJson::Null,
        };
        app.instructions =
            mcp_request::instructions(&app.options, &app.body, &mut Calls(&app)).await?;
        app.info = mcp_request::server_info(&app.options)?;
        Ok(app)
    }
}
pub struct Application {
    owner: Mcp,
    pub body: IvyJson,
    pub context: IvyJson,
    pub received_bytes: usize,
    pub options: mcp_request::Options,
    pub info: IvyJson,
    pub instructions: IvyJson,
}
struct Storage(Arc<dyn Worker>);
impl mcp_request::Storage for Storage {
    async fn request(&mut self, message: IvyJson) -> IvyResult<IvyJson> {
        self.0.request(message.into()).await
    }
}
struct Calls<'a>(&'a Application);
impl mcp_request::Invocation for Calls<'_> {
    fn new_id(&mut self) -> JsString {
        (self.0.owner.0.ids)()
    }
    fn now_ms(&mut self) -> f64 {
        (self.0.owner.0.now)()
    }
    async fn invoke(&mut self, envelope: IvyJson) -> mcp_request::Result<IvyJson> {
        self.0
            .owner
            .0
            .invocation
            .invoke(
                self.0.context.clone(),
                envelope,
                self.0.received_bytes,
            )
            .await
            .map_err(Into::into)
    }
    fn observe(&mut self, value: Observation) -> mcp_request::Result<()> {
        match &self.0.owner.0.observe {
            Some(observe) => observe(value),
            None => Ok(()),
        }
    }
    fn observing(&self) -> bool {
        self.0.owner.0.observe.is_some()
    }
}
impl Application {
    pub fn configuration(&self) -> IvyJson {
        IvyJson::object([
            (
                "capabilities",
                IvyJson::object([("tools", IvyJson::object([] as [(&str, IvyJson); 0]))]),
            ),
            ("instructions", self.instructions.clone()),
            (
                "cacheHints",
                IvyJson::object([
                    (
                        "server/discover",
                        IvyJson::object([
                            ("ttlMs", IvyJson::Number(0.0)),
                            ("cacheScope", "private".into()),
                        ]),
                    ),
                    (
                        "tools/list",
                        IvyJson::object([
                            ("ttlMs", IvyJson::Number(0.0)),
                            ("cacheScope", "private".into()),
                        ]),
                    ),
                ]),
            ),
        ])
    }
    pub fn protocol_options(&self) -> IvyJson {
        IvyJson::object([
            ("legacy", "stateless".into()),
            ("responseMode", "json".into()),
        ])
    }
    pub async fn list_tools(
        &self,
        cursor: Option<&JsString>,
        runtime: &dyn mcp::Runtime,
    ) -> Result<IvyJson, mcp_request::ProtocolError> {
        mcp_request::list_tools(
            &mut Storage(self.owner.0.worker.clone()),
            &self.options,
            &self.info,
            cursor,
            runtime,
        )
        .await
    }
    pub async fn call_tool(
        &self,
        name: &JsString,
        arguments: Option<&IvyJson>,
        runtime: &dyn mcp::Runtime,
    ) -> mcp_request::Result<IvyJson> {
        mcp_request::call_tool(
            &mut Storage(self.owner.0.worker.clone()),
            &self.options,
            &self.body,
            name,
            arguments,
            runtime,
            &mut Calls(self),
        )
        .await
    }
}
/// A fresh handler follows application instruction/info preparation. The native
/// adapter must qualify SDK negotiation/schema/HTTP behavior before deployment.
pub trait Factory {
    type Handler: Handler;
    fn create(&mut self, application: &Application) -> mcp_request::Result<Self::Handler>;
}
pub trait Handler {
    fn handle(
        &mut self,
        application: &Application,
        response: &mut impl Response,
    ) -> impl std::future::Future<Output = mcp_request::Result<()>>;
    fn close(&mut self) -> impl std::future::Future<Output = mcp_request::Result<()>>;
}
pub async fn handle(
    request: &mut Request<'_>,
    owner: &Mcp,
    method: Option<&JsString>,
    path: &JsString,
    authentication: &AuthHeaders,
    headers: server_http_rpc::Headers<'_>,
    rpc_id: &mut IvyJson,
    stream: &mut impl BodyStream,
    response: &mut impl Response,
    factory: &mut impl Factory,
) -> mcp_request::Result<bool> {
    let Some(surface) = Surface::at(path) else {
        return Ok(false);
    };
    request.admit_public_route(method, path)?;
    let context = request
        .authenticate_mcp(
            method.cloned(),
            authentication,
            owner.resource(surface),
            response,
        )
        .await?;
    if method != Some(&JsString::from("POST")) {
        response.set_header("Allow", &"POST".into())?;
        http_response::json(
            response,
            405,
            &IvyJson::object([("error", "method_not_allowed".into())]),
        )?;
        return Ok(true);
    }
    let (body, received_bytes) = server_http_rpc::read(request, headers, stream).await?;
    server_http_rpc::single(&body)?;
    *rpc_id = rpc::valid_request_id(body.get("id"));
    let application = owner
        .prepare(surface, context, body, received_bytes)
        .await?;
    let mut handler = factory.create(&application)?;
    let result = handler.handle(&application, response).await;
    // A finally rejection supersedes the handler result in the source.
    handler.close().await?;
    result?;
    Ok(true)
}
