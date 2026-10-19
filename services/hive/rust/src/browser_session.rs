//! Browser session policy after HTTP path/content-type/body admission. The
//! transport owns page rendering, response I/O and the live WebSocket peer set.
use crate::{
    browser_push::Worker,
    http_auth::{self, Connection, Headers},
    oauth::Parameters,
};
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString,
    hash::digest,
    json::try_js_string,
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
    /// Preserve the owning Set's iteration semantics, including synchronous
    /// removals/additions. Do not retain a registry lock across terminate().
    fn iter(&self) -> Box<dyn Iterator<Item = Arc<dyn Peer>> + '_>;
}
pub struct Session<'a> {
    pub connection: &'a Connection,
    pub worker: &'a dyn Worker,
}
impl Session<'_> {
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
        if session.is_null() {
            return Err(IvyError::internal());
        }
        let token = session
            .get("sessionToken")
            .map(try_js_string)
            .transpose()?
            .unwrap_or_else(|| "undefined".into());
        response.set_cookie(&owner.cookie(&token, false))?;
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
        if truthy(context.get("sessionDigest")) {
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
        for peer in peers.iter() {
            let peer_context = peer.context();
            if peer_context.is_null() {
                return Err(IvyError::internal());
            }
            if truthy(peer_context.get("sessionDigest"))
                && equal(
                    peer_context.get("credentialDigest"),
                    context.get("credentialDigest"),
                )
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
fn truthy(value: Option<&IvyJson>) -> bool {
    match value {
        None | Some(IvyJson::Null) => false,
        Some(IvyJson::Bool(v)) => *v,
        Some(IvyJson::Number(v)) => *v != 0.0 && !v.is_nan(),
        Some(IvyJson::String(v)) => !v.0.is_empty(),
        _ => true,
    }
}
fn equal(a: Option<&IvyJson>, b: Option<&IvyJson>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(IvyJson::Object(_) | IvyJson::Array(_)), Some(other)) => {
            std::ptr::eq(a.unwrap(), other)
        }
        (Some(a), Some(b)) => a == b,
        _ => false,
    }
}
