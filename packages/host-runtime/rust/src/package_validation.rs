//! Package identity/catalog admission without archive or process dependencies.
use crate::{IvyError, IvyJson, IvyResult};
use ivy_contracts::{JsString, core_schema::validate_host, json};

pub const MAXIMUM_ARCHIVE_BYTES: u64 = 256 * 1024 * 1024;
pub const CATALOG_BYTES: usize = 4 * 1024 * 1024;

/// Stable identities can be read before a newer component manifest is admitted.
/// Manifest payloads stay borrowed from the one received catalog.
#[derive(Clone)]
pub struct PackageIndex<'a> {
    pub component_id: String,
    pub version: String,
    pub value: &'a IvyJson,
}
pub struct CatalogIndex<'a> { pub revision: f64, pub packages: Vec<PackageIndex<'a>> }
pub struct PackageEntry<'a> {
    pub component_id: String,
    pub version: String,
    pub build_id: &'a JsString,
    pub archive_hash: &'a JsString,
    pub bytes: u64,
    pub revision: u64,
    pub manifest: &'a IvyJson,
    pub value: &'a IvyJson,
}
pub struct PackageCatalog<'a> { pub revision: f64, pub packages: Vec<PackageEntry<'a>> }

impl<'a> PackageIndex<'a> {
    pub fn read(value: &'a IvyJson) -> IvyResult<Self> {
        require(matches!(value, IvyJson::Object(_)) && id(value.get("componentId"))
            && value.get("version").is_some_and(|value| value.as_js_string().is_some()),
            "invalid_arguments", "Package catalog entry identity is invalid.")?;
        let version = value.required("version")?.as_js_string().unwrap().to_utf8().unwrap_or_default();
        json::compare_versions(&version, "0.0.0")?;
        Ok(Self { component_id: value.required("componentId")?.as_str()?, version, value })
    }
    pub fn validate(self) -> IvyResult<PackageEntry<'a>> {
        let manifest = check_package_authorization(self.value)?;
        let published = self.value.get("publishedAt").and_then(IvyJson::as_js_string);
        let publisher = self.value.get("publisherPrincipalId").and_then(IvyJson::as_js_string);
        require(safe_integer(self.value.get("revision"), 1.0, 9_007_199_254_740_991.0)
            && published.is_some_and(|value| ivy_sdk::clock::parse_date(value).is_finite())
            && publisher.is_some_and(|value| (1..=256).contains(&value.0.len())),
            "invalid_arguments", "Package catalog entry is invalid.")?;
        Ok(PackageEntry {
            component_id: self.component_id, version: self.version, manifest, value: self.value,
            build_id: self.value.required("buildId")?.as_js_string().unwrap(),
            archive_hash: self.value.required("archiveHash")?.as_js_string().unwrap(),
            bytes: self.value.required("bytes")?.as_f64().unwrap() as u64,
            revision: self.value.required("revision")?.as_f64().unwrap() as u64,
        })
    }
}
impl<'a> CatalogIndex<'a> {
    pub fn read(value: &'a IvyJson) -> IvyResult<Self> {
        require(matches!(value, IvyJson::Object(_)), "invalid_arguments", "Package catalog needs an object.")?;
        let packages = value.get("packages").and_then(IvyJson::as_array);
        require(value.get("schemaVersion") == Some(&IvyJson::Number(1.0))
            && safe_integer(value.get("revision"), 0.0, 9_007_199_254_740_991.0)
            && packages.is_some_and(|packages| packages.len() <= 4096), "invalid_arguments", "Package catalog is invalid.")?;
        Ok(Self { revision: value.required("revision")?.as_f64().unwrap(), packages: packages.unwrap().iter().map(PackageIndex::read).collect::<IvyResult<_>>()? })
    }
    pub fn validate(self) -> IvyResult<PackageCatalog<'a>> {
        Ok(PackageCatalog { revision: self.revision, packages: self.packages.into_iter().map(PackageIndex::validate).collect::<IvyResult<_>>()? })
    }
}
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

fn check_package_authorization(value: &IvyJson) -> IvyResult<&IvyJson> {
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
    Ok(manifest)
}
pub fn validate_package_authorization(value: &IvyJson) -> IvyResult<IvyJson> {
    check_package_authorization(value)?;
    Ok(IvyJson::object(["componentId", "version", "buildId", "archiveHash", "bytes", "manifest"]
        .into_iter().map(|key| (key, value.get(key).unwrap().clone()))))
}

/// Inspect stable identities before parsing another component's newer manifest.
pub fn validate_package_catalog_index(value: &IvyJson) -> IvyResult<IvyJson> {
    CatalogIndex::read(value)?;
    Ok(value.clone())
}

pub fn validate_package_catalog(value: &IvyJson) -> IvyResult<IvyJson> {
    CatalogIndex::read(value)?.validate()?;
    Ok(value.clone())
}
