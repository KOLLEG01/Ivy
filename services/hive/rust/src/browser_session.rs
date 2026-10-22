//! Browser session policy after HTTP path/content-type/body admission. The
//! transport owns page rendering, response I/O and the live WebSocket peer set.
use crate::{
    admission::RequestLease,
    browser_push::Worker,
    http_auth::{self, Connection, Headers},
    http_body::{BodyBuffers, BodyStream, require_form},
    oauth::Parameters,
};
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString,
    hash::digest,
    whatwg_url::{parse_url_with_base, url_components},
};
use std::sync::Arc;

pub trait Response {
    fn set_cookie(&mut self, value: &JsString) -> IvyResult<()>;
    fn redirect(&mut self, target: &JsString) -> IvyResult<()>;
    fn login_rejected(&mut self, target: &JsString) -> IvyResult<()>;
}
pub trait Peer: Send + Sync {
    fn context(&self) -> &IvyJson;
    fn terminate(&self) -> IvyResult<()>;
}
pub trait Peers {
    /// Supply current transport peers without retaining a registry lock across
    /// terminate(). Native transports queue termination on their I/O executor.
    fn iter(&self) -> Box<dyn Iterator<Item = Arc<dyn Peer>> + '_>;
}
pub struct Session<'a> {
    pub connection: &'a Connection,
    pub worker: &'a dyn Worker,
}
pub struct LoginHeaders<'a> {
    pub authentication: &'a Headers,
    pub content_type: Option<&'a IvyJson>,
    pub content_length: Option<&'a IvyJson>,
}
impl Session<'_> {
    /// Caller selected POST /login after the outer maintenance/path guards.
    /// Unlike OAuth, login reads the form before authentication admission.
    pub async fn login_post(
        &self,
        headers: LoginHeaders<'_>,
        buffers: &BodyBuffers,
        body: &mut impl BodyStream,
        lease: Option<&mut RequestLease>,
        response: &mut impl Response,
    ) -> IvyResult<()> {
        http_auth::same_origin(
            self.connection.authentication().canonical_origin(),
            headers.authentication.origin.as_ref(),
        )?;
        require_form(headers.content_type, "Login requires a form POST.")?;
        let bytes = buffers
            .read(body, headers.content_length, 8192, lease)
            .await?;
        // Buffer.toString('utf8') replaces malformed input and preserves a BOM.
        let text = String::from_utf8_lossy(&bytes);
        let form = Parameters::parse_form(&text.as_ref().into());
        self.login(headers.authentication, &form, response).await
    }
    /// The HTTP owner has already checked exact Origin and form content type,
    /// read the bounded8KiB body, and used the canonical form decoder. It must
    /// keep driving this future until completion when a socket disconnects.
    pub async fn login(
        &self,
        headers: &Headers,
        form: &Parameters,
        response: &mut impl Response,
    ) -> IvyResult<()> {
        let token = form.get("token");
        require(
            token.is_some_and(|v| !v.0.is_empty() && v.0.len() <= 4096),
            "unauthenticated",
            "A Hive credential is required.",
        )?;
        let owner = self.connection.authentication();
        let target = return_path(
            owner.canonical_origin(),
            owner.base_path(),
            form.get("returnTo"),
        )?;
        let mut admission = self.connection.admit_authentication(headers)?;
        let result = self
            .worker
            .request(IvyJson::object([
                ("action", "session.create".into()),
                (
                    "credentialDigest",
                    digest(String::from_utf16_lossy(&token.unwrap().0)).into(),
                ),
            ]))
            .await;
        let result = match result {
            Err(error) if error.code == "unauthenticated" => {
                response.login_rejected(&target).map(|_| None)
            }
            Err(error) => Err(error),
            Ok(value) => Ok(Some(value)),
        };
        // Rejection rendering belongs inside the original catch/finally. A
        // successful response's cookie/redirect belongs after that finally.
        admission.release();
        let Some(session) = result? else {
            return Ok(());
        };
        let token = session
            .get("sessionToken")
            .and_then(IvyJson::as_js_string)
            .filter(|token| !token.0.is_empty())
            .ok_or_else(IvyError::internal)?;
        response.set_cookie(&owner.cookie(token, false))?;
        response.redirect(&target)
    }
    /// The shared authenticated-HTTP prefix. Browser-push returns before this
    /// prefix; other routes call it once before their route-specific policy.
    pub fn refresh_cookie(
        &self,
        headers: &Headers,
        context: &IvyJson,
        response: &mut impl Response,
    ) -> IvyResult<()> {
        if context.is_null() {
            return Err(IvyError::internal());
        }
        if context
            .get("sessionDigest")
            .and_then(IvyJson::as_js_string)
            .is_some_and(|value| !value.0.is_empty())
        {
            if let Some(token) =
                http_auth::session_token(headers.cookie.as_ref()).filter(|v| !v.0.is_empty())
            {
                response.set_cookie(&self.connection.authentication().cookie(&token, false))?;
            }
        }
        Ok(())
    }
    /// Includes the authenticated-HTTP cookie prefix. The caller has already
    /// captured a bounded authentication context and selected POST /logout.
    /// Keep the future alive through worker completion and peer/response I/O.
    pub async fn logout(
        &self,
        headers: &Headers,
        context: &IvyJson,
        peers: &impl Peers,
        response: &mut impl Response,
    ) -> IvyResult<()> {
        self.refresh_cookie(headers, context, response)?;
        let owner = self.connection.authentication();
        http_auth::same_origin(owner.canonical_origin(), headers.origin.as_ref())?;
        self.worker
            .request(IvyJson::object([
                ("action", "session.end".into()),
                ("context", context.clone()),
            ]))
            .await?;
        owner.reset();
        let credential = context
            .get("credentialDigest")
            .and_then(IvyJson::as_js_string)
            .ok_or_else(IvyError::internal)?;
        for peer in peers.iter() {
            let peer_context = peer.context();
            if peer_context
                .get("sessionDigest")
                .and_then(IvyJson::as_js_string)
                .is_some_and(|value| !value.0.is_empty())
                && peer_context
                    .get("credentialDigest")
                    .and_then(IvyJson::as_js_string)
                    == Some(credential)
            {
                peer.terminate()?;
            }
        }
        response.set_cookie(&owner.cookie(&"".into(), true))?;
        let mut target = owner.base_path().clone();
        target.0.extend("/login".encode_utf16());
        response.redirect(&target)
    }
}
/// Preserve the original URL constructor and pathname/search/hash getters.
/// Origin/base path come from the HTTP owner's validated public base URL.
pub fn return_path(
    origin: &JsString,
    base_path: &JsString,
    input: Option<&JsString>,
) -> IvyResult<JsString> {
    let Some(input) = input.filter(|v| !v.0.is_empty()) else {
        let mut result = base_path.clone();
        result.0.push(47);
        return Ok(result);
    };
    let invalid = || {
        IvyError::new(
            "invalid_arguments",
            "Login return path must stay inside Hive.",
        )
    };
    if !input.0.starts_with(&[47]) || input.0.starts_with(&[47, 47]) || input.0.contains(&92) {
        return Err(invalid());
    }
    let destination = parse_url_with_base(input, origin)?;
    let components = url_components(&destination.href.clone().into())?;
    let path = JsString::from(components.pathname.clone());
    let mut prefix = base_path.clone();
    prefix.0.push(47);
    if JsString::from(components.origin) != *origin
        || !(path == *base_path || path.0.starts_with(&prefix.0))
    {
        return Err(invalid());
    }
    // In a serialized HTTP URL, the first '?' before its fragment is the query
    // delimiter, including URLs whose constructor admitted user information.
    // Empty query/fragment delimiters are absent from the respective getters.
    let without_fragment = destination.href.split('#').next().unwrap();
    let search = without_fragment
        .find('?')
        .map(|i| &without_fragment[i..])
        .filter(|q| q.len() > 1)
        .unwrap_or("");
    let mut result = components.pathname;
    result.push_str(search);
    result.push_str(&destination.hash);
    Ok(result.into())
}
fn require(condition: bool, code: &str, message: &str) -> IvyResult<()> {
    if condition {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
