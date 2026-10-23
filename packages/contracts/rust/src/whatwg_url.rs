//! URL and form fields used by Hive's HTTP and OAuth interfaces.
use crate::{IvyError, IvyResult, JsString};
use url::Url;

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
fn parsed(input: &JsString) -> IvyResult<Url> {
    Url::parse(&input.to_utf8()?).map_err(|_| IvyError::internal())
}
fn projection(url: Url) -> UrlProjection {
    UrlProjection {
        href: url.to_string(),
        protocol: format!("{}:", url.scheme()),
        hostname: url.host_str().unwrap_or_default().into(),
        username: url.username().into(),
        password: url.password().unwrap_or_default().into(),
        hash: url
            .fragment()
            .filter(|value| !value.is_empty())
            .map(|value| format!("#{value}"))
            .unwrap_or_default(),
    }
}
pub fn parse_url(input: &JsString) -> IvyResult<UrlProjection> {
    Ok(projection(parsed(input)?))
}
/// Remove display credentials with the same native parser used by other URL APIs.
pub fn clear_url_credentials(input: &JsString) -> IvyResult<JsString> {
    let mut url =
        Url::parse(&String::from_utf16_lossy(&input.0)).map_err(|_| IvyError::internal())?;
    let _ = url.set_username("");
    let _ = url.set_password(None);
    Ok(url.to_string().into())
}
pub fn parse_url_with_base(input: &JsString, base: &JsString) -> IvyResult<UrlProjection> {
    Ok(projection(
        parsed(base)?
            .join(&input.to_utf8()?)
            .map_err(|_| IvyError::internal())?,
    ))
}
pub fn url_components(input: &JsString) -> IvyResult<UrlComponents> {
    let url = parsed(input)?;
    Ok(UrlComponents {
        origin: url.origin().ascii_serialization(),
        pathname: url.path().into(),
    })
}
pub fn parse_form(input: &JsString) -> Vec<(JsString, JsString)> {
    let text = String::from_utf16_lossy(&input.0);
    url::form_urlencoded::parse(text.strip_prefix('?').unwrap_or(&text).as_bytes())
        .map(|(name, value)| (name.as_ref().into(), value.as_ref().into()))
        .collect()
}
pub fn set_url_search_params(
    input: &JsString,
    updates: &[(JsString, JsString)],
) -> IvyResult<JsString> {
    let mut url = parsed(input)?;
    if !updates.is_empty() {
        let mut pairs: Vec<(String, String)> = url.query_pairs().into_owned().collect();
        for (name, value) in updates {
            let (name, value) = (name.to_utf8()?, value.to_utf8()?);
            if let Some(index) = pairs.iter().position(|(key, _)| *key == name) {
                pairs[index].1 = value;
                let mut position = 0;
                pairs.retain(|(key, _)| {
                    let keep = *key != name || position == index;
                    position += 1;
                    keep
                });
            } else {
                pairs.push((name, value));
            }
        }
        url.query_pairs_mut().clear().extend_pairs(pairs);
    }
    Ok(url.to_string().into())
}
