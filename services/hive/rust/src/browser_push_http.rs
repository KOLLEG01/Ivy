//! POST /api/browser-push after path selection and bounded authentication.
use crate::{
    admission::RequestLease,
    browser_push::{BrowserPush, Worker},
    http_body::{BodyBuffers, BodyStream},
    http_response::{self, Response},
};
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString, json::parse};

pub trait Wake {
    fn wake(&self);
}
impl Wake for BrowserPush {
    fn wake(&self) {
        BrowserPush::wake(self);
    }
}
pub struct Headers<'a> {
    pub origin: Option<&'a IvyJson>,
    pub content_length: Option<&'a IvyJson>,
}
pub struct Handler<'a> {
    pub canonical_origin: &'a JsString,
    pub buffers: &'a BodyBuffers,
    pub worker: &'a dyn Worker,
    pub push: &'a dyn Wake,
}
impl Handler<'_> {
    /// The caller supplies its captured authentication context, keeps the outer
    /// request lease until completion, and does not refresh the session cookie
    /// on this route (the source returns before its cookie-refresh branch).
    pub async fn handle(
        &self,
        context: &IvyJson,
        headers: Headers<'_>,
        body: &mut impl BodyStream,
        lease: Option<&mut RequestLease>,
        response: &mut impl Response,
    ) -> IvyResult<()> {
        crate::http_auth::same_origin(self.canonical_origin, headers.origin)?;
        let bytes = self
            .buffers
            .read(body, headers.content_length, 8192, lease)
            .await?;
        // This route uses Buffer.toString, unlike RPC's fatal UTF-8 decoder. Its
        // JSON.parse failures are ordinary exceptions, redacted by the HTTP owner.
        let input = parse(&String::from_utf8_lossy(&bytes), usize::MAX)
            .map_err(|_| IvyError::internal())?;
        let action = input.get("action").and_then(IvyJson::as_js_string);
        if !matches!(
            action.and_then(|v| v.to_utf8().ok()).as_deref(),
            Some("status" | "subscribe" | "unsubscribe" | "test")
        ) {
            return Err(IvyError::new(
                "invalid_arguments",
                "Invalid browser push action.",
            ));
        }
        let result = self
            .worker
            .request(IvyJson::object([
                ("action", "browserPush.request".into()),
                ("context", context.clone()),
                ("request", input.clone()),
            ]))
            .await?;
        http_response::json(response, 200, &result)?;
        if action == Some(&JsString::from("test")) {
            self.push.wake();
        }
        Ok(())
    }
}
