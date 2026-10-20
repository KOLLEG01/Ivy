//! Existing authentication documents and their native HTTP response bindings.
#[path = "auth_page_fragments.rs"]
mod fragments;
use crate::{
    browser_session,
    http_auth::Connection,
    http_response::{self, Response},
    oauth::Parameters,
};
use ivy_contracts::{IvyJson, IvyResult, JsString};

fn escape(value: &JsString, apostrophe: bool) -> JsString {
    let mut result = JsString::default();
    for unit in &value.0 {
        let escaped = match *unit {
            38 => Some("&amp;"),
            34 => Some("&quot;"),
            60 => Some("&lt;"),
            62 => Some("&gt;"),
            39 if apostrophe => Some("&#39;"),
            _ => None,
        };
        if let Some(text) = escaped {
            result.0.extend(text.encode_utf16());
        } else {
            result.0.push(*unit);
        }
    }
    result
}
fn append(value: &JsString, suffix: &str) -> JsString {
    let mut result = value.clone();
    result.0.extend(suffix.encode_utf16());
    result
}
fn interpolate(parts: &[&str; 7], values: [JsString; 6]) -> JsString {
    let mut result = JsString::default();
    for (part, value) in parts.iter().zip(values) {
        result.0.extend(part.encode_utf16());
        result.0.extend(value.0);
    }
    result.0.extend(parts[6].encode_utf16());
    result
}
pub fn login(base_path: &JsString, return_to: &JsString, rejected: bool) -> JsString {
    interpolate(
        &fragments::LOGIN,
        [
            escape(&append(base_path, "/mcp-icon.svg"), false),
            escape(&append(base_path, "/login/theme.js"), false),
            if rejected {
                fragments::LOGIN_ERROR.into()
            } else {
                JsString::default()
            },
            escape(&append(base_path, "/login"), false),
            if rejected {
                fragments::LOGIN_INVALID.into()
            } else {
                JsString::default()
            },
            escape(return_to, false),
        ],
    )
}
pub fn oauth(
    base_path: &JsString,
    transaction: &JsString,
    client_name: &JsString,
    scope: &JsString,
    rejected: bool,
) -> JsString {
    interpolate(
        &fragments::OAUTH,
        [
            escape(&append(base_path, "/mcp-icon.svg"), true),
            escape(client_name, true),
            escape(scope, true),
            if rejected {
                fragments::OAUTH_ERROR.into()
            } else {
                JsString::default()
            },
            escape(base_path, true),
            escape(transaction, true),
        ],
    )
}
pub fn login_theme_script() -> &'static str {
    fragments::LOGIN_THEME_SCRIPT
}

/// Like ServerResponse.end(string), HTTP body encoding replaces lone UTF-16
/// surrogates. Renderer results retain the original code units until this point.
pub fn html(response: &mut impl Response, status: u16, document: &JsString) -> IvyResult<()> {
    response.write_head(
        status,
        IvyJson::object([("Content-Type", "text/html; charset=utf-8".into())]),
    )?;
    response.end(String::from_utf16_lossy(&document.0))
}
/// Caller selected GET /login and supplied canonical, decoded search entries.
pub fn login_response(
    connection: &Connection,
    query: &Parameters,
    response: &mut impl Response,
) -> IvyResult<()> {
    let owner = connection.authentication();
    let target = browser_session::return_path(
        owner.canonical_origin(),
        owner.base_path(),
        query.get("returnTo"),
    )?;
    html(response, 200, &login(owner.base_path(), &target, false))
}
/// Caller selected GET /login/theme.js after the outer maintenance guard.
pub fn theme_response(response: &mut impl Response) -> IvyResult<()> {
    response.write_head(
        200,
        IvyJson::object([("Content-Type", "text/javascript; charset=utf-8".into())]),
    )?;
    response.end(login_theme_script().to_owned())
}
pub struct SessionResponse<'a, R> {
    pub connection: &'a Connection,
    pub response: &'a mut R,
}
impl<R: Response> browser_session::Response for SessionResponse<'_, R> {
    fn set_cookie(&mut self, value: &JsString) -> IvyResult<()> {
        self.response.set_header("Set-Cookie", value)
    }
    fn redirect(&mut self, target: &JsString) -> IvyResult<()> {
        http_response::redirect(self.response, target)
    }
    fn login_rejected(&mut self, target: &JsString) -> IvyResult<()> {
        html(
            self.response,
            401,
            &login(self.connection.authentication().base_path(), target, true),
        )
    }
}
