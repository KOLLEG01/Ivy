use crate::{IvyError, IvyResult, JsString};
use url::{Url, quirks};

/// URL constructor fields used at JavaScript interoperability boundaries.
/// Authorization and scheme/host policy belong to the caller.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UrlProjection {
    pub href: String,
    pub protocol: String,
    pub hostname: String,
    pub username: String,
    pub password: String,
    pub hash: String,
}

/// Parse an absolute URL, with the constructor's USVString conversion and
/// IvyError::from(TypeError) failure mapping. This does not apply Hive-base policy.
pub fn parse_url(input: &JsString) -> IvyResult<UrlProjection> {
    let text = String::from_utf16_lossy(&input.0);
    let url = Url::parse(&text).map_err(|_| IvyError::internal())?;
    if url.scheme() == "file" {
        return file_projection(&text);
    }
    Ok(UrlProjection {
        href: href(&url),
        protocol: quirks::protocol(&url).into(),
        hostname: quirks::hostname(&url).into(),
        username: quirks::username(&url).into(),
        password: quirks::password(&url).into(),
        hash: quirks::hash(&url).into(),
    })
}

/// Construct an absolute WHATWG URL, clear its username and password, and
/// serialize it. Constructor failures are returned to the caller, which decides
/// whether to retain the original input. Opaque/file setters are no-ops.
pub fn clear_url_credentials(input: &JsString) -> IvyResult<JsString> {
    let text = String::from_utf16_lossy(&input.0);
    let mut url = Url::parse(&text).map_err(|_| IvyError::internal())?;
    if url.scheme() == "file" {
        return Ok(JsString::from(file_projection(&text)?.href));
    }
    let _ = quirks::set_username(&mut url, "");
    let _ = quirks::set_password(&mut url, "");
    Ok(JsString::from(href(&url)))
}
fn href(url: &Url) -> String {
    let mut href = quirks::href(url).to_string();
    if url.cannot_be_a_base()
        && url.path().ends_with(' ')
        && (url.query().is_some() || url.fragment().is_some())
    {
        let last = url.scheme().len() + url.path().len();
        href.replace_range(last..last + 1, "%20");
    }
    href
}

// The URL crate retains older file-path rules (dropping a host at a drive
// segment and stripping repeated leading slashes). Preserve Node's current
// file path state while reusing the parser for host/component encoding.
fn file_projection(input: &str) -> IvyResult<UrlProjection> {
    let cleaned: String = input
        .trim_matches(|c: char| c <= '\u{20}')
        .chars()
        .filter(|c| !matches!(c, '\t' | '\r' | '\n'))
        .collect();
    let rest = &cleaned[cleaned.find(':').ok_or_else(IvyError::internal)? + 1..];
    let end = rest.find(['?', '#']).unwrap_or(rest.len());
    let main = rest[..end].replace('\\', "/");
    let suffix = &rest[end..];
    let mut authority = "";
    let mut path = main.as_str();
    if let Some(rest) = main.strip_prefix("//") {
        let slash = rest.find('/').unwrap_or(rest.len());
        let candidate = &rest[..slash];
        if drive(candidate) {
            path = rest;
        } else {
            authority = candidate;
            path = &rest[slash..];
        }
    }
    let base =
        Url::parse(&format!("file://{authority}/{suffix}")).map_err(|_| IvyError::internal())?;
    let hostname = quirks::hostname(&base).to_string();
    let segments: Vec<_> = path.strip_prefix('/').unwrap_or(path).split('/').collect();
    let mut normalized: Vec<String> = Vec::new();
    for (index, segment) in segments.iter().enumerate() {
        let dotted = segment.to_ascii_lowercase().replace("%2e", ".");
        if dotted == "." || dotted == ".." {
            if dotted == ".." && !(normalized.len() == 1 && drive(&normalized[0])) {
                normalized.pop();
            }
            if index + 1 == segments.len() {
                normalized.push(String::new());
            }
            continue;
        }
        let segment = if normalized.is_empty() && drive(segment) {
            format!("{}:", segment.as_bytes()[0] as char)
        } else {
            segment.to_string()
        };
        let encoded = Url::parse(&format!("https://placeholder.invalid/_/{segment}/"))
            .map_err(|_| IvyError::internal())?;
        normalized.push(
            encoded
                .path()
                .strip_prefix("/_/")
                .and_then(|path| path.strip_suffix('/'))
                .ok_or_else(IvyError::internal)?
                .to_string(),
        );
    }
    let mut href = format!("file://{hostname}/{}", normalized.join("/"));
    if let Some(query) = base.query() {
        href.push('?');
        href.push_str(query);
    }
    if let Some(fragment) = base.fragment() {
        href.push('#');
        href.push_str(fragment);
    }
    Ok(UrlProjection {
        href,
        protocol: "file:".into(),
        hostname,
        username: String::new(),
        password: String::new(),
        hash: quirks::hash(&base).into(),
    })
}
fn drive(segment: &str) -> bool {
    let bytes = segment.as_bytes();
    bytes.len() == 2 && bytes[0].is_ascii_alphabetic() && matches!(bytes[1], b':' | b'|')
}
