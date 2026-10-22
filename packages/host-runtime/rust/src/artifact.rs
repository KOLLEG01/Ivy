//! Runtime release-manifest boundary shared by retained and new candidates.
pub use crate::artifact_inventory::{
    ArtifactFile, FileCompare, artifact_files, artifact_source_identity, native_file_compare,
};
pub use crate::artifact_manifest::{
    read_release_manifest, release_manifest, verify_transferred_archive,
};
pub use crate::streaming_hash::file_hash;
use crate::{
    IvyError, IvyJson, IvyResult,
    config::{CONFIGURATION_BYTES, FileError, path_value, read_json},
    node_runtime::NodeRuntime,
    paths::{inside, realpath},
};
use ivy_contracts::{JsString, core_schema::validate_host};
use std::{
    fs,
    path::{Path, PathBuf},
};

fn internal<T, E>(value: Result<T, E>) -> IvyResult<T> {
    value.map_err(|_| IvyError::internal())
}
fn require(value: bool, code: &str, message: &str) -> IvyResult<()> {
    if value {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
pub fn launch_entrypoint(manifest: &IvyJson) -> Option<&JsString> {
    let command = manifest.get("entrypoint")?;
    if crate::native_plan::is_native_plan(manifest) {
        command.get("executable")?.as_js_string()
    } else {
        command.get("args")?.as_array()?.first()?.as_js_string()
    }
}
pub(crate) fn verify_runtime_requirements(
    manifest: &IvyJson,
    node: Option<&str>,
    os: &str,
    arch: &str,
) -> IvyResult<()> {
    if manifest.get("kind") == Some(&"app".into()) {
        return Ok(());
    }
    let requirements = manifest.required("requirements")?;
    for (key, actual) in [("os", os), ("arch", arch)] {
        if let Some(values) = requirements.get(key) {
            require(
                values
                    .as_array()
                    .is_some_and(|values| values.contains(&actual.into())),
                "unsupported_runtime",
                &format!("Package does not support {os}/{arch}."),
            )?;
        }
    }
    let native_health = manifest
        .get("readiness")
        .and_then(|v| v.get("command"))
        .and_then(|v| v.get("executable"))
        .and_then(IvyJson::as_js_string)
        .is_some_and(|v| {
            v.0.starts_with(&"dist/native/".encode_utf16().collect::<Vec<_>>())
        });
    if crate::native_plan::is_native_plan(manifest) && native_health {
        return Ok(());
    }
    let node = node.ok_or_else(|| {
        IvyError::new(
            "unsupported_runtime",
            "This workload requires an explicit Node runtime.",
        )
    })?;
    let supported = crate::package_requirements::runtime_requirements_satisfied(
        &crate::package_requirements::Requirements {
            node: &requirements.required("node")?.as_str()?,
            os: None,
            arch: None,
        },
        &crate::package_requirements::Runtime { node, os, arch },
    )?;
    require(
        supported,
        "unsupported_runtime",
        &format!("Package does not support {os}/{arch} on {node}."),
    )
}
fn native_payload(candidate: &IvyJson, root: &Path, manifest: &IvyJson) -> IvyResult<()> {
    if !crate::native_plan::is_native_plan(manifest) {
        return crate::native_dispatch::verify(candidate, root, manifest);
    }
    let executable = manifest
        .required("entrypoint")?
        .required("executable")?
        .as_str()?;
    let directory = executable
        .rsplit_once('/')
        .ok_or_else(IvyError::internal)?
        .0;
    let selected = format!(
        "dist/native/{}",
        crate::portable_native_files::native_platform()?
    );
    require(
        directory == "dist/native" || directory == selected,
        "unsupported_runtime",
        "Native executable targets another platform.",
    )?;
    let command = manifest.required("readiness")?.required("command")?;
    let health = command.required("executable")?.as_str()?;
    let mut required = vec![executable.clone()];
    if health.starts_with("dist/native/") {
        require(
            health
                .rsplit_once('/')
                .is_some_and(|(parent, _)| parent == directory),
            "artifact_changed",
            "Native readiness must use the selected artifact platform.",
        )?;
        required.push(health);
    }
    required.extend(crate::portable_native_files::portable_native_files_for_manifest(manifest)?);
    for file in required {
        require(
            !file
                .split('/')
                .any(|part| part.is_empty() || part == "." || part == ".." || part.contains('\\')),
            "artifact_changed",
            "Native payload paths must be direct contained files.",
        )?;
        crate::paths::direct_native_file(root, &file)?;
    }
    if directory != "dist/native" {
        let stamp =
            crate::paths::direct_native_file(root, &format!("{directory}/build-info.json"))?;
        let info = read_json(&stamp, CONFIGURATION_BYTES).map_err(FileError::error)?;
        require(
            info.get("schemaVersion") == Some(&IvyJson::Number(1.0))
                && info.get("buildId") == candidate.get("buildId")
                && info.get("version") == manifest.get("version")
                && info
                    .get("componentId")
                    .is_none_or(|v| Some(v) == candidate.get("componentId")),
            "build_mismatch",
            "Native target build differs from its prepared release identity.",
        )?;
    }
    Ok(())
}
fn verify_launch(
    candidate: &IvyJson,
    artifact_root: &Path,
    entrypoint: Option<&JsString>,
) -> IvyResult<()> {
    validate_host("Candidate", candidate)?;
    let root = path_value(candidate.required("artifactRoot")?)?;
    require(
        internal(inside(artifact_root, &internal(realpath(&root))?))?,
        "target_conflict",
        "Launch leaves its artifact root.",
    )?;
    let mut entries = vec![PathBuf::from("dist/build-info.json")];
    if let Some(entry) = entrypoint.filter(|entry| entry.0.starts_with(&[100, 105, 115, 116, 47])) {
        entries.push(path_value(&IvyJson::String(entry.clone()))?);
    }
    for selected in entries {
        let local = root.join(selected);
        let metadata = internal(fs::symlink_metadata(&local))?;
        require(
            metadata.is_file()
                && !metadata.file_type().is_symlink()
                && internal(inside(&root, &internal(realpath(&local))?))?,
            "artifact_changed",
            "Launch entry or stamp is unavailable.",
        )?;
    }
    let info = read_json(&root.join("dist/build-info.json"), CONFIGURATION_BYTES)
        .map_err(FileError::error)?;
    if info.is_null() {
        return Err(IvyError::internal());
    }
    require(
        info.get("buildId") == candidate.get("buildId"),
        "build_mismatch",
        "Launch build differs from its prepared identity.",
    )
}
pub async fn verify_launch_candidate(
    candidate: IvyJson,
    artifact_root: impl Into<PathBuf>,
    entrypoint: Option<JsString>,
) -> IvyResult<()> {
    let root = artifact_root.into();
    internal(
        tokio::task::spawn_blocking(move || verify_launch(&candidate, &root, entrypoint.as_ref()))
            .await,
    )?
}
pub async fn verify_launch_candidate_with_manifest(
    candidate: IvyJson,
    artifact_root: impl Into<PathBuf>,
    entrypoint: Option<JsString>,
    manifest: IvyJson,
) -> IvyResult<()> {
    let root = artifact_root.into();
    internal(
        tokio::task::spawn_blocking(move || {
            let manifest = release_manifest(&manifest)?;
            let entry = if crate::native_plan::is_native_plan(&manifest) {
                launch_entrypoint(&manifest)
            } else {
                entrypoint.as_ref().or_else(|| launch_entrypoint(&manifest))
            };
            verify_launch(&candidate, &root, entry)?;
            require(
                manifest.get("buildId") == candidate.get("buildId")
                    && manifest.get("componentId") == candidate.get("componentId"),
                "build_mismatch",
                "Launch manifest differs from its prepared release identity.",
            )?;
            native_payload(
                &candidate,
                &path_value(candidate.required("artifactRoot")?)?,
                &manifest,
            )
        })
        .await,
    )?
}
pub async fn verify_candidate(
    candidate: IvyJson,
    artifact_root: impl Into<PathBuf>,
    node: &NodeRuntime,
) -> IvyResult<IvyJson> {
    verify_candidate_for_runtime(candidate, artifact_root.into(), Some(node.version.clone())).await
}
/// Native source plans do not require Node discovery for artifact admission.
pub async fn verify_native_candidate(
    candidate: IvyJson,
    artifact_root: impl Into<PathBuf>,
) -> IvyResult<IvyJson> {
    verify_candidate_for_runtime(candidate, artifact_root.into(), None).await
}
async fn verify_candidate_for_runtime(
    candidate: IvyJson,
    root: PathBuf,
    node: Option<String>,
) -> IvyResult<IvyJson> {
    internal(
        tokio::task::spawn_blocking(move || {
            validate_host("Candidate", &candidate)?;
            let candidate_root = path_value(candidate.required("artifactRoot")?)?;
            let manifest_path = path_value(candidate.required("manifestPath")?)?;
            for selected in [&candidate_root, &manifest_path] {
                require(
                    internal(inside(&root, &internal(realpath(selected))?))?,
                    "target_conflict",
                    "Candidate leaves its explicit host artifact root.",
                )?;
            }
            let manifest = release_manifest(
                &read_json(&manifest_path, CONFIGURATION_BYTES).map_err(FileError::error)?,
            )?;
            let os = if cfg!(windows) {
                "win32"
            } else {
                std::env::consts::OS
            };
            let arch = match std::env::consts::ARCH {
                "x86_64" => "x64",
                "aarch64" => "arm64",
                other => other,
            };
            verify_runtime_requirements(&manifest, node.as_deref(), os, arch)?;
            require(
                manifest.get("buildId") == candidate.get("buildId")
                    && manifest.get("componentId") == candidate.get("componentId"),
                "build_mismatch",
                "Candidate manifest does not identify its release.",
            )?;
            verify_launch(&candidate, &root, launch_entrypoint(&manifest))?;
            native_payload(&candidate, &candidate_root, &manifest)?;
            if !crate::native_plan::is_native_plan(&manifest) {
                for selected in
                    crate::portable_native_files::portable_native_files_for_manifest(&manifest)?
                {
                    let local = candidate_root.join(selected);
                    let metadata = fs::symlink_metadata(&local).ok();
                    let valid = metadata
                        .is_some_and(|m| m.is_file() && !m.file_type().is_symlink() && m.len() > 0);
                    require(
                        valid && internal(inside(&candidate_root, &internal(realpath(&local))?))?,
                        "artifact_missing",
                        "Candidate is missing its required native runtime payload.",
                    )?;
                }
            }
            Ok(manifest)
        })
        .await,
    )?
}
