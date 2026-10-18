//! Authoritative skills reads and preflight before filesystem application.
use crate::{
    environment::{EnvironmentKind, read_environment_document, select_environment_document},
    skills::{SkillFiles, normalize_skills},
};
use ivy_contracts::{IvyJson, IvyResult, JsString, hash::hash_json};
use ivy_sdk::rpc::RpcClient;

pub struct PreparedSkills {
    pub groups: Vec<SkillFiles>,
    pub desired_version: String,
    pub authority: String,
}

/// Preserve global-then-host reads, including disabled documents and inactive
/// revisions in the desired version. Path/state checks follow this preparation.
pub async fn prepare_skills(
    connection: &dyn RpcClient,
    defaults: &IvyJson,
    host_id: &JsString,
    hive_url: &JsString,
) -> IvyResult<PreparedSkills> {
    let global = read_environment_document(connection, EnvironmentKind::Skills, None).await?;
    let host =
        read_environment_document(connection, EnvironmentKind::Skills, Some(host_id)).await?;
    let desired = select_environment_document(defaults, global.as_ref(), host.as_ref())?;
    Ok(PreparedSkills {
        groups: normalize_skills(&desired.document)?,
        desired_version: desired.version,
        authority: hash_json(&IvyJson::object([
            ("hive", IvyJson::String(hive_url.clone())),
            ("hostId", IvyJson::String(host_id.clone())),
        ]))?,
    })
}
