use ivy_contracts::{
    IvyError, IvyJson, IvyResult,
    hash::digest,
    json::{parse, try_js_string},
};
use std::{
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Debug, Clone, PartialEq)]
pub struct BuildIdentity {
    pub schema_version: u32,
    /// Package metadata is retained verbatim; None represents a missing version.
    pub version: Option<IvyJson>,
    pub build_id: String,
}
pub fn build_identity(root: &Path) -> IvyResult<BuildIdentity> {
    let bytes = std::fs::read(root.join("package.json")).map_err(|_| IvyError::internal())?;
    let package =
        parse(&String::from_utf8_lossy(&bytes), usize::MAX).map_err(|_| IvyError::internal())?;
    // JSON.parse accepts primitive roots; only null forbids property access.
    if package.is_null() {
        return Err(IvyError::internal());
    }
    let version = package.get("version").cloned();
    let supplied = std::env::var("IVY_BUILD_ID").ok();
    let build_id = match supplied {
        Some(id)
            if id.len() == 71
                && id.starts_with("sha256:")
                && id[7..]
                    .bytes()
                    .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)) =>
        {
            id
        }
        _ => {
            let version = version_text(version.as_ref())?;
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|_| IvyError::internal())?
                .as_millis();
            digest(format!("{version}\0{now}\0{}", uuid::Uuid::new_v4()))
        }
    };
    Ok(BuildIdentity {
        schema_version: 1,
        version,
        build_id,
    })
}
impl BuildIdentity {
    /// Serialize through the lossless JSON codec, including lone UTF-16 surrogates.
    pub fn json(&self) -> IvyJson {
        let mut fields = vec![("schemaVersion", IvyJson::Number(self.schema_version as f64))];
        if let Some(version) = &self.version {
            fields.push(("version", version.clone()));
        }
        fields.push(("buildId", self.build_id.clone().into()));
        IvyJson::object(fields)
    }
}

fn version_text(version: Option<&IvyJson>) -> IvyResult<String> {
    let Some(version) = version else {
        return Ok("undefined".into());
    };
    Ok(String::from_utf16_lossy(&try_js_string(version)?.0))
}
