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
    let url = Url::parse(&String::from_utf16_lossy(&input.0)).map_err(|_| IvyError::internal())?;
    Ok(UrlProjection {
        href: quirks::href(&url).into(),
        protocol: quirks::protocol(&url).into(),
        hostname: quirks::hostname(&url).into(),
        username: quirks::username(&url).into(),
        password: quirks::password(&url).into(),
        hash: quirks::hash(&url).into(),
    })
}
