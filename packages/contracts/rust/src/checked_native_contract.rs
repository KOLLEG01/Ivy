//! Offline retained catalogs. Live admission must use the native owner's snapshot.
pub use crate::native_catalog_path::native_catalog_path;
use crate::{
    IvyError, IvyJson, IvyResult, JsString,
    json::{parse, stringify},
    native_contract::NativeContract,
};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
};

/// Derived from the same normative Agent schema as the public native read contract.
pub fn native_versions() -> IvyResult<&'static [JsString]> {
    static VERSIONS: OnceLock<IvyResult<Vec<JsString>>> = OnceLock::new();
    VERSIONS
        .get_or_init(|| {
            let schema = parse(
                include_str!("../../../../specs/schemas/agent.schema.json"),
                usize::MAX,
            )?;
            let mut value = &schema;
            for key in [
                "$defs",
                "PreventInput",
                "properties",
                "nativeVersion",
                "enum",
            ] {
                value = value.get(key).ok_or_else(IvyError::internal)?;
            }
            value
                .as_array()
                .ok_or_else(IvyError::internal)?
                .iter()
                .map(|v| v.as_js_string().cloned().ok_or_else(IvyError::internal))
                .collect()
        })
        .as_ref()
        .map(Vec::as_slice)
        .map_err(Clone::clone)
}
fn unsupported(message: &str) -> IvyError {
    IvyError::new("native_version_unsupported", message)
}
fn mismatch(message: &str) -> IvyError {
    IvyError::new("native_catalog_mismatch", message)
}
fn missing() -> IvyError {
    unsupported("The requested native catalog is not retained in this distribution.")
}
fn exact_hash(value: &IvyJson) -> bool {
    let Some(value) = value.as_js_string() else {
        return false;
    };
    value.0.len() == 71
        && value.0.starts_with(&[115, 104, 97, 50, 53, 54, 58])
        && value.0[7..]
            .iter()
            .all(|c| (48..=57).contains(c) || (97..=102).contains(c))
}
fn catalog_filename(name: &str) -> bool {
    if name == "catalog.json" {
        return true;
    }
    let Some(middle) = name
        .strip_prefix("catalog.")
        .and_then(|v| v.strip_suffix(".json"))
    else {
        return false;
    };
    let Some((platform, arch)) = middle.split_once('-') else {
        return false;
    };
    [platform, arch].iter().all(|v| {
        !v.is_empty()
            && v.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
    })
}
fn load(path: &Path) -> IvyResult<NativeContract> {
    let bytes = std::fs::read(path).map_err(|_| IvyError::internal())?;
    let value =
        parse(&String::from_utf8_lossy(&bytes), usize::MAX).map_err(|_| IvyError::internal())?;
    NativeContract::new(&value)
}

/// Keep one store for the distribution root. Successful selections survive later
/// file changes; failed reads/selection are not cached. Callers supply the runtime
/// distribution root, never a compiled-in source or installation path.
pub struct CheckedNativeContracts {
    root: PathBuf,
    cache: Mutex<HashMap<String, Arc<NativeContract>>>,
}
impl CheckedNativeContracts {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            cache: Mutex::new(HashMap::new()),
        }
    }
    pub fn checked_native_contract(
        &self,
        version: &JsString,
        identity: Option<&IvyJson>,
    ) -> IvyResult<Arc<NativeContract>> {
        if !native_versions()?.contains(version) {
            return Err(unsupported(
                "A supported native catalog version is required.",
            ));
        }
        let version_text = version.to_utf8()?;
        let key = format!(
            "{}:{}",
            version_text,
            stringify(identity.unwrap_or(&IvyJson::Null), usize::MAX)?
        );
        // The original synchronous loader serializes reads and cache insertion.
        let mut cache = self.cache.lock().map_err(|_| IvyError::internal())?;
        if let Some(value) = cache.get(&key) {
            return Ok(value.clone());
        }
        let directory = self
            .root
            .join("specs")
            .join("native")
            .join(format!("codex-{version_text}"));
        let selected = if let Some(identity) = identity {
            if identity.is_null() {
                return Err(IvyError::internal());
            }
            let fields = identity.as_object().ok_or_else(|| {
                mismatch("An explicit native catalog identity must contain exact hashes.")
            })?;
            if fields.is_empty()
                || !fields.iter().all(|(key, value)| {
                    ["sourceHash", "catalogHash", "nativeExecutableHash"]
                        .iter()
                        .any(|allowed| key == &JsString::from(*allowed))
                        && exact_hash(value)
                })
            {
                return Err(mismatch(
                    "An explicit native catalog identity must contain exact hashes.",
                ));
            }
            let mut files = std::fs::read_dir(&directory)
                .map_err(|_| missing())?
                .map(|entry| entry.map_err(|_| missing()).map(|v| v.file_name()))
                .collect::<IvyResult<Vec<_>>>()?;
            files.retain(|name| name.to_str().is_some_and(catalog_filename));
            // Node's scandir results are sorted; matching names are ASCII.
            files.sort();
            let mut selected = None;
            for file in files {
                let contract = load(&directory.join(file))?;
                if contract
                    .catalog()
                    .get("version")
                    .and_then(IvyJson::as_js_string)
                    != Some(version)
                {
                    continue;
                }
                if identity
                    .get("sourceHash")
                    .is_some_and(|v| Some(v) != contract.catalog().get("sourceHash"))
                    || identity
                        .get("nativeExecutableHash")
                        .is_some_and(|v| Some(v) != contract.catalog().get("nativeExecutableHash"))
                    || identity.get("catalogHash").is_some_and(|v| {
                        v.as_js_string() != Some(&JsString::from(contract.catalog_hash()))
                    })
                {
                    continue;
                }
                selected = Some(contract);
                break;
            }
            selected.ok_or_else(|| mismatch("No retained native platform build matches the explicitly selected catalog identity."))?
        } else {
            let platform = match std::env::consts::OS {
                "windows" => "win32",
                "macos" => "darwin",
                other => other,
            };
            let architecture = match std::env::consts::ARCH {
                "x86_64" => "x64",
                "aarch64" => "arm64",
                "x86" => "ia32",
                other => other,
            };
            let platform_path = directory.join(format!("catalog.{platform}-{architecture}.json"));
            let path = if platform_path.exists() {
                platform_path
            } else {
                directory.join("catalog.json")
            };
            let bytes = std::fs::read(path).map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    missing()
                } else {
                    IvyError::internal()
                }
            })?;
            let value = parse(&String::from_utf8_lossy(&bytes), usize::MAX)
                .map_err(|_| IvyError::internal())?;
            let contract = NativeContract::new(&value)?;
            if contract
                .catalog()
                .get("version")
                .and_then(IvyJson::as_js_string)
                != Some(version)
            {
                return Err(mismatch(
                    "The retained catalog identifies another native version.",
                ));
            }
            contract
        };
        let selected = Arc::new(selected);
        cache.insert(key, selected.clone());
        Ok(selected)
    }
}
