//! Read-only catalog checks before the host drains an existing service.
use super::concurrency::map_concurrent;
use crate::native_path::{from_native, to_native};
use futures_util::{
    FutureExt,
    future::{BoxFuture, Shared},
};
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString, agent_schema::validate_agent,
    checked_native_contract::native_catalog_path, hash::hash_json, json::parse,
};
use ivy_sdk::{
    rpc::{RequestOptions, RpcClient},
    tools::{BoundToolClient, InterfaceRequirement, ToolCallOptions},
};
use std::{
    collections::{HashMap, HashSet},
    ffi::OsString,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
    time::Duration,
};

#[derive(Clone)]
struct CatalogIdentity {
    version: JsString,
    executable: JsString,
}
type Catalogs = Arc<HashMap<JsString, CatalogIdentity>>;
type Pending = Arc<Shared<BoxFuture<'static, IvyResult<Catalogs>>>>;

fn field(value: &IvyJson, key: &str) -> IvyResult<JsString> {
    value
        .required(key)?
        .as_js_string()
        .cloned()
        .ok_or_else(IvyError::internal)
}
fn message(parts: &[JsString]) -> JsString {
    JsString(parts.iter().flat_map(|s| s.0.iter().copied()).collect())
}
async fn catalog(path: &Path) -> IvyResult<IvyJson> {
    let bytes = tokio::fs::read(path)
        .await
        .map_err(|_| IvyError::internal())?;
    let value =
        parse(&String::from_utf8_lossy(&bytes), usize::MAX).map_err(|_| IvyError::internal())?;
    validate_agent("Catalog", &value)?;
    Ok(value)
}

/// Settings have already passed the owning host's settings validator.
pub async fn verify_configured_native_catalog(
    artifact_root: &Path,
    instance_id: &JsString,
    settings: &IvyJson,
) -> IvyResult<()> {
    let version = field(settings, "nativeVersion")?;
    let executable = field(settings, "nativeExecutableHash")?;
    // The checked selector's public interface uses configuration path strings.
    let selected = native_catalog_path(
        &from_native(&artifact_root.join("dist")),
        &version,
        None,
        None,
    )?;
    let value = catalog(&to_native(&selected)).await?;
    let required_executable = field(&value, "nativeExecutableHash")?;
    if field(&value, "version")? != version || required_executable != executable {
        return Err(IvyError::new_js(
            "incompatible_native_catalog".into(),
            message(&[
                instance_id.clone(),
                " is configured for native version ".into(),
                version,
                ", executable ".into(),
                executable,
                ", but the candidate requires executable ".into(),
                required_executable,
                ". Prepare the matching native runtime and configuration before retrying.".into(),
            ]),
        ));
    }
    Ok(())
}
fn version_directory(name: &str) -> bool {
    let Some(version) = name.strip_prefix("codex-") else {
        return false;
    };
    let parts: Vec<_> = version.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
}
fn catalog_filename(name: &str) -> bool {
    if name == "catalog.json" {
        return true;
    }
    let Some(name) = name
        .strip_prefix("catalog.")
        .and_then(|s| s.strip_suffix(".json"))
    else {
        return false;
    };
    let Some((platform, arch)) = name.split_once('-') else {
        return false;
    };
    [platform, arch].iter().all(|s| {
        !s.is_empty()
            && s.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
    })
}
async fn entries(root: &Path) -> IvyResult<Vec<tokio::fs::DirEntry>> {
    let mut directory = tokio::fs::read_dir(root)
        .await
        .map_err(|_| IvyError::internal())?;
    let mut entries = Vec::new();
    while let Some(entry) = directory
        .next_entry()
        .await
        .map_err(|_| IvyError::internal())?
    {
        entries.push(entry);
    }
    // Matching names are ASCII; preserve Node scandir order and failure precedence.
    entries.sort_by_key(|entry| entry.file_name());
    Ok(entries)
}
async fn load_catalogs(root: PathBuf) -> IvyResult<Catalogs> {
    let mut catalogs = HashMap::new();
    for directory in entries(&root.join("dist/specs/native")).await? {
        if !directory
            .file_type()
            .await
            .map_err(|_| IvyError::internal())?
            .is_dir()
            || !directory
                .file_name()
                .to_str()
                .is_some_and(version_directory)
        {
            continue;
        }
        for entry in entries(&directory.path()).await? {
            if !entry.file_name().to_str().is_some_and(catalog_filename) {
                continue;
            }
            let value = catalog(&entry.path()).await?;
            catalogs.insert(
                hash_json(&value)?.into(),
                CatalogIdentity {
                    version: field(&value, "version")?,
                    executable: field(&value, "nativeExecutableHash")?,
                },
            );
        }
    }
    if catalogs.is_empty() {
        return Err(IvyError::new(
            "incompatible_native_catalog",
            "The candidate declares native catalog consumption but contains no retained catalogs.",
        ));
    }
    Ok(Arc::new(catalogs))
}
async fn candidate_catalogs(root: &Path) -> IvyResult<Catalogs> {
    static RETAINED: OnceLock<Mutex<HashMap<OsString, Pending>>> = OnceLock::new();
    let retained = RETAINED.get_or_init(Default::default);
    // Preserve the exact supplied path as the key, rather than normalizing aliases.
    let key = root.as_os_str().to_owned();
    let pending = {
        let mut cache = retained.lock().map_err(|_| IvyError::internal())?;
        cache
            .entry(key.clone())
            .or_insert_with(|| Arc::new(load_catalogs(root.to_owned()).boxed().shared()))
            .clone()
    };
    let result = pending.as_ref().clone().await;
    if result.is_err() {
        let mut cache = retained.lock().map_err(|_| IvyError::internal())?;
        if cache
            .get(&key)
            .is_some_and(|current| Arc::ptr_eq(current, &pending))
        {
            cache.remove(&key);
        }
    }
    result
}
fn options() -> RequestOptions {
    RequestOptions {
        timeout: Duration::from_secs(5),
        ..Default::default()
    }
}

/// Only ready, connected, synchronized, enabled, non-stale native owners participate.
/// Retains immutable candidate identities, never owner status or selected bindings.
pub async fn verify_native_catalog_compatibility(
    client: Arc<dyn RpcClient>,
    artifact_root: &Path,
    component_id: &JsString,
) -> IvyResult<()> {
    let catalogs = candidate_catalogs(artifact_root).await?;
    let mut cursor: Option<JsString> = None;
    let mut visited = HashSet::new();
    loop {
        let mut params = IvyJson::object([
            ("serviceName", "agent-manager".into()),
            ("limit", IvyJson::Number(200.0)),
        ]);
        if let Some(cursor) = &cursor {
            params.insert("cursor", IvyJson::String(cursor.clone()));
        }
        let page = client
            .request("serviceNodes.list", params, options())
            .await?;
        let nodes = page
            .required("items")?
            .as_array()
            .ok_or_else(IvyError::internal)?
            .iter()
            .filter(|node| {
                ["connected", "synced", "ready", "desiredEnabled"]
                    .iter()
                    .all(|key| node.get(key).and_then(IvyJson::as_bool) == Some(true))
                    && node.get("stale").and_then(IvyJson::as_bool) != Some(true)
            })
            .cloned()
            .collect();
        map_concurrent(nodes, 8, |node, _| {
            let client = client.clone(); let catalogs = catalogs.clone();
            async move {
                let id = field(&node, "serviceNodeId")?;
                let tools = BoundToolClient::new(client, id.clone(), &[InterfaceRequirement { namespace: "agent".into(), interface_version: "1.0.0".into() }], None)?;
                let status = tools.read(&"agent.status".into(), IvyJson::object(Vec::<(String,IvyJson)>::new()), ToolCallOptions { request: options(), ..Default::default() }).await?;
                validate_agent("Status", &status)?;
                if status.get("serviceNodeId") != node.get("serviceNodeId") || status.get("hostId") != node.get("hostId") || field(&status, "state")? != JsString::from("ready") {
                    return Err(IvyError::new_js("compatibility_unknown".into(), message(&["Native owner ".into(), id.clone(), " changed during the ".into(), component_id.clone(), " deployment check. Retry with its current identity.".into()])));
                }
                let hash = field(&status, "catalogHash")?;
                let version = field(&status, "nativeVersion")?;
                let executable = field(&status, "nativeExecutableHash")?;
                if !catalogs.get(&hash).is_some_and(|selected| selected.version == version && selected.executable == executable) {
                    return Err(IvyError::new_js("incompatible_native_catalog".into(), message(&[component_id.clone(), " cannot consume the running catalog of ".into(), id, ": native version ".into(), version, ", catalog ".into(), hash, ", executable ".into(), executable, ". Deploy a matching native owner and configuration before retrying this candidate.".into()])));
                }
                Ok(())
            }
        }).await?;
        cursor = page
            .get("nextCursor")
            .and_then(IvyJson::as_js_string)
            .filter(|s| !s.0.is_empty())
            .cloned();
        let Some(next) = &cursor else {
            return Ok(());
        };
        if !visited.insert(next.clone()) {
            return Err(IvyError::new(
                "compatibility_unknown",
                "Native owner discovery returned a repeated cursor.",
            ));
        }
    }
}
