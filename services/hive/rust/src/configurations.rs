//! Retained desired host configuration and its secret-preserving editor.
use crate::{
    objects::{self, Location},
    retention::Runtime,
    store::{AuthenticatedContext, Store},
};
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString,
    core_schema::validate_host,
    hash::digest,
    json::{canonical, object_entries, parse, try_js_string},
    limits::MANAGEMENT_FRAME_BYTES,
    schema::pointer_parts,
};
use rusqlite::params;
use std::{collections::HashMap, sync::OnceLock};

pub const CONTRACT_KEY: &str = "ivy/host-configuration";
fn contract() -> &'static IvyJson {
    static VALUE: OnceLock<IvyJson> = OnceLock::new();
    VALUE.get_or_init(||parse(r#"{"key":"ivy/host-configuration","version":"1.0.0","owner":{"kind":"hive"},"mediaType":"application/json","retention":{"objects":{"mode":"retain"},"revisions":{"mode":"all"}},"specMarkdown":"Complete desired HostConfig JSON. Hive retains every accepted revision; the owning HostExecutor converges locally.","jsonSchema":{"type":"object","properties":{"schemaVersion":{"const":1},"hostId":{"type":"string","minLength":1,"maxLength":256}},"required":["schemaVersion","hostId"]}}"#,usize::MAX).expect("host configuration contract"))
}
fn empty() -> IvyJson {
    IvyJson::object(std::iter::empty::<(&str, IvyJson)>())
}
fn field<'a>(value: &'a IvyJson, key: &str) -> IvyResult<&'a IvyJson> {
    value.get(key).ok_or_else(IvyError::internal)
}
fn string<'a>(value: &'a IvyJson, key: &str) -> IvyResult<&'a JsString> {
    field(value, key)?
        .as_js_string()
        .ok_or_else(IvyError::internal)
}
fn array<'a>(value: &'a IvyJson, key: &str) -> IvyResult<&'a [IvyJson]> {
    field(value, key)?.as_array().ok_or_else(IvyError::internal)
}
fn text(value: &JsString) -> String {
    String::from_utf16_lossy(&value.0)
}
fn require(condition: bool, code: &str, message: &str) -> IvyResult<()> {
    if condition {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn clone_json(value: &IvyJson) -> IvyResult<IvyJson> {
    parse(
        &canonical(value, MANAGEMENT_FRAME_BYTES)?,
        MANAGEMENT_FRAME_BYTES,
    )
}
fn paths(instance: &IvyJson) -> IvyResult<Vec<JsString>> {
    let mut result = vec!["/credential".into()];
    if let Some(value) = instance.get("secretPaths").filter(|v| !v.is_null()) {
        for path in value.as_array().ok_or_else(IvyError::internal)? {
            result.push(
                path.as_js_string()
                    .cloned()
                    .ok_or_else(IvyError::internal)?,
            );
        }
    }
    Ok(result)
}
fn pointer<'a>(value: &'a IvyJson, path: &JsString) -> IvyResult<Option<&'a IvyJson>> {
    let mut current = value;
    for part in pointer_parts(path)? {
        if !matches!(current, IvyJson::Object(_)) {
            return Ok(None);
        }
        let Some(value) = current.get_key(&part) else {
            return Ok(None);
        };
        current = value;
    }
    Ok(Some(current))
}
fn set_pointer(value: &mut IvyJson, path: &JsString, replacement: IvyJson) -> IvyResult<()> {
    let parts = pointer_parts(path)?;
    let mut current = value;
    for part in &parts[..parts.len() - 1] {
        let IvyJson::Object(entries) = current else {
            return Err(IvyError::internal());
        };
        current = &mut entries
            .iter_mut()
            .find(|(key, _)| key == part)
            .ok_or_else(IvyError::internal)?
            .1;
    }
    if !matches!(current, IvyJson::Object(_)) {
        return Err(IvyError::internal());
    }
    current.insert(
        parts.last().ok_or_else(IvyError::internal)?.clone(),
        replacement,
    );
    Ok(())
}
fn marker(value: &IvyJson) -> Option<(&JsString, &JsString)> {
    let fields = value.as_object()?;
    if fields.len() != 1 {
        return None;
    }
    let identity = value.get("$ivySecret")?;
    if identity.as_object()?.len() != 2 {
        return None;
    }
    Some((
        identity.get("instanceId")?.as_js_string()?,
        identity.get("path")?.as_js_string()?,
    ))
}
struct Marker {
    path: JsString,
    instance_id: JsString,
    declared_path: JsString,
}
fn child_path(path: &JsString, key: &JsString) -> JsString {
    let mut out = path.0.clone();
    out.push(47);
    for unit in &key.0 {
        match unit {
            126 => out.extend([126, 48]),
            47 => out.extend([126, 49]),
            v => out.push(*v),
        }
    }
    JsString(out)
}
fn markers(value: &IvyJson, path: JsString, result: &mut Vec<Marker>) -> IvyResult<()> {
    if let Some((instance_id, declared_path)) = marker(value) {
        result.push(Marker {
            path,
            instance_id: instance_id.clone(),
            declared_path: declared_path.clone(),
        });
        return Ok(());
    }
    require(
        !matches!(value, IvyJson::Object(_)) || value.get("$ivySecret").is_none(),
        "invalid_arguments",
        "A configured-secret marker is malformed.",
    )?;
    if let Some(values) = value.as_array() {
        for (index, value) in values.iter().enumerate() {
            markers(value, child_path(&path, &index.to_string().into()), result)?;
        }
    } else if matches!(value, IvyJson::Object(_)) {
        for (key, value) in object_entries(value) {
            markers(value, child_path(&path, key), result)?;
        }
    }
    Ok(())
}
fn redact(configuration: &IvyJson) -> IvyResult<IvyJson> {
    let mut projected = clone_json(configuration)?;
    let instances = instances_mut(&mut projected)?;
    for instance in instances {
        for path in paths(instance)? {
            if pointer(instance, &path)?.is_some() {
                let identity = IvyJson::object([
                    ("instanceId", field(instance, "instanceId")?.clone()),
                    ("path", IvyJson::String(path.clone())),
                ]);
                set_pointer(instance, &path, IvyJson::object([("$ivySecret", identity)]))?;
            }
        }
    }
    Ok(projected)
}
fn instances_mut(value: &mut IvyJson) -> IvyResult<&mut Vec<IvyJson>> {
    let IvyJson::Object(entries) = value else {
        return Err(IvyError::internal());
    };
    let value = &mut entries
        .iter_mut()
        .find(|(key, _)| key == &JsString::from("instances"))
        .ok_or_else(IvyError::internal)?
        .1;
    let IvyJson::Array(instances) = value else {
        return Err(IvyError::internal());
    };
    Ok(instances)
}
fn restore(value: &IvyJson, current: Option<&IvyJson>) -> IvyResult<IvyJson> {
    require(
        matches!(value, IvyJson::Object(_)),
        "invalid_arguments",
        "Host configuration editor requires an object.",
    )?;
    let mut submitted = clone_json(value)?;
    let instances = submitted.get("instances").and_then(IvyJson::as_array);
    require(
        instances.is_some(),
        "invalid_arguments",
        "Host configuration editor requires an instance array.",
    )?;
    let instances = instances.unwrap();
    require(
        instances.iter().all(|v| matches!(v, IvyJson::Object(_))),
        "invalid_arguments",
        "Host configuration editor instances must be objects.",
    )?;
    for instance in instances {
        require(
            instance
                .get("instanceId")
                .and_then(IvyJson::as_js_string)
                .is_some_and(|v| !v.0.is_empty()),
            "invalid_arguments",
            "Host configuration editor instances require an identity.",
        )?;
        require(
            instance.get("secretPaths").is_none_or(|v| {
                v.as_array()
                    .is_some_and(|v| v.iter().all(|v| v.as_js_string().is_some()))
            }),
            "invalid_arguments",
            "Host configuration editor secretPaths must be string pointers.",
        )?;
        if let Some(paths) = instance.get("secretPaths").and_then(IvyJson::as_array) {
            for path in paths {
                pointer_parts(path.as_js_string().unwrap())?;
            }
        }
    }
    let mut retained = HashMap::new();
    if let Some(current) = current {
        for instance in array(current, "instances")? {
            retained.insert(string(instance, "instanceId")?.clone(), instance);
        }
    }
    let instances = instances_mut(&mut submitted)?;
    for instance in instances {
        let id = string(instance, "instanceId")?.clone();
        let previous = retained.get(&id).copied();
        let allowed = previous.map(paths).transpose()?.unwrap_or_default();
        let submitted_paths = paths(instance)?;
        let mut found = vec![];
        markers(instance, JsString::default(), &mut found)?;
        for found in found {
            require(
                previous.is_some()
                    && !found.path.0.is_empty()
                    && allowed.contains(&found.path)
                    && submitted_paths.contains(&found.path)
                    && found.instance_id == id
                    && found.declared_path == found.path,
                "invalid_arguments",
                "A configured-secret marker must retain its original instance and declared setting path.",
            )?;
            let selected = pointer(previous.unwrap(), &found.path)?;
            require(
                selected.is_some(),
                "invalid_arguments",
                "A configured-secret marker needs an existing value at its declared setting path.",
            )?;
            set_pointer(instance, &found.path, selected.unwrap().clone())?;
        }
    }
    Ok(submitted)
}

pub struct Configurations;
impl Configurations {
    pub fn new(store: &mut Store) -> IvyResult<Self> {
        objects::register(store, contract(), field(contract(), "owner")?)?;
        Ok(Self)
    }
    pub fn is_configuration_contract(key: &JsString) -> bool {
        key == &JsString::from(CONTRACT_KEY)
    }
    fn object_id(&self, store: &Store, host: &JsString) -> IvyResult<Option<JsString>> {
        let mut statement=store.connection().prepare_cached("SELECT o.id FROM objects o JOIN revisions r ON r.object_id=o.id AND r.revision=o.current_revision WHERE o.contract_key=? AND json_extract(r.content,'$.hostId')=? ORDER BY o.id LIMIT 2").map_err(|e|store.query_error(e))?;
        let rows = statement
            .query_map(params![CONTRACT_KEY, text(host)], |row| {
                row.get::<_, String>(0)
            })
            .map_err(|e| store.query_error(e))?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| store.query_error(e))?;
        require(
            rows.len() <= 1,
            "storage_invalid",
            "Hive retained more than one current configuration for a host.",
        )?;
        Ok(rows.first().cloned().map(JsString::from))
    }
    fn configuration(&self, value: &IvyJson, host: &JsString) -> IvyResult<IvyJson> {
        validate_host("HostConfig", value)?;
        require(
            value.get("hostId").and_then(IvyJson::as_js_string) == Some(host),
            "target_conflict",
            "Host configuration belongs to another host.",
        )?;
        let instances = array(value, "instances")?;
        let unique = |key: &str| -> IvyResult<bool> {
            let mut found = vec![];
            for instance in instances {
                let identity = string(instance, key)?;
                if found.contains(&identity) {
                    return Ok(false);
                }
                found.push(identity);
            }
            Ok(true)
        };
        require(
            unique("instanceId")? && unique("serviceNodeId")?,
            "invalid_arguments",
            "Host instance and Service Node identities must be unique.",
        )?;
        for instance in instances {
            let paths = instance
                .get("secretPaths")
                .and_then(IvyJson::as_array)
                .unwrap_or(&[]);
            let mut seen = vec![];
            let mut valid = true;
            for path in paths {
                let path = path.as_js_string().ok_or_else(IvyError::internal)?;
                if seen.contains(&path) {
                    valid = false;
                    break;
                }
                seen.push(path);
                if !path
                    .0
                    .starts_with(&"/settings/".encode_utf16().collect::<Vec<_>>())
                {
                    valid = false;
                    break;
                }
                if pointer_parts(path)?.iter().any(|part| {
                    part.0 == [48]
                        || part.0.first().is_some_and(|v| (49..=57).contains(v))
                            && part.0.iter().all(|v| (48..=57).contains(v))
                }) {
                    valid = false;
                    break;
                }
            }
            require(
                valid,
                "invalid_arguments",
                "Additional secret paths must be unique setting paths; protect arrays as a whole instead of addressing their elements.",
            )?;
        }
        Ok(value.clone())
    }
    pub fn get(&self, store: &Store, host: &JsString, revision: Option<u64>) -> IvyResult<IvyJson> {
        let id = self
            .object_id(store, host)?
            .ok_or_else(|| IvyError::new("not_found", "Host configuration not found."))?;
        let read = objects::read(store, Location::Id(&id), revision)?;
        let content = field(&read, "content")?;
        require(
            content.get("encoding") == Some(&"json".into()),
            "storage_invalid",
            "Host configuration content is not JSON.",
        )?;
        let configuration = self.configuration(field(content, "value")?, host)?;
        let revision = field(&read, "revision")?;
        Ok(IvyJson::object([
            ("hostId", IvyJson::String(host.clone())),
            ("objectId", IvyJson::String(id)),
            ("revision", field(revision, "revision")?.clone()),
            ("contentHash", field(revision, "contentHash")?.clone()),
            ("updatedAt", field(revision, "createdAt")?.clone()),
            ("byteLength", field(revision, "byteLength")?.clone()),
            ("configuration", configuration),
        ]))
    }
    pub fn list(&self, store: &Store, runtime: &dyn Runtime) -> IvyResult<Vec<IvyJson>> {
        let ids = store
            .connection()
            .prepare_cached(
                "SELECT id FROM objects WHERE contract_key=? AND effective_archive=0 ORDER BY id",
            )
            .map_err(|e| store.query_error(e))?
            .query_map([CONTRACT_KEY], |row| row.get::<_, String>(0))
            .map_err(|e| store.query_error(e))?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| store.query_error(e))?;
        let mut result = vec![];
        for id in ids {
            let read = objects::read(store, Location::Id(&id.into()), None)?;
            let content = field(&read, "content")?;
            require(
                content.get("encoding") == Some(&"json".into()),
                "storage_invalid",
                "Host configuration content is not JSON.",
            )?;
            let value = field(content, "value")?;
            if value.is_null() {
                return Err(IvyError::internal());
            }
            let host = value
                .get("hostId")
                .map(try_js_string)
                .transpose()?
                .unwrap_or_else(|| "undefined".into());
            result.push(self.get(store, &host, None)?);
        }
        result.sort_by(|a, b| {
            runtime.compare_contract_keys(
                a.get("hostId").unwrap().as_js_string().unwrap(),
                b.get("hostId").unwrap().as_js_string().unwrap(),
            )
        });
        for value in &mut result {
            value.remove("configuration");
        }
        Ok(result)
    }
    fn prepare_write(&self, store: &Store, params: &IvyJson) -> IvyResult<IvyJson> {
        let host = string(params, "hostId")?;
        let configuration = self.configuration(field(params, "configuration")?, host)?;
        let object = self.object_id(store, host)?;
        require(
            if object.is_some() {
                params.get("expectedRevision").is_some()
            } else {
                params.get("expectedRevision").is_none()
            },
            "revision_conflict",
            if object.is_some() {
                "Updating a host configuration requires its current revision."
            } else {
                "A new host configuration cannot name an existing revision."
            },
        )?;
        let mut value = IvyJson::object([
            ("mutationId", field(params, "mutationId")?.clone()),
            ("contractVersion", "1.0.0".into()),
            ("references", empty()),
            (
                "content",
                IvyJson::object([
                    ("encoding", "json".into()),
                    ("value", clone_json(&configuration)?),
                ]),
            ),
        ]);
        if let Some(id) = object {
            value.insert("objectId", IvyJson::String(id));
            value.insert(
                "expectedRevision",
                field(params, "expectedRevision")?.clone(),
            );
        } else {
            value.insert(
                "create",
                IvyJson::object([
                    ("contractKey", CONTRACT_KEY.into()),
                    ("parentId", IvyJson::Null),
                    ("ownerObjectId", IvyJson::Null),
                    (
                        "name",
                        format!("Ivy host configuration {}", &digest(&text(host))[7..23]).into(),
                    ),
                ]),
            );
        }
        Ok(value)
    }
    pub fn put(
        &self,
        store: &mut Store,
        context: &AuthenticatedContext,
        params: &IvyJson,
        now_ms: u64,
        now: &JsString,
    ) -> IvyResult<IvyJson> {
        let saved = objects::write_hive_prepared(
            store,
            context,
            &"hostConfigurations.put".into(),
            params,
            now_ms,
            now,
            |store| self.prepare_write(store, params),
        )?;
        self.get(
            store,
            string(params, "hostId")?,
            field(field(&saved, "revision")?, "revision")?
                .as_f64()
                .map(|v| v as u64),
        )
    }
    pub fn edit(
        &self,
        store: &Store,
        host: &JsString,
        revision: Option<u64>,
    ) -> IvyResult<IvyJson> {
        let mut value = self.get(store, host, revision)?;
        let projected = redact(field(&value, "configuration")?)?;
        value.insert("configuration", projected);
        Ok(value)
    }
    pub fn project_read(&self, read: &IvyJson) -> IvyResult<IvyJson> {
        if field(&read, "object")?.get("contractKey") != Some(&CONTRACT_KEY.into())
            || field(read, "content")?.get("encoding") != Some(&"json".into())
        {
            return Ok(read.clone());
        }
        let value = field(field(read, "content")?, "value")?;
        if value.is_null() {
            return Err(IvyError::internal());
        }
        let host = value
            .get("hostId")
            .map(try_js_string)
            .transpose()?
            .unwrap_or_else(|| "undefined".into());
        let projected = redact(&self.configuration(value, &host)?)?;
        let mut result = read.clone();
        result.insert(
            "content",
            IvyJson::object([("encoding", "json".into()), ("value", projected)]),
        );
        Ok(result)
    }
    pub fn save(
        &self,
        store: &mut Store,
        context: &AuthenticatedContext,
        params: &IvyJson,
        now_ms: u64,
        now: &JsString,
    ) -> IvyResult<IvyJson> {
        let saved = objects::write_hive_prepared(
            store,
            context,
            &"hostConfigurations.save".into(),
            params,
            now_ms,
            now,
            |store| {
                let host = string(params, "hostId")?;
                let current = if self.object_id(store, host)?.is_some() {
                    Some(field(&self.get(store, host, None)?, "configuration")?.clone())
                } else {
                    None
                };
                let value = restore(field(params, "configuration")?, current.as_ref())?;
                let mut prepared = params.clone();
                prepared.insert("configuration", value);
                self.prepare_write(store, &prepared)
            },
        )?;
        self.edit(
            store,
            string(params, "hostId")?,
            field(field(&saved, "revision")?, "revision")?
                .as_f64()
                .map(|v| v as u64),
        )
    }
    pub fn history(&self, store: &Store, params: &IvyJson) -> IvyResult<IvyJson> {
        let id = self
            .object_id(store, string(params, "hostId")?)?
            .ok_or_else(|| IvyError::new("not_found", "Host configuration not found."))?;
        let mut query = IvyJson::object([("objectId", IvyJson::String(id))]);
        for key in ["limit", "cursor"] {
            if let Some(v) = params.get(key) {
                query.insert(key, v.clone());
            }
        }
        let mut result = objects::history(store, &query)?;
        if result.get("historyComplete").is_none_or(IvyJson::is_null) {
            result.insert("historyComplete", false.into());
        }
        Ok(result)
    }
}
