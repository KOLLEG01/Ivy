//! Public OAuth metadata and JSON registration after outer HTTP admission.
use crate::{
    admission::RequestLease,
    http_auth::{Connection, Headers as AuthenticationHeaders},
    http_body::{BodyBuffers, BodyStream},
    http_response::{self, Response},
    oauth::{HiveOAuth, Parameters},
};
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString, json::parse};

pub struct Headers<'a> {
    pub authentication: &'a AuthenticationHeaders,
    pub content_type: Option<&'a IvyJson>,
    pub content_length: Option<&'a IvyJson>,
}
pub struct Handler<'a> {
    pub connection: &'a Connection,
    pub oauth: &'a HiveOAuth,
    pub buffers: &'a BodyBuffers,
}
impl Handler<'_> {
    /// Caller selected GET /oauth/authorize and decoded its canonical search
    /// parameters. Rendering stays inside the same authentication admission.
    pub fn authorize_page(
        &self,
        headers: &AuthenticationHeaders,
        query: &Parameters,
        response: &mut impl Response,
    ) -> IvyResult<()> {
        let mut admission = self.connection.admit_authentication(headers)?;
        let result = (|| {
            let pending = self.oauth.begin(query)?;
            crate::auth_pages::html(
                response,
                200,
                &crate::auth_pages::oauth(
                    self.connection.authentication().base_path(),
                    pending.get("transaction").unwrap().as_js_string().unwrap(),
                    pending.get("clientName").unwrap().as_js_string().unwrap(),
                    pending.get("scope").unwrap().as_js_string().unwrap(),
                    false,
                ),
            )
        })();
        admission.release();
        result
    }
    /// Pathname is the canonical URL owner's pathname getter. The HTTP owner
    /// keeps its outer request lease and drives this future through completion,
    /// including after socket disconnect. False leaves other routes to that
    /// owner; this module does not apply their base-path or maintenance guards.
    pub async fn try_handle(
        &self,
        method: Option<&JsString>,
        pathname: &JsString,
        headers: Headers<'_>,
        body: &mut impl BodyStream,
        lease: Option<&mut RequestLease>,
        response: &mut impl Response,
    ) -> IvyResult<bool> {
        let authentication = self.connection.authentication();
        let base_path = authentication.base_path();
        let get = method == Some(&JsString::from("GET"));
        // This metadata route precedes the installation's base-path guard.
        if get && *pathname == JsString::from("/.well-known/oauth-authorization-server") {
            http_response::json(
                response,
                200,
                &self.oauth.authorization_server_metadata(base_path)?,
            )?;
            return Ok(true);
        }
        let mut prefix = base_path.clone();
        prefix.0.push(47);
        let path = if pathname == base_path {
            JsString::from("/")
        } else if pathname.0.starts_with(&prefix.0) {
            JsString(pathname.0[base_path.0.len()..].to_vec())
        } else {
            return Ok(false);
        };
        if get {
            let metadata = if path == JsString::from("/.well-known/oauth-authorization-server") {
                Some(self.oauth.authorization_server_metadata(base_path)?)
            } else if path == JsString::from("/.well-known/oauth-protected-resource") {
                Some(self.oauth.protected_resource_metadata(None)?)
            } else if path == JsString::from("/.well-known/oauth-protected-resource/mcp-dev") {
                Some(
                    self.oauth
                        .protected_resource_metadata(Some(self.oauth.development_resource()))?,
                )
            } else {
                None
            };
            if let Some(metadata) = metadata {
                http_response::json(response, 200, &metadata)?;
                return Ok(true);
            }
        }
        if method != Some(&JsString::from("POST")) || path != JsString::from("/oauth/register") {
            return Ok(false);
        }
        // Registration shares authentication arrivals/slots, but intentionally
        // has no browser Origin check. Admission precedes the content-type guard.
        let mut admission = self
            .connection
            .admit_authentication(headers.authentication)?;
        let result = async {
            require_json(headers.content_type)?;
            let bytes = self
                .buffers
                .read(body, headers.content_length, 32 * 1024, lease)
                .await?;
            // TextDecoder's fatal UTF-8 mode removes one initial BOM. Parse
            // failures share the source's redacted invalid_frame result.
            let text = std::str::from_utf8(&bytes).map_err(|_| malformed())?;
            let text = text.strip_prefix('\u{feff}').unwrap_or(text);
            let input = parse(text, usize::MAX).map_err(|_| malformed())?;
            let client = self.oauth.register(&input)?;
            http_response::json(response, 201, &client)
        }
        .await;
        // Response suppression/destruction and callback failures are inside the
        // original finally, just like body, parser and registration failures.
        admission.release();
        result.map(|_| true)
    }
}
fn require_json(value: Option<&IvyJson>) -> IvyResult<()> {
    let valid = match value {
        None | Some(IvyJson::Null) => false,
        Some(IvyJson::String(value)) => {
            value.0.split(|v| *v == 59).next()
                == Some(JsString::from("application/json").0.as_slice())
        }
        Some(_) => return Err(IvyError::internal()),
    };
    if valid {
        Ok(())
    } else {
        Err(IvyError::new(
            "invalid_arguments",
            "OAuth registration requires JSON.",
        ))
    }
}
fn malformed() -> IvyError {
    IvyError::new("invalid_frame", "Malformed JSON input.")
}
