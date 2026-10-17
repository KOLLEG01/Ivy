use ivy_contracts::{IvyError, IvyJson, IvyResult, hash::digest, json::parse};
use std::{
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BuildIdentity {
    pub schema_version: u32,
    pub version: String,
    pub build_id: String,
}
pub fn build_identity(root: &Path) -> IvyResult<BuildIdentity> {
    let package = std::fs::read_to_string(root.join("package.json"))
        .map_err(|e| IvyError::new("invalid_arguments", e.to_string()))?;
    let package = parse(&package, 1_048_576)?;
    let version = package
        .get("version")
        .ok_or_else(|| IvyError::new("invalid_arguments", "Package version is missing."))?
        .as_str()?;
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
    pub fn json(&self) -> IvyJson {
        IvyJson::object([
            ("schemaVersion", IvyJson::Number(1.0)),
            ("version", self.version.clone().into()),
            ("buildId", self.build_id.clone().into()),
        ])
    }
}
