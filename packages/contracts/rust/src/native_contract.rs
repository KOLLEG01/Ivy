//! Retained catalog identity, schema projection and exact argument/result
//! validation. Invocation authority and provider selection belong to callers.
use crate::{
    IvyError, IvyJson, IvyResult, JsString,
    agent_schema::validate_agent,
    core_schema::validate_shared,
    hash::{hash_json, tool_definition_hash},
    json::{canonical, js_string},
    limits::MANAGEMENT_FRAME_BYTES,
    schema::SchemaValidators,
};
use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex, OnceLock},
};

pub fn is_read_only_native_method(method: &JsString) -> bool {
    matches!(
        method.to_utf8().ok().as_deref(),
        Some(
            "account/rateLimits/read"
                | "collaborationMode/list"
                | "config/read"
                | "fs/readFile"
                | "model/list"
                | "permissionProfile/list"
                | "project/list"
                | "server/diagnostics"
                | "thread/items/list"
                | "thread/list"
                | "thread/loaded/list"
                | "thread/goal/get"
                | "thread/read"
                | "thread/turns/list"
                | "userVerification/status"
        )
    )
}
pub fn native_tool_definition(
    catalog: &IvyJson,
    method: &IvyJson,
    catalog_hash: Option<&JsString>,
) -> IvyResult<IvyJson> {
    let hash = catalog_hash
        .cloned()
        .map(Ok)
        .unwrap_or_else(|| hash_json(catalog).map(JsString::from))?;
    if catalog.is_null() || method.is_null() {
        return Err(IvyError::internal());
    }
    let read_only = method
        .get("method")
        .and_then(IvyJson::as_js_string)
        .is_some_and(is_read_only_native_method);
    let mut description = JsString::from("Installed public Codex ");
    description.0.extend(
        catalog
            .get("version")
            .map(js_string)
            .unwrap_or_else(|| "undefined".into())
            .0,
    );
    description.0.extend(" method ".encode_utf16());
    description.0.extend(
        method
            .get("method")
            .map(js_string)
            .unwrap_or_else(|| "undefined".into())
            .0,
    );
    description
        .0
        .extend(". Supply its unchanged native arguments.".encode_utf16());
    description.0.extend(
        if read_only {
            " This read-only call does not require an operationId."
        } else {
            " Supply an outer stable operationId."
        }
        .encode_utf16(),
    );
    let mut value = IvyJson::object([("namespace", "codex".into())]);
    if let Some(name) = method.get("method") {
        value.insert("name", name.clone());
    }
    if let Some(version) = catalog.get("version") {
        value.insert("interfaceVersion", version.clone());
    }
    value.insert("description", IvyJson::String(description));
    for key in ["inputSchema", "outputSchema"] {
        if let Some(schema) = method.get(key) {
            value.insert(key, schema.clone());
        }
    }
    if read_only {
        value.insert(
            "annotations",
            IvyJson::object([
                ("readOnlyHint", true.into()),
                ("idempotentHint", true.into()),
            ]),
        );
    }
    if let Some(name) = method.get("method") {
        value.insert("nativeMethod", name.clone());
    }
    value.insert("nativeSchemaIdentity", IvyJson::String(hash));
    Ok(value)
}
struct Definition {
    value: IvyJson,
    hash: OnceLock<String>,
}
pub struct NativeContract {
    catalog: IvyJson,
    catalog_hash: String,
    methods: Vec<(JsString, Definition)>,
    indexes: HashMap<JsString, usize>,
    schemas: Mutex<HashMap<String, Arc<IvyJson>>>,
    validators: SchemaValidators,
}
impl NativeContract {
    pub fn new(catalog: &IvyJson) -> IvyResult<Self> {
        validate_agent("Catalog", catalog)?;
        let catalog = catalog.clone();
        let catalog_hash = hash_json(&catalog)?;
        let identity = JsString::from(catalog_hash.clone());
        let mut methods = Vec::new();
        let mut indexes = HashMap::new();
        for method in catalog
            .get("clientRequests")
            .and_then(IvyJson::as_array)
            .unwrap()
        {
            let name = method
                .get("method")
                .and_then(IvyJson::as_js_string)
                .unwrap()
                .clone();
            if indexes.insert(name.clone(), methods.len()).is_some() {
                return Err(IvyError::new(
                    "native_catalog_invalid",
                    "The selected native catalog contains a duplicate method.",
                ));
            }
            methods.push((
                name,
                Definition {
                    value: native_tool_definition(&catalog, method, Some(&identity))?,
                    hash: OnceLock::new(),
                },
            ));
        }
        Ok(Self {
            catalog,
            catalog_hash,
            methods,
            indexes,
            schemas: Mutex::new(HashMap::new()),
            validators: SchemaValidators::new(),
        })
    }
    pub fn catalog(&self) -> &IvyJson {
        &self.catalog
    }
    pub fn catalog_hash(&self) -> &str {
        &self.catalog_hash
    }
    fn entry(&self, method: &JsString) -> IvyResult<&Definition> {
        self.indexes
            .get(method)
            .map(|i| &self.methods[*i].1)
            .ok_or_else(|| {
                IvyError::new(
                    "native_method_unsupported",
                    "The method is absent from the exact selected native catalog.",
                )
            })
    }
    pub fn definition(&self, method: &JsString) -> IvyResult<&IvyJson> {
        Ok(&self.entry(method)?.value)
    }
    pub fn definitions(&self) -> Vec<(JsString, IvyJson)> {
        self.methods
            .iter()
            .map(|(name, value)| (name.clone(), value.value.clone()))
            .collect()
    }
    pub fn definition_hash(&self, method: &JsString) -> IvyResult<&str> {
        let entry = self.entry(method)?;
        if entry.hash.get().is_none() {
            let hash = tool_definition_hash(&entry.value)?;
            let _ = entry.hash.set(hash);
        }
        Ok(entry.hash.get().unwrap())
    }
    pub fn verify_definition(
        &self,
        method: &JsString,
        definition: &IvyJson,
        hash: &JsString,
    ) -> IvyResult<()> {
        validate_shared("ToolDefinition", definition)?;
        if hash != &JsString::from(tool_definition_hash(definition)?)
            || hash != &JsString::from(self.definition_hash(method)?)
        {
            return Err(IvyError::new(
                "native_definition_changed",
                "The selected provider definition differs from the retained native contract.",
            ));
        }
        Ok(())
    }
    pub fn input_schema(
        &self,
        method: &JsString,
        reserved: &[JsString],
    ) -> IvyResult<Arc<IvyJson>> {
        let mut names = reserved.to_vec();
        names.sort();
        names.dedup();
        let key = canonical(
            &IvyJson::object([
                ("method", IvyJson::String(method.clone())),
                (
                    "reserved",
                    IvyJson::Array(names.iter().cloned().map(IvyJson::String).collect()),
                ),
            ]),
            MANAGEMENT_FRAME_BYTES,
        )?;
        if let Some(value) = self
            .schemas
            .lock()
            .map_err(|_| IvyError::internal())?
            .get(&key)
        {
            return Ok(value.clone());
        }
        let mut input = self.definition(method)?.get("inputSchema").unwrap().clone();
        let root = if let Some(reference) = input.get("$ref") {
            reference_path(
                &input,
                reference,
                "Native arguments must use their exact finite local root definition.",
            )?
        } else {
            Some(vec![])
        };
        let object = match root.as_ref().and_then(|p| at(&input, p)) {
            Some(IvyJson::Null) => return Err(IvyError::internal()),
            Some(value) => object_arguments(value),
            None => false,
        };
        if object {
            let value = at_mut(&mut input, root.as_ref().unwrap()).unwrap();
            let properties = child_mut(value, &"properties".into()).unwrap();
            for name in &names {
                match properties {
                    IvyJson::Object(fields) => fields.retain(|(key, _)| key != name),
                    IvyJson::Array(items) => {
                        if name == &JsString::from("length") {
                            return Err(IvyError::internal());
                        }
                        if let Some(i) = array_index(name) {
                            if let Some(item) = items.get_mut(i) {
                                *item = IvyJson::Null;
                            }
                        }
                    }
                    _ => unreachable!(),
                }
            }
            let required = match value.get("required") {
                None | Some(IvyJson::Null) => vec![],
                Some(IvyJson::Array(items)) => items
                    .iter()
                    .filter(|v| !v.as_js_string().is_some_and(|n| names.contains(n)))
                    .cloned()
                    .collect(),
                _ => return Err(IvyError::internal()),
            };
            value.insert("required", IvyJson::Array(required));
            value.insert("additionalProperties", false.into());
        } else {
            if !names.is_empty() {
                return Err(IvyError::new(
                    "native_template_invalid",
                    "Only native object arguments can reserve workflow fields.",
                ));
            }
            if let Some(root) = root {
                close_alternatives(&mut input, root)?;
            }
        }
        strip(&mut input);
        let value = Arc::new(input);
        let mut cache = self.schemas.lock().map_err(|_| IvyError::internal())?;
        Ok(cache.entry(key).or_insert(value).clone())
    }

    pub fn validate_input(
        &self,
        method: &JsString,
        params: &IvyJson,
        maximum_bytes: usize,
    ) -> IvyResult<()> {
        self.validate_template(method, params, &[], maximum_bytes)
    }
    pub fn validate_template(
        &self,
        method: &JsString,
        params: &IvyJson,
        reserved: &[JsString],
        maximum_bytes: usize,
    ) -> IvyResult<()> {
        let schema = self.input_schema(method, reserved)?;
        self.validators.validate(&schema, params, maximum_bytes)
    }
    pub fn validate_result(
        &self,
        method: &JsString,
        result: &IvyJson,
        maximum_bytes: usize,
    ) -> IvyResult<()> {
        self.validators.validate(
            self.definition(method)?.get("outputSchema").unwrap(),
            result,
            maximum_bytes,
        )
    }
}
type Path = Vec<JsString>;
fn array_index(key: &JsString) -> Option<usize> {
    let s = key.to_utf8().ok()?;
    let i = s.parse::<usize>().ok()?;
    (s == i.to_string()).then_some(i)
}
fn child<'a>(value: &'a IvyJson, key: &JsString) -> Option<&'a IvyJson> {
    match value {
        IvyJson::Array(items) => items.get(array_index(key)?),
        _ => value.get_key(key),
    }
}
fn child_mut<'a>(value: &'a mut IvyJson, key: &JsString) -> Option<&'a mut IvyJson> {
    match value {
        IvyJson::Array(items) => items.get_mut(array_index(key)?),
        IvyJson::Object(fields) => fields.iter_mut().find(|(k, _)| k == key).map(|(_, v)| v),
        _ => None,
    }
}
fn at<'a>(mut value: &'a IvyJson, path: &[JsString]) -> Option<&'a IvyJson> {
    for key in path {
        value = child(value, key)?;
    }
    Some(value)
}
fn at_mut<'a>(mut value: &'a mut IvyJson, path: &[JsString]) -> Option<&'a mut IvyJson> {
    for key in path {
        value = child_mut(value, key)?;
    }
    Some(value)
}
fn reference_path(input: &IvyJson, reference: &IvyJson, message: &str) -> IvyResult<Option<Path>> {
    let invalid = || IvyError::new("native_catalog_invalid", message);
    let text = reference.as_str().map_err(|_| invalid())?;
    let name = text
        .strip_prefix("#/$defs/")
        .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric()))
        .ok_or_else(invalid)?;
    let defs = input
        .get("$defs")
        .filter(|v| !v.is_null())
        .ok_or_else(invalid)?;
    if child(defs, &name.into()).is_some() {
        return Ok(Some(vec!["$defs".into(), name.into()]));
    }
    Err(invalid())
}
fn object_arguments(value: &IvyJson) -> bool {
    value.get("type").and_then(IvyJson::as_js_string) == Some(&"object".into())
        && matches!(
            value.get("properties"),
            Some(IvyJson::Object(_) | IvyJson::Array(_))
        )
}
fn close_alternatives(input: &mut IvyJson, root: Path) -> IvyResult<()> {
    let mut work = vec![(root, false)];
    let mut visited = HashSet::new();
    while let Some((path, finish)) = work.pop() {
        let node = at(input, &path).ok_or_else(IvyError::internal)?;
        if node.is_null() {
            return Err(IvyError::internal());
        }
        if !matches!(node, IvyJson::Object(_) | IvyJson::Array(_)) {
            continue;
        }
        if !finish {
            if !visited.insert(path.clone()) {
                continue;
            }
            let reference = node
                .get("$ref")
                .map(|r| {
                    reference_path(
                        input,
                        r,
                        "Native argument alternatives must use finite local definitions.",
                    )
                })
                .transpose()?
                .flatten();
            work.push((path, true));
            if let Some(reference) = reference {
                work.push((reference, false));
            }
        } else {
            let node = at_mut(input, &path).unwrap();
            if object_arguments(node) {
                node.insert("additionalProperties", false.into());
            }
            for key in ["oneOf", "anyOf"] {
                if let Some(items) = node.get(key).and_then(IvyJson::as_array) {
                    for index in (0..items.len()).rev() {
                        let mut next = path.clone();
                        next.push(key.into());
                        next.push(index.to_string().into());
                        work.push((next, false));
                    }
                }
            }
        }
    }
    Ok(())
}
// Catalog hashing has already bounded ordinary JSON depth before projection.
fn strip(value: &mut IvyJson) {
    if !matches!(value, IvyJson::Object(_) | IvyJson::Array(_)) {
        return;
    }
    for key in ["description", "title", "$id", "$schema"] {
        value.remove(key);
    }
    for key in ["properties", "$defs"] {
        if let Some(child) = child_mut(value, &key.into()) {
            match child {
                IvyJson::Object(fields) => {
                    for (_, v) in fields {
                        strip(v);
                    }
                }
                IvyJson::Array(items) => {
                    for v in items {
                        strip(v);
                    }
                }
                _ => {}
            }
        }
    }
    for key in ["anyOf", "oneOf", "allOf"] {
        if let Some(IvyJson::Array(items)) = child_mut(value, &key.into()) {
            for v in items {
                strip(v);
            }
        }
    }
    for key in ["items", "additionalProperties", "if", "then", "else", "not"] {
        if let Some(child) = child_mut(value, &key.into()) {
            strip(child);
        }
    }
}
