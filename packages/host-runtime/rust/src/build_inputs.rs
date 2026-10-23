//! Exact additional source files declared by the selected component and target.
use crate::{IvyError, IvyResult, paths};
use ivy_contracts::JsString;
use serde::Deserialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
};

const TARGETS: [&str; 4] = ["win32-x64", "win32-arm64", "linux-x64", "linux-arm64"];
const DECLARATION_BYTES: u64 = 64 * 1024;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Declaration {
    schema_version: u32,
    targets: BTreeMap<String, Vec<String>>,
}

fn invalid() -> IvyError {
    IvyError::new(
        "invalid_arguments",
        "Build inputs require a bounded version 1 target map of distinct checkout-relative file paths.",
    )
}
fn valid_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 1024
        && !path
            .chars()
            .any(|c| c.is_control() || "\\:*?[]{}".contains(c))
        && path.split('/').all(|part| {
            !part.is_empty() && part != "." && part != ".." && !part.ends_with(['.', ' '])
        })
}

/// Include the declaration itself even when Git ignores it. Only selected-target
/// files are required; other components and target runtimes are not inspected.
pub(crate) fn source_inputs(
    root: &Path,
    component: &str,
    target: &str,
) -> IvyResult<Vec<JsString>> {
    if !TARGETS.contains(&target) {
        return Err(invalid());
    }
    let declaration = format!("services/{component}/build-inputs.json");
    match fs::symlink_metadata(root.join(&declaration)) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(_) => return Err(invalid()),
        Ok(_) => {}
    }
    let file = paths::direct_native_file(root, &declaration)?;
    if fs::metadata(&file).map_err(|_| invalid())?.len() > DECLARATION_BYTES {
        return Err(invalid());
    }
    let bytes = fs::read(file).map_err(|_| invalid())?;
    if bytes.len() as u64 > DECLARATION_BYTES {
        return Err(invalid());
    }
    let value: Declaration = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
    if value.schema_version != 1 || value.targets.len() > TARGETS.len() {
        return Err(invalid());
    }
    for (platform, files) in &value.targets {
        let mut seen = BTreeSet::new();
        if !TARGETS.contains(&platform.as_str())
            || files.len() > 128
            || files
                .iter()
                .any(|path| !valid_path(path) || !seen.insert(path.to_ascii_lowercase()))
        {
            return Err(invalid());
        }
    }
    let mut selected = vec![declaration.into()];
    for path in value.targets.get(target).into_iter().flatten() {
        let file = paths::direct_native_file(root, path)?;
        if fs::metadata(file).map_err(|_| invalid())?.len() > 64 * 1024 * 1024 {
            return Err(IvyError::new(
                "limit_exceeded",
                "Source copy exceeds its per-file byte limit.",
            ));
        }
        selected.push(path.as_str().into());
    }
    Ok(selected)
}
