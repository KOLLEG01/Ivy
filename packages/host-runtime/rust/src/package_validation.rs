//! Package identity/catalog admission without archive or process dependencies.
use crate::{IvyError, IvyJson, IvyResult};
use ivy_contracts::{core_schema::validate_host, json};

pub const MAXIMUM_ARCHIVE_BYTES: u64 = 256 * 1024 * 1024;
fn require(value: bool, code: &str, message: &str) -> IvyResult<()> {
    if value { Ok(()) } else { Err(IvyError::new(code, message)) }
}
fn string(value: Option<&IvyJson>) -> Option<String> { value.and_then(|value| value.as_str().ok()) }
fn safe_integer(value: Option<&IvyJson>, minimum: f64, maximum: f64) -> bool {
    value.and_then(IvyJson::as_f64).is_some_and(|n| n.is_finite() && n.fract() == 0.0 && n >= minimum && n <= maximum)
}
fn hash(value: Option<&IvyJson>) -> bool {
    string(value).is_some_and(|s| s.len() == 71 && s.starts_with("sha256:")
        && s[7..].bytes().all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)))
}
fn id(value: Option<&IvyJson>) -> bool {
    string(value).is_some_and(|s| (1..=128).contains(&s.len()) && s.as_bytes()[0].is_ascii_lowercase()
        && s.bytes().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-'))
}

pub fn validate_package_authorization(value: &IvyJson) -> IvyResult<IvyJson> {
    require(matches!(value, IvyJson::Object(_)), "invalid_arguments", "Package upload authorization needs an object.")?;
    // The original host validator canonicalizes the property before its schema;
    // an absent property is undefined, which is outside the JSON domain.
    let manifest = value.get("manifest").ok_or_else(|| IvyError::new("invalid_arguments", "Value is outside the JSON domain."))?;
    validate_host("ReleaseManifest", manifest)?;
    let version = value.get("version").and_then(IvyJson::as_js_string).ok_or_else(|| IvyError::new("invalid_arguments", "Package version must be a string."))?.to_utf8().unwrap_or_default();
    json::compare_versions(&version, "0.0.0")?;
    require(id(value.get("componentId")) && value.get("componentId") == manifest.get("componentId")
        && value.get("version") == manifest.get("version") && hash(value.get("buildId"))
        && value.get("buildId") == manifest.get("buildId") && hash(value.get("archiveHash"))
        && safe_integer(value.get("bytes"), 1.0, MAXIMUM_ARCHIVE_BYTES as f64),
        "invalid_arguments", "Package upload identity does not match its manifest.")?;
    Ok(IvyJson::object(["componentId", "version", "buildId", "archiveHash", "bytes", "manifest"]
        .into_iter().map(|key| (key, value.get(key).unwrap().clone()))))
}

/// Inspect stable identities before parsing another component's newer manifest.
pub fn validate_package_catalog_index(value: &IvyJson) -> IvyResult<IvyJson> {
    require(matches!(value, IvyJson::Object(_)), "invalid_arguments", "Package catalog needs an object.")?;
    let packages = value.get("packages").and_then(IvyJson::as_array);
    require(value.get("schemaVersion") == Some(&IvyJson::Number(1.0))
        && safe_integer(value.get("revision"), 0.0, 9_007_199_254_740_991.0)
        && packages.is_some_and(|packages| packages.len() <= 4096), "invalid_arguments", "Package catalog is invalid.")?;
    for entry in packages.unwrap() {
        require(matches!(entry, IvyJson::Object(_)) && id(entry.get("componentId"))
            && entry.get("version").is_some_and(|value| value.as_js_string().is_some()),
            "invalid_arguments", "Package catalog entry identity is invalid.")?;
        json::compare_versions(&entry.required("version")?.as_js_string().unwrap().to_utf8().unwrap_or_default(), "0.0.0")?;
    }
    Ok(value.clone())
}

pub fn validate_package_catalog(value: &IvyJson) -> IvyResult<IvyJson> {
    validate_package_catalog_index(value)?;
    for entry in value.required("packages")?.as_array().unwrap() {
        let authorized = validate_package_authorization(entry)?;
        let published = entry.get("publishedAt").and_then(IvyJson::as_js_string);
        let publisher = entry.get("publisherPrincipalId").and_then(IvyJson::as_js_string);
        require(safe_integer(entry.get("revision"), 1.0, 9_007_199_254_740_991.0)
            && published.is_some_and(|value| ivy_sdk::clock::parse_date(value).is_finite())
            && publisher.is_some_and(|value| (1..=256).contains(&value.0.len()))
            && json::canonical(authorized.required("manifest")?, usize::MAX)? == json::canonical(entry.required("manifest")?, usize::MAX)?,
            "invalid_arguments", "Package catalog entry is invalid.")?;
    }
    Ok(value.clone())
}
