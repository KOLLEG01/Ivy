//! Ordered HiveServer constructor policy over typed configuration inputs.
//! Native URL construction failure stays distinct from an Ivy policy error;
//! its exception projection belongs to the eventual bootstrap boundary.
use crate::{http_auth::validate_trusted_proxies, store::Credential};
use ivy_contracts::{
    IvyError, JsString,
    whatwg_url::{parse_url, url_components},
};
use std::collections::HashSet;

#[derive(Debug, Clone, PartialEq)]
pub enum Error {
    Ivy(IvyError),
    UrlConstructor { input: JsString },
}
impl From<IvyError> for Error {
    fn from(value: IvyError) -> Self {
        Self::Ivy(value)
    }
}
#[derive(Debug, Clone, PartialEq)]
pub struct BaseUrl {
    pub href: JsString,
    pub origin: JsString,
    pub base_path: JsString,
}
pub fn validate(
    public_base_url: &JsString,
    trusted_proxy_addresses: &[JsString],
    package_publisher_principal_ids: &[JsString],
    credentials: &[Credential],
) -> Result<BaseUrl, Error> {
    validate_trusted_proxies(trusted_proxy_addresses)?;
    if package_publisher_principal_ids
        .iter()
        .collect::<HashSet<_>>()
        .len()
        != package_publisher_principal_ids.len()
        || !package_publisher_principal_ids.iter().all(|id| {
            credentials
                .iter()
                .any(|credential| credential.principal_id == *id)
        })
    {
        return Err(IvyError::new(
            "invalid_arguments",
            "Package publishers must be distinct configured Hive principals.",
        )
        .into());
    }
    let invalid_url = || Error::UrlConstructor {
        input: public_base_url.clone(),
    };
    let url = parse_url(public_base_url).map_err(|_| invalid_url())?;
    // URL.search is empty for a bare '?', even though href retains the marker.
    let has_search = url
        .href
        .split('#')
        .next()
        .unwrap()
        .split_once('?')
        .is_some_and(|(_, search)| !search.is_empty());
    if !matches!(url.protocol.as_str(), "http:" | "https:")
        || !url.username.is_empty()
        || !url.password.is_empty()
        || has_search
        || !url.hash.is_empty()
    {
        return Err(IvyError::new("invalid_arguments", "Invalid canonical publicBaseUrl.").into());
    }
    if url.protocol != "https:"
        && !matches!(url.hostname.as_str(), "localhost" | "127.0.0.1" | "[::1]")
    {
        return Err(IvyError::new(
            "invalid_arguments",
            "Public browser authentication requires HTTPS.",
        )
        .into());
    }
    let parts = url_components(&JsString::from(url.href.clone())).map_err(|_| invalid_url())?;
    Ok(BaseUrl {
        href: url.href.into(),
        origin: parts.origin.into(),
        base_path: parts
            .pathname
            .strip_suffix('/')
            .unwrap_or(&parts.pathname)
            .into(),
    })
}
