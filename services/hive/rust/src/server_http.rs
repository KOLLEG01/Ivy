//! HTTP connection/request lifetime over the server's actual shared owners.
//! The transport drives accepted work to completion and supplies response I/O.
use crate::{
    admission::RequestBudget,
    auth_pages::SessionResponse,
    browser_push::{RequestFuture, Worker},
    browser_session::Session,
    http_auth::{self, Authentication, Headers},
    http_body::{BodyBuffers, BodyStream},
    http_common::{self, Requests, Route},
    http_response::Response,
    server_rpc::{Invocation, Reply},
};
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString};
use std::sync::Arc;

struct Owners {
    worker: Arc<dyn Worker>,
    authentication: Authentication,
    requests: Requests,
    buffers: BodyBuffers,
    invocation: Invocation,
    reset_bootstrap: bool,
}
#[derive(Clone)]
pub struct Http(Arc<Owners>);
impl Http {
    pub(crate) fn new(
        worker: Arc<dyn Worker>,
        authentication: Authentication,
        invocation: Invocation,
        requests: RequestBudget,
        reset_bootstrap: bool,
    ) -> Self {
        Self(Arc::new(Owners {
            worker,
            authentication,
            requests: Requests::new(requests),
            buffers: BodyBuffers::default(),
            invocation,
            reset_bootstrap,
        }))
    }
    /// Create once per actual socket. The transport's stable socket identity is
    /// shared by all its requests; connection clones retain its authentication.
    pub fn connection(&self, socket: u64, remote_address: Option<JsString>) -> Connection {
        Connection(Arc::new(Socket {
            http: self.clone(),
            socket,
            authentication: self.0.authentication.connection(remote_address),
        }))
    }
    pub fn buffered_bytes(&self) -> f64 {
        self.0.buffers.buffered_bytes()
    }
}
struct Socket {
    http: Http,
    socket: u64,
    authentication: http_auth::Connection,
}
#[derive(Clone)]
pub struct Connection(Arc<Socket>);
impl Connection {
    pub fn authentication(&self) -> &http_auth::Connection {
        &self.0.authentication
    }
    pub fn active_requests(&self) -> usize {
        self.0.http.0.requests.active(self.0.socket)
    }
    /// Header failure occurs before the active count changes. Call admit once
    /// inside the handler's error-response/finally scope; keep this request alive
    /// through that response even when client-address or reservation checks fail.
    pub fn begin(&self, response: &mut impl Response) -> IvyResult<Request<'_>> {
        Ok(Request {
            connection: self,
            scope: self.0.http.0.requests.begin(self.0.socket, response)?,
        })
    }
    /// Socket close clears identity caches. Accepted request/body/invocation
    /// work and its reservations continue until their owning request completes.
    pub fn close(&self) {
        self.0.authentication.close();
    }
}
pub struct Request<'a> {
    connection: &'a Connection,
    scope: http_common::Request,
}
impl Request<'_> {
    pub fn admit(&mut self, headers: &Headers) -> IvyResult<()> {
        self.scope
            .admit(&self.connection.authentication().client_address(headers)?)
    }
    pub fn route(&self, method: Option<&JsString>, raw_url: Option<&JsString>) -> IvyResult<Route> {
        let authentication = self.connection.authentication().authentication();
        http_common::route(
            method,
            raw_url,
            authentication.canonical_origin(),
            authentication.base_path(),
        )
    }
    pub fn authenticate(&self, request: http_auth::Request) -> RequestFuture {
        self.connection.authentication().authenticate(request)
    }
    /// The public-access guard follows routes that remain available during
    /// reset (metadata, health and uploads), before login/authentication work.
    pub fn admit_public_route(&self, method: Option<&JsString>, path: &JsString) -> IvyResult<()> {
        if self.connection.0.http.0.reset_bootstrap
            && !(method == Some(&JsString::from("POST")) && path == &JsString::from("/api/v1/rpc"))
        {
            return Err(IvyError::new(
                "maintenance_active",
                "Hive public access remains held during runtime reset bootstrap.",
            ));
        }
        Ok(())
    }
    /// Normal authenticated HTTP prefix. Browser-push returns before cookie
    /// refresh and therefore uses authenticate directly. Keep driving accepted
    /// work through completion even after the transport disconnects.
    pub async fn authenticate_http(
        &self,
        method: Option<JsString>,
        headers: &Headers,
        response: &mut impl Response,
    ) -> IvyResult<IvyJson> {
        let context = self
            .authenticate(http_auth::Request {
                method,
                headers: headers.clone(),
                transport: "http".into(),
                resource: None,
            })
            .await?;
        self.refresh_cookie(headers, &context, response)?;
        Ok(context)
    }
    pub async fn authenticate_mcp(
        &self,
        method: Option<JsString>,
        headers: &Headers,
        resource: JsString,
        response: &mut impl Response,
    ) -> IvyResult<IvyJson> {
        let context = self
            .authenticate(http_auth::Request {
                method,
                headers: headers.clone(),
                transport: "mcp".into(),
                resource: Some(resource),
            })
            .await?;
        http_auth::mcp_origin(
            self.connection
                .authentication()
                .authentication()
                .canonical_origin(),
            headers.origin.as_ref(),
        )?;
        self.refresh_cookie(headers, &context, response)?;
        Ok(context)
    }
    fn refresh_cookie(
        &self,
        headers: &Headers,
        context: &IvyJson,
        response: &mut impl Response,
    ) -> IvyResult<()> {
        let connection = self.connection.authentication();
        Session {
            connection,
            worker: self.connection.0.http.0.worker.as_ref(),
        }
        .refresh_cookie(
            headers,
            context,
            &mut SessionResponse {
                connection,
                response,
            },
        )
    }
    pub async fn body(
        &mut self,
        stream: &mut impl BodyStream,
        declared: Option<&IvyJson>,
        limit: usize,
    ) -> IvyResult<Vec<u8>> {
        self.connection
            .0
            .http
            .0
            .buffers
            .read(stream, declared, limit, self.scope.body_lease())
            .await
    }
    pub fn invoke(&self, context: IvyJson, request: IvyJson, received_bytes: f64) -> Reply {
        self.connection
            .0
            .http
            .0
            .invocation
            .invoke(context, request, received_bytes)
    }
    pub fn batch(
        &self,
        context: IvyJson,
        requests: Vec<IvyJson>,
        received_bytes: f64,
    ) -> Reply<String> {
        self.connection
            .0
            .http
            .0
            .invocation
            .batch(context, requests, received_bytes)
    }
    /// Original finally boundary, after success/error response effects. Drop
    /// also releases the same scope without canceling any posted invocation.
    pub fn release(&mut self) {
        self.scope.release();
    }
    /// Retain the request's admission until its transport response completes or
    /// is dropped, including a streamed artifact after handler dispatch ends.
    pub fn into_response_lease(self) -> http_common::Request {
        self.scope
    }
}
