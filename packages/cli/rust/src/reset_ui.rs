//! UI discovery and one publication attempt for the host reset coordinator.
//! The caller retains reset authority, contract bootstrap and bounded retries.
use crate::publish_ui::{PublishUiInput, UiBundle, publish_ui, read_ui_bundle};
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString, json::parse, operation::derive_operation_id_js,
};
use ivy_sdk::{
    clock::Clock,
    collation::LocaleComparator,
    rpc::{ClientOptions, HiveClient, RequestOptions, RpcClient},
};
use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug)]
pub struct ResetUiBundle {
    pub directory: PathBuf,
    pub definition: IvyJson,
    pub checked_bundle: UiBundle,
}

/// Select from already validated instance definitions without applying the
/// coordinator's later missing-credential gate or observing another instance.
pub fn reset_ui_credential(instances: &[IvyJson]) -> IvyResult<Option<JsString>> {
    for instance in instances {
        if instance.is_null() {
            return Err(IvyError::internal());
        }
        if let Some(value) = instance.get("credential").filter(|value| truthy(value)) {
            return value
                .as_js_string()
                .cloned()
                .map(Some)
                .ok_or_else(IvyError::internal);
        }
    }
    Ok(None)
}

/// One restore invocation's authenticated client and retained identity.
/// The host retains credential authority, its separate deadline and retries.
pub struct ResetUiPublisher {
    client: HiveClient,
    mutation_id: JsString,
}
impl ResetUiPublisher {
    pub fn new(
        public_base_url: &JsString,
        credential: &JsString,
        runtime_epoch: &JsString,
        reset_id: &JsString,
        clock: &dyn Clock,
    ) -> IvyResult<Self> {
        let client = HiveClient::new_js(
            public_base_url,
            ClientOptions {
                credential: Some(credential.clone()),
                ..ClientOptions::default()
            },
        )?;
        let issued_at = clock.utc_millis();
        let mut nonce = JsString::from("runtime-reset-uis-");
        nonce.0.extend_from_slice(&reset_id.0);
        let issued_at = u64::try_from(issued_at).map_err(|_| {
            IvyError::new("invalid_arguments", "Invalid operation identity fields.")
        })?;
        let mutation_id =
            ivy_contracts::operation::operation_id_js(runtime_epoch, issued_at, &nonce)?;
        Ok(Self {
            client,
            mutation_id,
        })
    }
    pub fn mutation_id(&self) -> &JsString {
        &self.mutation_id
    }
    pub async fn republish(&self, bundles: &[ResetUiBundle]) -> IvyResult<Vec<JsString>> {
        republish_reset_uis(&self.client, bundles, &self.mutation_id).await
    }
}

fn io<T>(value: std::io::Result<T>) -> IvyResult<T> {
    value.map_err(|_| IvyError::internal())
}

fn ui_error(code: &str, prefix: &str, ui_id: &JsString, suffix: &str) -> IvyError {
    let mut message = JsString::from(prefix);
    message.0.extend_from_slice(&ui_id.0);
    message.0.extend(suffix.encode_utf16());
    IvyError::new_js(code.into(), message)
}

fn normalized(path: PathBuf) -> PathBuf {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::{OsStrExt, OsStringExt};
        let value = JsString(path.as_os_str().encode_wide().collect());
        std::ffi::OsString::from_wide(&ivy_contracts::js_path::normalize_win32(&value).0).into()
    }
    #[cfg(not(windows))]
    {
        use std::path::Component;
        let mut result = PathBuf::new();
        for component in path.components() {
            match component {
                Component::CurDir => (),
                Component::ParentDir if result.file_name().is_some_and(|name| name != "..") => {
                    result.pop();
                }
                Component::ParentDir if result.has_root() => (),
                other => result.push(other),
            }
        }
        if result.as_os_str().is_empty() {
            result.push(".");
        }
        result
    }
}

fn file_name(value: &std::ffi::OsStr) -> JsString {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        JsString(value.encode_wide().collect())
    }
    #[cfg(not(windows))]
    {
        value.to_string_lossy().as_ref().into()
    }
}

fn scan(distribution: PathBuf, ui_ids: Vec<JsString>) -> IvyResult<Vec<ResetUiBundle>> {
    let root = normalized(distribution.join("dist").join("uis"));
    let entries = io(fs::read_dir(&root))?
        .map(|entry| {
            let entry = io(entry)?;
            Ok((
                file_name(&entry.file_name()),
                entry.file_name(),
                io(entry.file_type())?,
            ))
        })
        .collect::<IvyResult<Vec<_>>>()?;
    let mut bundles = Vec::new();
    for ui_id in ui_ids {
        let entry = entries.iter().find(|(name, _, _)| name == &ui_id);
        let Some((_, name, _)) = entry.filter(|(_, _, kind)| kind.is_dir()) else {
            return Err(ui_error(
                "not_found",
                "Configured ui bundle ",
                &ui_id,
                " is missing.",
            ));
        };
        let directory = root.join(name);
        let definition_path = directory.join("ivy-ui.json");
        match fs::symlink_metadata(&definition_path) {
            Ok(_) => (),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(ui_error(
                    "not_found",
                    "Configured ui definition ",
                    &ui_id,
                    " is missing.",
                ));
            }
            Err(_) => return Err(IvyError::internal()),
        }
        let bytes = io(fs::read(definition_path))?;
        let definition = parse(&String::from_utf8_lossy(&bytes), usize::MAX)
            .map_err(|_| IvyError::internal())?;
        if definition.is_null() {
            return Err(IvyError::internal());
        }
        let matches = definition
            .get("metadata")
            .and_then(|value| value.get("uiId"))
            == Some(&IvyJson::String(ui_id.clone()));
        let owned = if matches {
            let contracts = definition
                .get("dataContracts")
                .and_then(IvyJson::as_array)
                .ok_or_else(IvyError::internal)?;
            let mut owned = true;
            for contract in contracts {
                let owner = contract
                    .get("owner")
                    .filter(|value| !value.is_null())
                    .ok_or_else(IvyError::internal)?;
                if owner.get("kind") != Some(&"agent".into()) {
                    owned = false;
                    break;
                }
            }
            owned
        } else {
            false
        };
        if !matches || !owned {
            return Err(ui_error(
                "invalid_arguments",
                "Configured ui bundle ",
                &ui_id,
                " cannot be published by the reset coordinator.",
            ));
        }
        let checked_bundle = read_ui_bundle(&directory, &definition)?;
        bundles.push(ResetUiBundle {
            directory,
            definition,
            checked_bundle,
        });
    }
    Ok(bundles)
}

/// Validate every configured UI in caller order before reset effects begin.
pub async fn reset_ui_bundles(
    distribution: &Path,
    ui_ids: &[JsString],
) -> IvyResult<Vec<ResetUiBundle>> {
    if ui_ids.is_empty()
        || ui_ids.len() > 32
        || ui_ids.iter().collect::<HashSet<_>>().len() != ui_ids.len()
    {
        return Err(IvyError::new(
            "invalid_arguments",
            "Runtime reset needs a bounded unique configured ui list.",
        ));
    }
    let (distribution, ui_ids) = (distribution.to_owned(), ui_ids.to_owned());
    tokio::task::spawn_blocking(move || scan(distribution, ui_ids))
        .await
        .map_err(|_| IvyError::internal())?
}

/// Select required service contracts from the exact owner-supplied catalog.
/// The caller supplies the original builder order and executing locale; this
/// function does not discover, widen or replace the bootstrap authority.
pub fn reset_bootstrap_contracts(
    bundles: &[ResetUiBundle],
    service_contracts: &[IvyJson],
    compare: &LocaleComparator,
) -> IvyResult<Vec<IvyJson>> {
    let internal = IvyError::internal;
    let mut available = Vec::new();
    for bundle in bundles {
        available.extend(
            bundle
                .definition
                .get("dataContracts")
                .and_then(IvyJson::as_array)
                .ok_or_else(internal)?,
        );
    }
    available.extend(service_contracts);
    let mut positions: HashMap<JsString, usize> = HashMap::new();
    let mut selected: Vec<(JsString, IvyJson)> = Vec::new();
    for bundle in bundles {
        let required = bundle
            .definition
            .get("requirements")
            .and_then(|value| value.get("contracts"))
            .and_then(IvyJson::as_array)
            .ok_or_else(internal)?;
        for required in required {
            let key = required
                .get("key")
                .and_then(IvyJson::as_js_string)
                .ok_or_else(internal)?;
            let read = required
                .get("readVersions")
                .and_then(IvyJson::as_array)
                .ok_or_else(internal)?;
            let write = required
                .get("writeVersions")
                .and_then(IvyJson::as_array)
                .ok_or_else(internal)?;
            for version in read.iter().chain(write) {
                let found = available.iter().find(|value| {
                    value.get("key") == Some(&IvyJson::String(key.clone()))
                        && value.get("version") == Some(version)
                });
                let Some(definition) = found else {
                    let ui_id = bundle
                        .definition
                        .get("metadata")
                        .and_then(|value| value.get("uiId"))
                        .and_then(IvyJson::as_js_string)
                        .ok_or_else(internal)?;
                    let version = version.as_js_string().ok_or_else(internal)?;
                    let mut message = JsString::from("Reset ui ");
                    message.0.extend_from_slice(&ui_id.0);
                    message
                        .0
                        .extend(" requires unavailable contract ".encode_utf16());
                    message.0.extend_from_slice(&key.0);
                    message.0.push(u16::from(b'@'));
                    message.0.extend_from_slice(&version.0);
                    message.0.push(u16::from(b'.'));
                    return Err(IvyError::new_js("contract_not_found".into(), message));
                };
                let owner = definition
                    .get("owner")
                    .filter(|value| !value.is_null())
                    .ok_or_else(internal)?;
                if owner.get("kind") == Some(&"service".into()) {
                    let mut identity = key.clone();
                    identity.0.push(u16::from(b'@'));
                    identity
                        .0
                        .extend_from_slice(&version.as_js_string().ok_or_else(internal)?.0);
                    if let Some(index) = positions.get(&identity) {
                        selected[*index].1 = (**definition).clone();
                    } else {
                        positions.insert(identity.clone(), selected.len());
                        selected.push((identity, (**definition).clone()));
                    }
                }
            }
        }
    }
    selected.sort_by(|left, right| compare.compare(&left.0, &right.0));
    Ok(selected
        .into_iter()
        .map(|(_, definition)| definition)
        .collect())
}

fn truthy(value: &IvyJson) -> bool {
    match value {
        IvyJson::Null => false,
        IvyJson::Bool(value) => *value,
        IvyJson::Number(value) => *value != 0.0 && !value.is_nan(),
        IvyJson::String(value) => !value.0.is_empty(),
        _ => true,
    }
}

/// Inspect, publish when absent, then verify readiness. Never retry or replay here.
pub async fn republish_reset_uis(
    client: &dyn RpcClient,
    bundles: &[ResetUiBundle],
    base_mutation_id: &JsString,
) -> IvyResult<Vec<JsString>> {
    let mut published = Vec::new();
    for bundle in bundles {
        let ui_id = bundle
            .definition
            .get("metadata")
            .and_then(|value| value.get("uiId"))
            .and_then(IvyJson::as_js_string)
            .ok_or_else(IvyError::internal)?;
        let params = IvyJson::object([("uiId", IvyJson::String(ui_id.clone()))]);
        let current = match client
            .request("uis.inspect", params.clone(), RequestOptions::default())
            .await
        {
            Ok(value) => value,
            Err(error) if error.code == "not_found" => IvyJson::Null,
            Err(error) => return Err(error),
        };
        if truthy(&current) {
            if current.get("currentReleaseId")
                != Some(&IvyJson::from(bundle.checked_bundle.release_id.as_str()))
            {
                return Err(ui_error(
                    "mutation_conflict",
                    "Reset ui ",
                    ui_id,
                    " has another active release.",
                ));
            }
            if current.get("status") != Some(&"ready".into()) {
                return Err(ui_error(
                    "service_not_ready",
                    "Republished ui ",
                    ui_id,
                    " is not ready.",
                ));
            }
        } else {
            let mutation_id = derive_operation_id_js(
                base_mutation_id,
                &IvyJson::Array(vec!["ui".into(), IvyJson::String(ui_id.clone())]),
            )?;
            publish_ui(
                client,
                PublishUiInput {
                    directory: &bundle.directory,
                    definition: &bundle.definition,
                    expected_release_id: None,
                    mutation_id: &mutation_id,
                    checked_bundle: Some(&bundle.checked_bundle),
                },
            )
            .await?;
            let inspection = client
                .request("uis.inspect", params, RequestOptions::default())
                .await?;
            if inspection.is_null() {
                return Err(IvyError::internal());
            }
            if inspection.get("status") != Some(&"ready".into()) {
                return Err(ui_error(
                    "service_not_ready",
                    "Republished ui ",
                    ui_id,
                    " is not ready.",
                ));
            }
        }
        published.push(ui_id.clone());
    }
    Ok(published)
}
