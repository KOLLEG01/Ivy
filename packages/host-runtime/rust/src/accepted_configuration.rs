//! Private immutable configuration snapshots captured at operation admission.
use crate::{IvyError, IvyJson, IvyResult, layout::{path, service_paths}, paths::inside, node_path};
use ivy_contracts::{JsString, core_schema::validate_host, json};
use std::{fs::{self, OpenOptions}, io::Write, path::{Path, PathBuf}};

const LIMIT: usize = 1024 * 1024;
fn require(condition: bool, code: &str, message: &str) -> IvyResult<()> {
    if condition { Ok(()) } else { Err(IvyError::new(code, message)) }
}
fn directory(config: &IvyJson) -> IvyResult<PathBuf> { Ok(path(config.required("runtimeRoot")?)?.join("accepted-configurations")) }
fn canonical(value: &IvyJson) -> IvyResult<String> { json::canonical(value, ivy_contracts::limits::MANAGEMENT_FRAME_BYTES) }
fn nullable(value: Option<&IvyJson>) -> &IvyJson { static NULL: IvyJson = IvyJson::Null; value.unwrap_or(&NULL) }
fn write_snapshot(directory: &Path, target: &Path, value: &IvyJson) -> IvyResult<()> {
    let directory = node_path::filesystem_path(&node_path::from_native(directory));
    let target = node_path::filesystem_path(&node_path::from_native(target));
    let mut builder = fs::DirBuilder::new(); builder.recursive(true);
    #[cfg(unix)] { use std::os::unix::fs::DirBuilderExt; builder.mode(0o700); }
    builder.create(&directory).map_err(|_| IvyError::internal())?;
    let mut options = OpenOptions::new(); options.write(true).create_new(true);
    #[cfg(unix)] { use std::os::unix::fs::OpenOptionsExt; options.mode(0o600); }
    let mut file = options.open(target).map_err(|_| IvyError::internal())?;
    let mut text = json::canonical(value, LIMIT - 1)?; text.push('\n');
    file.write_all(text.as_bytes()).and_then(|_| file.sync_all()).map_err(|_| IvyError::internal())?;
    drop(file);
    #[cfg(unix)] { fs::File::open(directory).and_then(|file| file.sync_all()).map_err(|_| IvyError::internal())?; }
    Ok(())
}
pub fn save_accepted_configuration(config: &IvyJson) -> IvyResult<PathBuf> {
    validate_host("HostConfig", config)?;
    let directory = directory(config)?;
    let target = directory.join(format!("{}.json", uuid::Uuid::new_v4()));
    write_snapshot(&directory, &target, config)?;
    Ok(target)
}
pub fn accepted_instance_configuration_path(config: &IvyJson, revision: &str) -> IvyResult<PathBuf> {
    let revision = if revision.len() == 71 && revision.starts_with("sha256:") && revision.as_bytes()[7..].iter().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(b)) {
        format!("bootstrap-{}", &revision[7..])
    } else { revision.to_string() };
    require(!revision.is_empty() && revision.len() <= 128 && revision.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-'),
        "target_conflict", "Runtime target revision cannot name an accepted configuration.")?;
    Ok(directory(config)?.join(format!("{revision}.instance.json")))
}
fn direct_file(source: &Path, message: &str) -> IvyResult<()> {
    let metadata = fs::symlink_metadata(crate::node_path::filesystem_path(&crate::node_path::from_native(source))).map_err(|_| IvyError::internal())?;
    require(metadata.is_file() && !metadata.file_type().is_symlink() && metadata.len() <= LIMIT as u64, "configuration_changed", message)
}
fn read_value(source: &Path, schema: &str, message: &str) -> IvyResult<IvyJson> {
    let read = || -> IvyResult<IvyJson> {
        let bytes = fs::read(crate::node_path::filesystem_path(&crate::node_path::from_native(source))).map_err(|_| IvyError::internal())?;
        let value = json::parse(&String::from_utf8_lossy(&bytes), usize::MAX)?;
        validate_host(schema, &value)?;
        Ok(value)
    };
    read().map_err(|_| IvyError::new("configuration_changed", message))
}
pub fn save_accepted_instance_configuration(config: &IvyJson, source: &Path, revision: &str) -> IvyResult<PathBuf> {
    save_accepted_instance_configuration_js(config, &node_path::from_native(source), revision)
}
// Preserve lexical UTF-16 through the containment check on every platform;
// Node's replacement encoding applies only at the filesystem boundary.
pub(crate) fn save_accepted_instance_configuration_js(config: &IvyJson, source: &JsString, revision: &str) -> IvyResult<PathBuf> {
    let root = config.required("runtimeRoot")?.as_js_string().ok_or_else(IvyError::internal)?;
    require(node_path::Context::current().map_err(|_| IvyError::internal())?.inside(root, source), "target_conflict", "Previous runtime configuration leaves host runtime data.")?;
    let source = node_path::filesystem_path(source);
    direct_file(&source, "Previous runtime configuration is not a bounded direct file.")?;
    let value = read_value(&source, "InstanceConfig", "Previous runtime configuration is unreadable or invalid.")?;
    let directory = directory(config)?;
    // Match the original ordering: the directory is created before revision validation.
    let mut builder = fs::DirBuilder::new(); builder.recursive(true);
    #[cfg(unix)] { use std::os::unix::fs::DirBuilderExt; builder.mode(0o700); }
    builder.create(node_path::filesystem_path(&node_path::from_native(&directory))).map_err(|_| IvyError::internal())?;
    let target = accepted_instance_configuration_path(config, revision)?;
    write_snapshot(&directory, &target, &value)?;
    Ok(target)
}
fn check_accepted(anchor: &IvyJson, source: &Path, instance: bool) -> IvyResult<()> {
    let directory = directory(anchor)?;
    require(inside(&directory, source).map_err(|_| IvyError::internal())?, "target_conflict", if instance { "Accepted instance configuration leaves host runtime data." } else { "Accepted configuration leaves host runtime data." })?;
    let message = if instance { "Accepted instance configuration is not a bounded direct file." } else { "Accepted configuration is not a bounded direct file." };
    direct_file(source, message)?;
    require(source.parent() == Some(directory.as_path()), "configuration_changed", message)
}
pub fn read_accepted_instance_configuration(anchor: &IvyJson, source: &Path) -> IvyResult<IvyJson> {
    check_accepted(anchor, source, true)?;
    read_value(source, "InstanceConfig", "Accepted instance configuration is unreadable or invalid.")
}
pub fn read_accepted_configuration(anchor: &IvyJson, source: &Path) -> IvyResult<IvyJson> {
    check_accepted(anchor, source, false)?;
    let config = read_value(source, "HostConfig", "Accepted configuration is unreadable or invalid.")?;
    same_installation(anchor, &config)?;
    Ok(config)
}
pub fn same_installation(anchor: &IvyJson, next: &IvyJson) -> IvyResult<()> {
    require(["hostId", "runtimeRoot", "artifactRoot", "stagingRoot", "servicesRoot", "configPath"].iter().all(|key| anchor.get(key) == next.get(key)) &&
        anchor.required("executables")?.get("node") == next.required("executables")?.get("node"), "target_conflict", "Host identity, operational roots and bootstrap Node executable cannot change within an installation.")?;
    require(canonical(nullable(anchor.get("restoredFrom")))? == canonical(nullable(next.get("restoredFrom")))?, "target_conflict", "An installation cannot discard or substitute its original recovery identity.")?;
    let others = next.required("instances")?.as_array().ok_or_else(IvyError::internal)?;
    for instance in anchor.required("instances")?.as_array().ok_or_else(IvyError::internal)? {
        if let Some(other) = others.iter().find(|v| v.get("instanceId") == instance.get("instanceId")) {
            require(other.get("componentId") == instance.get("componentId") && other.get("serviceNodeId") == instance.get("serviceNodeId"), "target_conflict", "An existing instance identity cannot be reused for another component or Service Node.")?;
            require(canonical(&service_paths(anchor, instance)?)? == canonical(&service_paths(next, other)?)?, "target_conflict", "Existing service storage cannot move through a configuration edit; use an explicit isolated restore.")?;
        }
    }
    Ok(())
}
pub fn installation_record(config: &IvyJson) -> IvyResult<String> {
    let mut record = IvyJson::object(Vec::<(String, IvyJson)>::new());
    for key in ["runtimeRoot", "artifactRoot", "stagingRoot"] { record.insert(key, config.required(key)?.clone()); }
    for key in ["servicesRoot", "configPath", "restoredFrom"] { record.insert(key, nullable(config.get(key)).clone()); }
    if let Some(node) = config.required("executables")?.get("node") { record.insert("node", node.clone()); }
    canonical(&record)
}
