use ivy_contracts::{IvyError, IvyJson, IvyResult, hash::digest, json::parse};
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
    if package.as_object().is_none() {
        return Err(IvyError::internal());
    }
    let version = package.get("version").cloned();
    let version_text = version
        .as_ref()
        .map(|version| {
            version
                .as_js_string()
                .map(|value| String::from_utf16_lossy(&value.0))
                .ok_or_else(IvyError::internal)
        })
        .transpose()?
        .unwrap_or_default();
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
            let version = version_text;
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
