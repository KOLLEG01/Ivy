//! Runtime release-manifest boundary shared by retained and new candidates.
use crate::{IvyError, IvyJson, IvyResult, config::{path_value, read_json, FileError, CONFIGURATION_BYTES}, node_runtime::NodeRuntime, paths::{inside, realpath}};
use ivy_contracts::{JsString, core_schema::validate_host};
use std::{fs, path::{Path, PathBuf}};
pub use crate::streaming_hash::file_hash;
pub use crate::artifact_inventory::{ArtifactFile, FileCompare, artifact_files, artifact_source_identity};

fn internal<T, E>(value: Result<T, E>) -> IvyResult<T> { value.map_err(|_| IvyError::internal()) }
fn require(value: bool, code: &str, message: &str) -> IvyResult<()> {
    if value { Ok(()) } else { Err(IvyError::new(code, message)) }
}
pub use crate::artifact_manifest::{verify_transferred_archive, release_manifest, read_release_manifest};

fn verify_launch(candidate: &IvyJson, artifact_root: &Path, entrypoint: Option<&JsString>) -> IvyResult<()> {
    validate_host("Candidate", candidate)?;
    let root = path_value(candidate.required("artifactRoot")?)?;
    require(internal(inside(artifact_root, &internal(realpath(&root))?))?, "target_conflict", "Launch leaves its artifact root.")?;
    let mut entries = vec![PathBuf::from("dist/build-info.json")];
    if let Some(entry) = entrypoint.filter(|entry| entry.0.starts_with(&[100, 105, 115, 116, 47])) {
        entries.push(path_value(&IvyJson::String(entry.clone()))?);
    }
    for selected in entries {
        let local = root.join(selected); let metadata = internal(fs::symlink_metadata(&local))?;
        require(metadata.is_file() && !metadata.file_type().is_symlink() && internal(inside(&root, &internal(realpath(&local))?))?,
            "artifact_changed", "Launch entry or stamp is unavailable.")?;
    }
    let info = read_json(&root.join("dist/build-info.json"), CONFIGURATION_BYTES).map_err(FileError::error)?;
    if info.is_null() { return Err(IvyError::internal()); }
    require(info.get("buildId") == candidate.get("buildId"), "build_mismatch", "Launch build differs from its prepared identity.")
}
pub async fn verify_launch_candidate(candidate: IvyJson, artifact_root: impl Into<PathBuf>, entrypoint: Option<JsString>) -> IvyResult<()> {
    let root = artifact_root.into();
    internal(tokio::task::spawn_blocking(move || verify_launch(&candidate, &root, entrypoint.as_ref())).await)?
}
/// Owners call this with the candidate's frozen journal manifest before drain.
/// Keep the legacy function above available for ordinary v1 callers; inspecting
/// native intent needs both retained command paths and the shutdown policy.
pub async fn verify_launch_candidate_with_manifest(candidate:IvyJson,artifact_root:impl Into<PathBuf>,entrypoint:Option<JsString>,manifest:IvyJson)->IvyResult<()>{
    let root=artifact_root.into();
    internal(tokio::task::spawn_blocking(move || {
        verify_launch(&candidate, &root, entrypoint.as_ref())?;
        let manifest=release_manifest(&manifest)?;
        require(manifest.get("buildId")==candidate.get("buildId")&&manifest.get("componentId")==candidate.get("componentId"),"build_mismatch","Launch manifest differs from its prepared release identity.")?;
        crate::native_dispatch::verify(&candidate,&path_value(candidate.required("artifactRoot")?)?,&manifest)
    }).await)?
}

/// The v1 dispatcher/host supplies the discovered intentional Node workload
/// runtime. Package OS and architecture requirements apply to this Rust host.
pub async fn verify_candidate(candidate: IvyJson, artifact_root: impl Into<PathBuf>, node: &NodeRuntime) -> IvyResult<IvyJson> {
    let root = artifact_root.into(); let node = node.version.clone();
    internal(tokio::task::spawn_blocking(move || {
        validate_host("Candidate", &candidate)?;
        let candidate_root = path_value(candidate.required("artifactRoot")?)?;
        let manifest_path = path_value(candidate.required("manifestPath")?)?;
        for selected in [&candidate_root, &manifest_path] {
            require(internal(inside(&root, &internal(realpath(selected))?))?, "target_conflict", "Candidate leaves its explicit host artifact root.")?;
        }
        let manifest = release_manifest(&read_json(&manifest_path, CONFIGURATION_BYTES).map_err(FileError::error)?)?;
        if manifest.get("kind") != Some(&IvyJson::from("app")) {
            let requirements = manifest.required("requirements")?;
            let strings = |key: &str| -> IvyResult<Option<Vec<String>>> {
                requirements.get(key).map(|value| value.as_array().ok_or_else(IvyError::internal)?.iter().map(IvyJson::as_str).collect()).transpose()
            };
            let os = strings("os")?; let arch = strings("arch")?;
            let os_refs = os.as_ref().map(|values| values.iter().map(String::as_str).collect::<Vec<_>>());
            let arch_refs = arch.as_ref().map(|values| values.iter().map(String::as_str).collect::<Vec<_>>());
            let os = if cfg!(windows) { "win32" } else { std::env::consts::OS };
            let arch = match std::env::consts::ARCH { "x86_64" => "x64", "aarch64" => "arm64", value => value };
            let range = requirements.required("node")?.as_str()?;
            let supported = crate::package_requirements::runtime_requirements_satisfied(
                &crate::package_requirements::Requirements { node: &range, os: os_refs.as_deref(), arch: arch_refs.as_deref() },
                &crate::package_requirements::Runtime { node: &node, os, arch })?;
            let version = if node.starts_with('v') { node.clone() } else { format!("v{node}") };
            require(supported, "unsupported_runtime", &format!("Package does not support {os}/{arch} on {version}."))?;
        }
        require(manifest.get("buildId") == candidate.get("buildId") && manifest.get("componentId") == candidate.get("componentId"),
            "build_mismatch", "Candidate manifest does not identify its release.")?;
        let entrypoint = manifest.get("entrypoint").and_then(|v| v.get("args")).and_then(IvyJson::as_array).and_then(|args| args.first()).and_then(IvyJson::as_js_string);
        verify_launch(&candidate, &root, entrypoint)?;
        crate::native_dispatch::verify(&candidate,&candidate_root,&manifest)?;
        if cfg!(windows) {
            for selected in crate::portable_native_files::portable_native_files(&manifest.required("componentId")?.as_str()?) {
                let local = candidate_root.join(selected);
                let metadata = fs::symlink_metadata(&local).ok();
                let valid = metadata.is_some_and(|m| m.is_file() && !m.file_type().is_symlink() && m.len() > 0);
                require(valid && internal(inside(&candidate_root, &internal(realpath(&local))?))?, "artifact_missing", "Windows candidate is missing its required native runtime payload.")?;
            }
        }
        Ok(manifest)
    }).await)?
}
