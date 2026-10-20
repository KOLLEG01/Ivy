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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UrlComponents {
    pub origin: String,
    pub pathname: String,
}

/// Parse an absolute URL, with the constructor's USVString conversion and
/// IvyError::from(TypeError) failure mapping. This does not apply Hive-base policy.
pub fn parse_url(input: &JsString) -> IvyResult<UrlProjection> {
    let text = String::from_utf16_lossy(&input.0);
    absolute_projection(&text)
}

/// Read the constructor's serialized origin and pathname, including opaque
/// origins ("null"). This does not make the URL suitable for a particular caller.
pub fn url_components(input: &JsString) -> IvyResult<UrlComponents> {
    let text = String::from_utf16_lossy(&input.0);
    let url = Url::parse(&text).map_err(|_| IvyError::internal())?;
    if url.scheme() == "file" {
        let projection = file_projection(&text)?;
        return Ok(UrlComponents {
            origin: "null".into(),
            pathname: file_pathname(&projection).into(),
        });
    }
    let origin = if url.scheme() == "blob" {
        match Url::parse(url.path()) {
            Ok(inner) if matches!(inner.scheme(), "http" | "https") => quirks::origin(&inner),
            _ => "null".into(),
        }
    } else {
        quirks::origin(&url)
    };
    let mut pathname = url.path().to_string();
    if url.cannot_be_a_base()
        && pathname.ends_with(' ')
        && (url.query().is_some() || url.fragment().is_some())
    {
        pathname.pop();
        pathname.push_str("%20");
    }
    Ok(UrlComponents { origin, pathname })
}

/// Construct a URL with a base. The base is parsed even for an absolute input,
/// as in JavaScript's two-argument constructor; both arguments are USVStrings.
pub fn parse_url_with_base(input: &JsString, base: &JsString) -> IvyResult<UrlProjection> {
    let base = absolute_projection(&String::from_utf16_lossy(&base.0))?;
    let text = clean_input(&String::from_utf16_lossy(&input.0));
    let scheme = text.find(':').filter(|&end| {
        text.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)
            && text.as_bytes()[..end]
                .iter()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'+' | b'-' | b'.'))
    });
    if let Some(end) = scheme {
        if text[..end].eq_ignore_ascii_case("file") {
            return if base.protocol == "file:" {
                resolve_file(&text[end + 1..], &base)
            } else {
                file_projection(&text)
            };
        }
    } else if base.protocol == "file:" {
        return resolve_file(&text, &base);
    }
    let special = matches!(
        base.protocol.as_str(),
        "http:" | "https:" | "ftp:" | "ws:" | "wss:"
    );
    if scheme.is_none()
        && special
        && text.chars().take_while(|c| matches!(c, '/' | '\\')).count() >= 2
    {
        return absolute_projection(&format!("{}{text}", base.protocol));
    }
    let base = Url::parse(&base.href).map_err(|_| IvyError::internal())?;
    if scheme.is_none() && base.cannot_be_a_base() && text.contains('#') && !text.starts_with('#') {
        return resolve_opaque_fragment(&text, &base);
    }
    // Backslashes are path characters for non-special URLs. The crate's
    // relative-slash state still treats a leading one as an authority slash.
    let url = if scheme.is_none() && !special && text.starts_with('\\') {
        base.join(&format!("./{text}"))
    } else if scheme.is_none() && !special && text.starts_with("/\\") {
        base.join("/")
            .and_then(|root| root.join(&format!(".{text}")))
    } else {
        base.join(&text)
    }
    .map_err(|_| IvyError::internal())?;
    Ok(projection(&url))
}

fn absolute_projection(text: &str) -> IvyResult<UrlProjection> {
    let url = Url::parse(&text).map_err(|_| IvyError::internal())?;
    if url.scheme() == "file" {
        return file_projection(&text);
    }
    Ok(projection(&url))
}

fn projection(url: &Url) -> UrlProjection {
    UrlProjection {
        href: href(&url),
        protocol: quirks::protocol(&url).into(),
        hostname: quirks::hostname(&url).into(),
        username: quirks::username(&url).into(),
        password: quirks::password(&url).into(),
        hash: quirks::hash(&url).into(),
    }
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

/// Construct URLSearchParams from a string, preserving duplicate names and order.
/// The input undergoes USVString conversion before form decoding.
pub fn parse_form(input: &JsString) -> Vec<(JsString, JsString)> {
    let text = String::from_utf16_lossy(&input.0);
    url::form_urlencoded::parse(text.strip_prefix('?').unwrap_or(&text).as_bytes())
        .map(|(name, value)| {
            (
                JsString::from(name.as_ref()),
                JsString::from(value.as_ref()),
            )
        })
        .collect()
}

/// URLSearchParams(entries).toString() for already-string name/value pairs.
/// Retain their order and duplicates, converting each UTF-16 string to USVString.
pub fn serialize_form(entries: &[(JsString, JsString)]) -> JsString {
    url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(entries.iter().map(|(name, value)| {
            (
                String::from_utf16_lossy(&name.0),
                String::from_utf16_lossy(&value.0),
            )
        }))
        .finish()
        .into()
}

/// Construct an absolute URL and apply searchParams.set for each pair in order.
/// Empty updates leave the constructor's query spelling intact. Caller policy
/// (including permitted redirect schemes and origins) is deliberately separate.
pub fn set_url_search_params(
    input: &JsString,
    updates: &[(JsString, JsString)],
) -> IvyResult<JsString> {
    let projected = absolute_projection(&String::from_utf16_lossy(&input.0))?;
    if updates.is_empty() {
        return Ok(projected.href.into());
    }
    // Work on the already normalized href so updating the query cannot reapply
    // the crate's older file-path rules or alter an opaque path or fragment.
    let href = &projected.href;
    let fragment = href.find('#').unwrap_or(href.len());
    let query = href[..fragment].find('?').unwrap_or(fragment);
    let mut pairs: Vec<(String, String)> = if query < fragment {
        // A literal '?' at the start of the URL's query is part of its first name;
        // only the string constructor above strips an optional leading '?'.
        url::form_urlencoded::parse(href[query + 1..fragment].as_bytes())
            .into_owned()
            .collect()
    } else {
        Vec::new()
    };
    for (name, value) in updates {
        let name = String::from_utf16_lossy(&name.0);
        let value = String::from_utf16_lossy(&value.0);
        if let Some(first) = pairs.iter().position(|(key, _)| key == &name) {
            pairs[first].1 = value;
            let mut index = 0;
            pairs.retain(|(key, _)| {
                let keep = index <= first || key != &name;
                index += 1;
                keep
            });
        } else {
            pairs.push((name, value));
        }
    }
    let query_text = url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(&pairs)
        .finish();
    Ok(format!("{}?{}{}", &href[..query], query_text, &href[fragment..]).into())
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

fn clean_input(input: &str) -> String {
    input
        .trim_matches(|c: char| c <= '\u{20}')
        .chars()
        .filter(|c| !matches!(c, '\t' | '\r' | '\n'))
        .collect()
}

// The pinned JavaScript constructor accepts non-fragment relative references
// against opaque bases when the reference contains '#'. Preserve its retained
// first path segment and untouched opaque base text, rather than relaxing all
// opaque-base failures or re-encoding the base as a hierarchical URL.
fn resolve_opaque_fragment(input: &str, base: &Url) -> IvyResult<UrlProjection> {
    if input.starts_with('/') {
        return absolute_projection(&format!("{}:{input}", base.scheme()));
    }
    if input.starts_with('?') {
        return absolute_projection(&format!("{}:{}{input}", base.scheme(), base.path()));
    }
    let end = input.find(['?', '#']).unwrap_or(input.len());
    let directory = base
        .path()
        .rfind('/')
        .map(|at| &base.path()[..at])
        .unwrap_or(base.path());
    let mut segments: Vec<String> = directory.split('/').map(str::to_owned).collect();
    let parts: Vec<_> = input[..end].split('/').collect();
    for (index, part) in parts.iter().enumerate() {
        match part.to_ascii_lowercase().replace("%2e", ".").as_str() {
            "." | ".." => {
                if part.to_ascii_lowercase().replace("%2e", ".") == ".." && segments.len() > 1 {
                    segments.pop();
                }
                if index + 1 == parts.len() {
                    segments.push(String::new());
                }
            }
            _ => {
                let encoded = Url::parse(&format!("custom://placeholder.invalid/_/{part}/"))
                    .map_err(|_| IvyError::internal())?;
                segments.push(encoded.path()[3..encoded.path().len() - 1].to_string());
            }
        }
    }
    absolute_projection(&format!(
        "{}:{}{}",
        base.scheme(),
        segments.join("/"),
        &input[end..]
    ))
}

fn file_pathname(url: &UrlProjection) -> &str {
    let path = &url.href["file://".len() + url.hostname.len()..];
    &path[..path.find(['?', '#']).unwrap_or(path.len())]
}

// Apply the file/file-slash states before the existing path normalization.
// Unlike the crate's older rules, a drive segment does not discard the host.
fn resolve_file(input: &str, base: &UrlProjection) -> IvyResult<UrlProjection> {
    let end = input.find(['?', '#']).unwrap_or(input.len());
    let main = input[..end].replace('\\', "/");
    let suffix = &input[end..];
    if main.starts_with("//") {
        return file_projection(&format!("file:{main}{suffix}"));
    }
    let base_path = file_pathname(base);
    let path = if let Some(relative) = main.strip_prefix('/') {
        let first = relative.split('/').next().unwrap_or("");
        let base_first = base_path
            .strip_prefix('/')
            .unwrap_or(base_path)
            .split('/')
            .next()
            .unwrap_or("");
        if !drive(first) && drive(base_first) {
            format!("/{base_first}/{relative}")
        } else {
            main
        }
    } else if main.is_empty() {
        let before_fragment = &base.href[..base.href.find('#').unwrap_or(base.href.len())];
        let prefix = if suffix.starts_with('?') {
            &before_fragment[..before_fragment.find('?').unwrap_or(before_fragment.len())]
        } else {
            before_fragment
        };
        return file_projection(&format!("{prefix}{suffix}"));
    } else if drive(main.split('/').next().unwrap_or("")) {
        format!("/{main}")
    } else {
        let directory = if drive(base_path.strip_prefix('/').unwrap_or(base_path)) {
            format!("{base_path}/")
        } else {
            base_path[..base_path.rfind('/').unwrap_or(0) + 1].to_string()
        };
        format!("{directory}{main}")
    };
    file_projection(&format!("file://{}{path}{suffix}", base.hostname))
}

// The URL crate retains older file-path rules (dropping a host at a drive
// segment and stripping repeated leading slashes). Preserve Node's current
// file path state while reusing the parser for host/component encoding.
fn file_projection(input: &str) -> IvyResult<UrlProjection> {
    let cleaned = clean_input(input);
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
