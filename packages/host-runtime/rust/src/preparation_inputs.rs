//! Candidate preflight gates and identities from the fixed captured source.
use crate::{
    IvyError, IvyJson, IvyResult,
    artifact::{FileCompare, artifact_files, artifact_source_identity},
    candidate_identity::candidate_build_id_for_platform,
    config_file::{CONFIGURATION_BYTES, FileError, read_json},
    node_path,
    preparation_cache::dependency_cache_key_json,
    streaming_hash,
};
use ivy_contracts::JsString;
use std::path::PathBuf;

fn internal<T, E>(value: Result<T, E>) -> IvyResult<T> {
    value.map_err(|_| IvyError::internal())
}
fn require(value: bool, message: &str) -> IvyResult<()> {
    if value {
        Ok(())
    } else {
        Err(IvyError::new("invalid_arguments", message))
    }
}
/// Run before snapshot/plan I/O, as in prepareCandidate.
pub fn validate_component_id(component: &JsString) -> IvyResult<()> {
    require(
        component
            .0
            .first()
            .is_some_and(|unit| (97..=122).contains(unit))
            && component
                .0
                .iter()
                .all(|unit| (97..=122).contains(unit) || (48..=57).contains(unit) || *unit == 45),
        "Component ID is not a local source plan name.",
    )
}
/// The caller has loaded/validated the BuildPlan through source_build_plan.
/// Keep the app path and absence-of-general-checks gates before package I/O.
pub fn validate_preparation_plan(component: &JsString, plan: &IvyJson) -> IvyResult<()> {
    if plan.get("kind") == Some(&"app".into()) {
        let app = plan.get("app");
        let mut dist: Vec<u16> = "dist/apps/".encode_utf16().collect();
        dist.extend(&component.0);
        require(
            app.and_then(|app| app.get("appId")) == Some(&IvyJson::String(component.clone()))
                && app.and_then(|app| app.get("dist")) == Some(&IvyJson::String(JsString(dist))),
            "App preparation requires its exact contained bundle path.",
        )?;
    }
    require(
        plan.required("checks")?
            .as_array()
            .ok_or_else(IvyError::internal)?
            .is_empty(),
        "Component plans do not define general verification checks.",
    )
}
fn pinned_npm(value: &IvyJson) -> IvyResult<bool> {
    let Some(text) = value.as_js_string() else { return Ok(false); };
    let Some(version) = text.0.strip_prefix(&[110, 112, 109, 64]) else {
        return Ok(false);
    };
    let parts: Vec<_> = version.split(|unit| *unit == 46).collect();
    Ok(parts.len() == 3
        && parts
            .iter()
            .all(|part| !part.is_empty() && part.iter().all(|unit| (48..=57).contains(unit))))
}
/// Retain the pinned package manager string used in cache identity.
pub async fn preparation_package_manager(source_root: PathBuf) -> IvyResult<IvyJson> {
    internal(
        tokio::task::spawn_blocking(move || {
            let path = source_root.join("package.json");
            let value = read_json(&path, CONFIGURATION_BYTES).map_err(FileError::error)?;
            if value.is_null() {
                return Err(IvyError::internal());
            }
            let manager = value.get("packageManager");
            require(
                manager.map(pinned_npm).transpose()?.unwrap_or(false),
                "The distribution needs a pinned npm package manager.",
            )?;
            Ok(manager.unwrap().clone())
        })
        .await,
    )?
}

/// Exact original process coordinates; the Node version retains its leading v.
pub struct PreparationProfile {
    pub os: JsString,
    pub arch: JsString,
    pub node: JsString,
}
pub struct PreparationIdentity {
    pub dependency_lock_hash: String,
    pub dependency_key: String,
    pub source_identity: String,
    pub build_id: String,
}
impl PreparationIdentity {
    pub fn json(&self) -> IvyJson {
        IvyJson::object([
            (
                "dependencyLockHash",
                self.dependency_lock_hash.clone().into(),
            ),
            ("dependencyKey", self.dependency_key.clone().into()),
            ("sourceIdentity", self.source_identity.clone().into()),
            ("buildId", self.build_id.clone().into()),
        ])
    }
}
/// Run after temporary-root/tool admission, retaining the original lock-then-tree order.
/// Admitted hashing/inventory survives a caller dropping its wait.
pub async fn preparation_identity(
    source_root: PathBuf,
    component: JsString,
    version: JsString,
    package_manager: IvyJson,
    profile: PreparationProfile,
    compare: FileCompare,
) -> IvyResult<PreparationIdentity> {
    preparation_identity_for_platform(
        source_root,
        component,
        version,
        package_manager,
        profile,
        compare,
        None,
    )
    .await
}
pub async fn preparation_identity_for_platform(
    source_root: PathBuf,
    component: JsString,
    version: JsString,
    package_manager: IvyJson,
    profile: PreparationProfile,
    compare: FileCompare,
    native_platform: Option<JsString>,
) -> IvyResult<PreparationIdentity> {
    internal(
        tokio::spawn(async move {
            let lock = node_path::filesystem_path(&node_path::from_native(
                &source_root.join("package-lock.json"),
            ));
            let dependency_lock_hash = streaming_hash::file_hash(lock).await?;
            let dependency_key = dependency_cache_key_json(
                dependency_lock_hash.clone().into(),
                package_manager,
                profile.os,
                profile.arch,
                profile.node,
            )?;
            let files = artifact_files(source_root, compare).await?;
            let source_identity = artifact_source_identity(&files)?;
            let build_id = candidate_build_id_for_platform(
                &component,
                &version,
                &source_identity.clone().into(),
                native_platform.as_ref(),
            )?;
            Ok(PreparationIdentity {
                dependency_lock_hash,
                dependency_key,
                source_identity,
                build_id,
            })
        })
        .await,
    )?
}
