//! Catalog navigation, provider-bound descriptions and retained definition identities.
use crate::{registry::Registry, store::Store};
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString,
    core_bundle::{bundled_schema, operation_input_schema},
    core_schema::{operation_catalog, operation_definition},
    hash::{digest, hash_json, tool_definition_hash},
    json::{canonical, parse, stringify},
};
use rusqlite::OptionalExtension;
use std::{
    cmp::Ordering,
    collections::{HashMap, HashSet},
    sync::{Arc, OnceLock},
};
#[path = "discovery_text.rs"]
mod strings;
pub(crate) use strings::collapse_whitespace;
use strings::*;

/// Text and date services used by catalog/search policy.
pub trait Runtime: crate::retention::Runtime {
    /// Unicode lowercase used to match search terms.
    fn lowercase(&self, value: &JsString) -> IvyResult<JsString>;
    /// Whether a Unicode code point is a letter or number.
    fn word_character(&self, code_point: u32) -> bool;
    /// English text ordering for catalog presentation.
    fn compare_english(&self, a: &JsString, b: &JsString) -> Ordering;
}
fn text(value: &JsString) -> String {
    String::from_utf16_lossy(&value.0)
}
fn field<'a>(v: &'a IvyJson, k: &str) -> IvyResult<&'a IvyJson> {
    v.get(k).ok_or_else(IvyError::internal)
}
fn string<'a>(v: &'a IvyJson, k: &str) -> IvyResult<&'a JsString> {
    field(v, k)?.as_js_string().ok_or_else(IvyError::internal)
}
fn array<'a>(v: &'a IvyJson, k: &str) -> IvyResult<&'a [IvyJson]> {
    field(v, k)?.as_array().ok_or_else(IvyError::internal)
}
fn optional_array<'a>(v: &'a IvyJson, k: &str) -> IvyResult<&'a [IvyJson]> {
    match v.get(k).filter(|v| !v.is_null()) {
        Some(v) => v.as_array().ok_or_else(IvyError::internal),
        None => Ok(&[]),
    }
}
fn is_text(v: Option<&IvyJson>, s: &str) -> bool {
    v.and_then(IvyJson::as_js_string) == Some(&s.into())
}
fn require(v: bool, code: &str, message: &str) -> IvyResult<()> {
    if v {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn empty() -> IvyJson {
    IvyJson::object(std::iter::empty::<(&str, IvyJson)>())
}
fn description(method: &str) -> JsString {
    meta("coreDescriptions")
        .get(method)
        .and_then(IvyJson::as_js_string)
        .cloned()
        .unwrap_or_else(|| method.into())
}
fn core_methods() -> Vec<String> {
    operation_catalog()
        .iter()
        .filter(|(name, v)| {
            v.get("access").and_then(|v| v.as_str()) == Some("client")
                && v.get("discoverable").and_then(|v| v.as_bool()) != Some(false)
                && !["discovery.", "namespaces.", "tools."]
                    .iter()
                    .any(|p| name.starts_with(p))
        })
        .map(|(k, _)| k.clone())
        .collect()
}
fn provider(node: &IvyJson) -> IvyResult<IvyJson> {
    Ok(IvyJson::object([
        ("serviceNodeId", field(node, "serviceNodeId")?.clone()),
        ("hostId", field(node, "hostId")?.clone()),
        ("available", Registry::eligible(node).into()),
    ]))
}
#[derive(Clone)]
struct IndexedTool {
    qualified_name: JsString,
    namespace: JsString,
    group: JsString,
    group_description: JsString,
    description: JsString,
    effect: &'static str,
    words: Vec<JsString>,
    mcp: Option<IvyJson>,
}
impl IndexedTool {
    fn json(&self) -> IvyJson {
        let mut v = IvyJson::object([
            (
                "qualifiedName",
                IvyJson::String(self.qualified_name.clone()),
            ),
            ("namespace", IvyJson::String(self.namespace.clone())),
            ("group", IvyJson::String(self.group.clone())),
            (
                "groupDescription",
                IvyJson::String(self.group_description.clone()),
            ),
            ("description", IvyJson::String(self.description.clone())),
            ("effect", self.effect.into()),
            (
                "words",
                IvyJson::Array(self.words.iter().cloned().map(IvyJson::String).collect()),
            ),
        ]);
        if let Some(mcp) = &self.mcp {
            v.insert("mcp", mcp.clone());
        }
        v
    }
}
fn indexed(
    namespace: &JsString,
    name: &JsString,
    description: &JsString,
    read_only: Option<&IvyJson>,
    discovery: Option<&IvyJson>,
    groups: &[(JsString, JsString)],
    runtime: &dyn Runtime,
) -> IvyResult<IndexedTool> {
    let group = discovery
        .and_then(|v| v.get("group"))
        .filter(|v| !v.is_null())
        .and_then(IvyJson::as_js_string)
        .cloned()
        .unwrap_or_else(|| namespace.clone());
    let group_description = groups
        .iter()
        .find(|(k, _)| k == &group)
        .map(|(_, v)| v.clone())
        .unwrap_or_else(|| default_group(&group));
    let summary = compact(
        discovery
            .and_then(|v| v.get("summary"))
            .filter(|v| !v.is_null())
            .and_then(IvyJson::as_js_string)
            .unwrap_or(description),
    );
    let keywords = discovery
        .map(|d| optional_array(d, "keywords"))
        .transpose()?
        .unwrap_or(&[])
        .iter()
        .map(|v| v.as_js_string().cloned().ok_or_else(IvyError::internal))
        .collect::<IvyResult<Vec<_>>>()?;
    let source = joined(
        &[
            namespace.clone(),
            name.clone(),
            summary.clone(),
            description.clone(),
            group_description.clone(),
            joined(&keywords, " "),
        ],
        " ",
    );
    Ok(IndexedTool {
        qualified_name: concat(&[namespace, &".".into(), name]),
        namespace: namespace.clone(),
        group,
        group_description,
        description: summary,
        effect: match read_only {
            Some(IvyJson::Bool(true)) => "read",
            Some(IvyJson::Bool(false)) => "write",
            _ => "unknown",
        },
        words: words(&source, runtime)?,
        mcp: None,
    })
}
fn groups(ns: &IvyJson) -> IvyResult<Vec<(JsString, JsString)>> {
    let mut result = vec![];
    for g in optional_array(ns, "discoveryGroups")? {
        let key = string(g, "group")?.clone();
        let value = string(g, "description")?.clone();
        if let Some((_, prior)) = result.iter_mut().find(|(k, _)| k == &key) {
            *prior = value;
        } else {
            result.push((key, value));
        }
    }
    Ok(result)
}
struct Index {
    source: String,
    identity: String,
    tools: Vec<IndexedTool>,
    groups: Vec<(JsString, JsString)>,
    description: JsString,
    bytes: usize,
}
/// Retained core definitions and calls do not require a search/locale runtime.
#[derive(Default)]
pub struct CoreCalls(HashMap<JsString, IvyJson>);
impl CoreCalls {
    pub fn binding(&mut self, qualified: &JsString) -> IvyResult<IvyJson> {
        let name = text(qualified);
        let method = name.strip_prefix("hive.");
        require(
            method.is_some_and(|m| core_methods().iter().any(|v| v == m)),
            "not_found",
            "Unknown discoverable Hive operation.",
        )?;
        if let Some(binding) = self.0.get(qualified) {
            return Ok(binding.clone());
        }
        let method = method.unwrap();
        let op = operation_definition(method).ok_or_else(IvyError::internal)?;
        let read_only = op.get("mutation").and_then(|v| v.as_bool()) != Some(true);
        let input = operation_input_schema(method)?;
        let mut discovery = empty();
        if let Some(name) = meta("coreMcpNames").get(method) {
            discovery.insert(
                "mcp",
                IvyJson::object([
                    ("name", name.clone()),
                    (
                        "surface",
                        if meta("publicCoreMcpMethods")
                            .as_array()
                            .unwrap()
                            .iter()
                            .any(|v| v.as_js_string() == Some(&method.into()))
                        {
                            "ivy"
                        } else {
                            "ivy_dev"
                        }
                        .into(),
                    ),
                ]),
            );
        }
        let idempotent = read_only
            || input
                .get("required")
                .and_then(IvyJson::as_array)
                .unwrap_or(&[])
                .iter()
                .any(|v| is_text(Some(v), "mutationId"));
        let definition = IvyJson::object([
            ("namespace", "hive".into()),
            ("name", method.into()),
            ("interfaceVersion", "1.0.0".into()),
            ("description", IvyJson::String(description(method))),
            ("discovery", discovery),
            ("inputSchema", input),
            (
                "outputSchema",
                bundled_schema(
                    &op.get("output")
                        .and_then(|v| v.as_str())
                        .ok_or_else(IvyError::internal)?
                        .into(),
                    Some(&"https://ivy.invalid/schemas/hive-operations.schema.json".into()),
                )?,
            ),
            (
                "annotations",
                IvyJson::object([
                    ("readOnlyHint", read_only.into()),
                    ("idempotentHint", idempotent.into()),
                ]),
            ),
        ]);
        let hash = tool_definition_hash(&definition)?;
        let binding = IvyJson::object([
            ("qualifiedName", IvyJson::String(qualified.clone())),
            ("definition", definition),
            ("definitionHash", hash.into()),
        ]);
        self.0.insert(qualified.clone(), binding.clone());
        Ok(binding)
    }
    pub fn resolve(&mut self, params: &IvyJson) -> IvyResult<IvyJson> {
        require(
            params
                .get("serviceNodeId")
                .and_then(IvyJson::as_js_string)
                .is_none_or(|v| v.0.is_empty())
                && params
                    .get("operationId")
                    .and_then(IvyJson::as_js_string)
                    .is_none_or(|v| v.0.is_empty()),
            "invalid_arguments",
            "Hive core uses its declared arguments and mutationId, not a service node or outer operationId.",
        )?;
        let binding = self.binding(string(params, "qualifiedName")?)?;
        require(
            binding.get("definitionHash") == params.get("expectedDefinitionHash"),
            "tool_definition_changed",
            "The retained Hive definition changed; describe and reassess.",
        )?;
        let arguments = field(params, "arguments")?;
        require(
            arguments.as_object().is_some(),
            "invalid_arguments",
            "Hive operation arguments must be an object.",
        )?;
        return Ok(IvyJson::object([
            (
                "method",
                field(field(&binding, "definition")?, "name")?.clone(),
            ),
            ("params", arguments.clone()),
        ]));
    }
}

pub struct Discovery {
    indexes: Vec<(JsString, Arc<Index>)>,
    index_bytes: usize,
    core_index: Vec<IndexedTool>,
    core_bindings: CoreCalls,
}
impl Discovery {
    pub fn new(runtime: &dyn Runtime) -> IvyResult<Self> {
        let groups = meta("coreGroups")
            .as_object()
            .ok_or_else(IvyError::internal)?
            .iter()
            .map(|(k, v)| {
                Ok((
                    k.clone(),
                    v.as_js_string().ok_or_else(IvyError::internal)?.clone(),
                ))
            })
            .collect::<IvyResult<Vec<_>>>()?;
        let mut core_index = vec![];
        for method in core_methods() {
            core_index.push(indexed(
                &"hive".into(),
                &method.as_str().into(),
                &description(&method),
                Some(
                    &(!operation_definition(&method)
                        .and_then(|v| v.get("mutation"))
                        .is_some_and(|v| v.as_bool() == Some(true)))
                    .into(),
                ),
                Some(&IvyJson::object([(
                    "group",
                    method.split('.').next().unwrap().into(),
                )])),
                &groups,
                runtime,
            )?);
        }
        Ok(Self {
            indexes: vec![],
            index_bytes: 0,
            core_index,
            core_bindings: CoreCalls::default(),
        })
    }
    pub fn core_binding(&mut self, qualified: &JsString) -> IvyResult<IvyJson> {
        self.core_bindings.binding(qualified)
    }
    pub fn tool_schema(
        &mut self,
        store: &Store,
        registry: &mut Registry,
        params: &IvyJson,
        runtime: &dyn crate::mcp::Runtime,
        now: f64,
    ) -> IvyResult<IvyJson> {
        let name = string(params, "name")?;
        let bindings = self
            .mcp_catalog(store, registry, Some(name), runtime, now)?
            .into_iter()
            .filter(|b| {
                b.get("definition")
                    .and_then(mcp_name)
                    .is_some_and(|v| is_text(v.get("surface"), "ivy"))
            })
            .collect::<Vec<_>>();
        let tool = crate::mcp::direct_descriptors(&bindings, runtime)?
            .into_iter()
            .find(|t| t.get("name") == params.get("name"))
            .ok_or_else(|| {
                IvyError::new(
                    "not_found",
                    "Public MCP tool is not in the current catalog.",
                )
            })?;
        let output = tool.get("outputSchema").cloned().unwrap_or(IvyJson::Null);
        let input = field(&tool, "inputSchema")?.clone();
        let hash = hash_json(&IvyJson::object([
            ("inputSchema", input.clone()),
            ("outputSchema", output.clone()),
        ]))?;
        Ok(IvyJson::object([
            ("name", field(&tool, "name")?.clone()),
            (
                "description",
                tool.get("description")
                    .cloned()
                    .unwrap_or_else(|| "".into()),
            ),
            ("inputSchema", input),
            ("outputSchema", output),
            ("schemaHash", hash.into()),
            ("complete", true.into()),
        ]))
    }
    fn evict(&mut self, id: &JsString) {
        if let Some(i) = self.indexes.iter().position(|(k, _)| k == id) {
            self.index_bytes -= self.indexes.remove(i).1.bytes;
        }
    }
    fn index(
        &mut self,
        store: &Store,
        node: &IvyJson,
        runtime: &dyn Runtime,
    ) -> IvyResult<Arc<Index>> {
        let id = string(node, "serviceNodeId")?;
        let source = store
            .connection()
            .query_row(
                "SELECT registry_json FROM registries WHERE node_id=?",
                [text(id)],
                |r| r.get::<_, Option<String>>(0),
            )
            .optional()
            .map_err(|e| store.query_error(e))?
            .flatten()
            .unwrap_or_default();
        if let Some(i) = self
            .indexes
            .iter()
            .position(|(k, v)| k == id && v.source == source)
        {
            let item = self.indexes.remove(i);
            let result = item.1.clone();
            self.indexes.push(item);
            return Ok(result);
        }
        let catalog = if source.is_empty() {
            IvyJson::Null
        } else {
            parse(&source, usize::MAX).map_err(|_| IvyError::internal())?
        };
        let mut declared = vec![];
        let mut tools = vec![];
        let mut descriptions = vec![];
        for ns in if catalog.is_null() {
            &[][..]
        } else {
            array(&catalog, "namespaces")?
        } {
            let ns_groups = groups(ns)?;
            for (key, value) in &ns_groups {
                if let Some((_, prior)) = declared.iter_mut().find(|(k, _)| k == key) {
                    *prior = value.clone();
                } else {
                    declared.push((key.clone(), value.clone()));
                }
            }
            descriptions.push(string(ns, "description")?.clone());
            for tool in array(ns, "tools")? {
                let mut value = indexed(
                    string(ns, "namespace")?,
                    string(tool, "name")?,
                    string(tool, "description")?,
                    tool.get("annotations").and_then(|v| v.get("readOnlyHint")),
                    tool.get("discovery"),
                    &ns_groups,
                    runtime,
                )?;
                value.mcp = mcp_name(tool).filter(|v| v.as_object().is_some()).cloned();
                tools.push(value);
            }
        }
        let hint = catalog
            .get("discoveryHint")
            .and_then(IvyJson::as_js_string)
            .map(JsString::trim_ecmascript)
            .filter(|v| !v.0.is_empty());
        let descriptions = joined(&descriptions, "; ");
        let description = compact(&hint.unwrap_or_else(|| {
            if descriptions.0.is_empty() {
                "No synchronized tool catalog.".into()
            } else {
                descriptions
            }
        }));
        let bytes = source.len()
            + stringify(
                &IvyJson::Array(tools.iter().map(IndexedTool::json).collect()),
                usize::MAX,
            )?
            .len();
        let result = Arc::new(Index {
            identity: digest(source.as_bytes()),
            source,
            groups: declared,
            description,
            tools,
            bytes,
        });
        self.evict(id);
        while !self.indexes.is_empty()
            && (self.index_bytes + bytes > 16 * 1024 * 1024 || self.indexes.len() >= 128)
        {
            let id = self.indexes[0].0.clone();
            self.evict(&id);
        }
        if bytes <= 16 * 1024 * 1024 {
            self.indexes.push((id.clone(), result.clone()));
            self.index_bytes += bytes;
        }
        Ok(result)
    }
    pub fn mcp_catalog(
        &mut self,
        store: &Store,
        registry: &mut Registry,
        name: Option<&JsString>,
        runtime: &dyn Runtime,
        now: f64,
    ) -> IvyResult<Vec<IvyJson>> {
        fn policy(definition: &IvyJson) -> IvyResult<JsString> {
            match mcp_name(definition).filter(|v| v.as_object().is_some()) {
                Some(mcp) => Ok(concat(&[
                    string(mcp, "name")?,
                    &"\0".into(),
                    mcp.get("surface")
                        .filter(|v| !v.is_null())
                        .and_then(IvyJson::as_js_string)
                        .unwrap_or(&"ivy_dev".into()),
                ])),
                None => Ok("".into()),
            }
        }
        let include = |binding: &IvyJson| -> bool {
            mcp_name(binding.get("definition").unwrap_or(&IvyJson::Null)).is_some_and(|mcp| {
                mcp.as_object().is_some()
                    && name.is_none_or(|n| {
                        n.0.is_empty() || mcp.get("name").and_then(IvyJson::as_js_string) == Some(n)
                    })
            })
        };
        let mut active: HashMap<(JsString, JsString), HashSet<JsString>> = HashMap::new();
        for node in registry
            .nodes(store, runtime, now)?
            .iter()
            .filter(|v| Registry::eligible(v))
        {
            if let Some(catalog) = registry
                .get_registry(store, string(node, "serviceNodeId")?)?
                .filter(|v| v.as_object().is_some())
            {
                for ns in array(&catalog, "namespaces")? {
                    for definition in array(ns, "tools")? {
                        active
                            .entry((
                                string(node, "serviceName")?.clone(),
                                concat(&[
                                    string(ns, "namespace")?,
                                    &".".into(),
                                    string(definition, "name")?,
                                ]),
                            ))
                            .or_default()
                            .insert(policy(definition)?);
                    }
                }
            }
        }
        let mut result = vec![];
        for method in core_methods() {
            let mut binding = self.core_binding(&format!("hive.{method}").as_str().into())?;
            if include(&binding) {
                binding.insert("serviceName", "hive".into());
                binding.insert("provider", IvyJson::Null);
                binding.insert("guideMarkdown", "".into());
                result.push(binding);
            }
        }
        for node in registry.nodes(store, runtime, now)? {
            let Some(catalog) = registry
                .get_registry(store, string(&node, "serviceNodeId")?)?
                .filter(|v| v.as_object().is_some())
            else {
                continue;
            };
            for ns in array(&catalog, "namespaces")? {
                let guide = guide(string(ns, "guideMarkdown")?, &catalog)?;
                for binding in registry.bindings(store, string(&node, "serviceNodeId")?, ns)? {
                    let available = Registry::eligible(&node);
                    let current = active.get(&(
                        string(&node, "serviceName")?.clone(),
                        string(binding, "qualifiedName")?.clone(),
                    ));
                    let binding_policy = policy(field(binding, "definition")?)?;
                    if !available
                        && current.is_some_and(|v| v.len() == 1 && !v.contains(&binding_policy))
                    {
                        continue;
                    }
                    if include(binding) {
                        let mut binding = binding.clone();
                        binding.insert("serviceName", field(&node, "serviceName")?.clone());
                        binding.insert("provider", provider(&node)?);
                        binding.insert("guideMarkdown", IvyJson::String(guide.clone()));
                        result.push(binding);
                    }
                }
            }
        }
        result.sort_by(|a, b| {
            let a_name = mcp_name(a.get("definition").unwrap())
                .unwrap()
                .get("name")
                .unwrap()
                .as_js_string()
                .unwrap();
            let b_name = mcp_name(b.get("definition").unwrap())
                .unwrap()
                .get("name")
                .unwrap()
                .as_js_string()
                .unwrap();
            runtime.compare_english(a_name, b_name).then_with(|| {
                runtime.compare_english(
                    a.get("provider")
                        .and_then(|v| v.get("serviceNodeId"))
                        .and_then(IvyJson::as_js_string)
                        .unwrap_or(&"".into()),
                    b.get("provider")
                        .and_then(|v| v.get("serviceNodeId"))
                        .and_then(IvyJson::as_js_string)
                        .unwrap_or(&"".into()),
                )
            })
        });
        Ok(result)
    }
    pub fn list(
        &mut self,
        store: &Store,
        registry: &Registry,
        params: &IvyJson,
        mcp_only: bool,
        runtime: &dyn Runtime,
        now: f64,
    ) -> IvyResult<IvyJson> {
        require(
            (params
                .get("group")
                .and_then(IvyJson::as_js_string)
                .is_none_or(|v| v.0.is_empty())
                && params.get("providers").and_then(IvyJson::as_bool) != Some(true))
                || params
                    .get("serviceName")
                    .and_then(IvyJson::as_js_string)
                    .is_some_and(|v| !v.0.is_empty()),
            "invalid_arguments",
            "Groups and provider lists require a serviceName.",
        )?;
        require(
            params.get("providers").and_then(IvyJson::as_bool) != Some(true)
                || (params
                    .get("query")
                    .and_then(IvyJson::as_js_string)
                    .is_none_or(|v| v.0.is_empty())
                    && params
                        .get("group")
                        .and_then(IvyJson::as_js_string)
                        .is_none_or(|v| v.0.is_empty())
                    && params
                        .get("serviceNodeId")
                        .and_then(IvyJson::as_js_string)
                        .is_none_or(|v| v.0.is_empty())),
            "invalid_arguments",
            "Provider selection cannot be combined with tool filters.",
        )?;
        let mut nodes = vec![];
        for node in registry.nodes(store, runtime, now)? {
            if !mcp_only
                || self
                    .index(store, &node, runtime)?
                    .tools
                    .iter()
                    .any(|t| t.mcp.is_some())
            {
                nodes.push(node);
            }
        }
        let evict: Vec<_> = self
            .indexes
            .iter()
            .filter(|(id, _)| {
                !nodes
                    .iter()
                    .any(|v| v.get("serviceNodeId").and_then(IvyJson::as_js_string) == Some(id))
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in evict {
            self.evict(&id);
        }
        if params
            .get("serviceNodeId")
            .and_then(IvyJson::as_js_string)
            .is_some_and(|v| !v.0.is_empty())
        {
            let node = registry.node(store, string(params, "serviceNodeId")?, runtime, now)?;
            require(
                !mcp_only
                    || self
                        .index(store, &node, runtime)?
                        .tools
                        .iter()
                        .any(|t| t.mcp.is_some()),
                "not_found",
                "Service does not publish an MCP surface.",
            )?;
            require(
                params
                    .get("serviceName")
                    .and_then(IvyJson::as_js_string)
                    .is_none_or(|v| v.0.is_empty())
                    || node.get("serviceName") == params.get("serviceName"),
                "target_conflict",
                "The selected node belongs to a different service.",
            )?;
        }
        let selected: Vec<_> = nodes
            .into_iter()
            .filter(|n| {
                (params
                    .get("serviceName")
                    .and_then(IvyJson::as_js_string)
                    .is_none_or(|v| v.0.is_empty())
                    || n.get("serviceName") == params.get("serviceName"))
                    && (params
                        .get("serviceNodeId")
                        .and_then(IvyJson::as_js_string)
                        .is_none_or(|v| v.0.is_empty())
                        || n.get("serviceNodeId") == params.get("serviceNodeId"))
            })
            .collect();
        let service = params
            .get("serviceName")
            .filter(|v| !v.is_null())
            .and_then(IvyJson::as_js_string)
            .cloned()
            .or_else(|| {
                if params
                    .get("serviceNodeId")
                    .and_then(IvyJson::as_js_string)
                    .is_some_and(|v| !v.0.is_empty())
                {
                    selected
                        .first()
                        .and_then(|v| v.get("serviceName"))
                        .and_then(IvyJson::as_js_string)
                        .cloned()
                } else {
                    None
                }
            });
        require(
            service
                .as_ref()
                .is_none_or(|v| v.0.is_empty() || v == &"hive".into())
                || !selected.is_empty(),
            "not_found",
            "Service is not registered.",
        )?;
        let mut identity = vec![];
        for node in &selected {
            identity.push(IvyJson::object([
                ("id", field(node, "serviceNodeId")?.clone()),
                ("serviceName", field(node, "serviceName")?.clone()),
                ("available", Registry::eligible(node).into()),
                (
                    "catalog",
                    self.index(store, node, runtime)?.identity.clone().into(),
                ),
            ]));
        }
        let mut entries = vec![];
        let mut scores: HashMap<(JsString, JsString), usize> = HashMap::new();
        if service.as_ref().is_none_or(|v| v.0.is_empty())
            && params
                .get("query")
                .and_then(IvyJson::as_js_string)
                .is_none_or(|v| v.0.is_empty())
        {
            entries.push(IvyJson::object([
                ("kind", "service".into()),
                ("name", "hive".into()),
                ("serviceName", "hive".into()),
                ("description", meta("coreServiceDescription").clone()),
                ("toolCount", IvyJson::Number(self.core_index.len() as f64)),
                ("providerCount", IvyJson::Number(1.0)),
                ("availableCount", IvyJson::Number(1.0)),
            ]));
            let mut names: Vec<_> = selected
                .iter()
                .map(|n| string(n, "serviceName").cloned())
                .collect::<IvyResult<_>>()?;
            names.sort_by(|a, b| a.0.cmp(&b.0));
            names.dedup();
            for name in names {
                let owners: Vec<_> = selected
                    .iter()
                    .filter(|n| n.get("serviceName").and_then(IvyJson::as_js_string) == Some(&name))
                    .collect();
                let description = self.index(store, owners[0], runtime)?.description.clone();
                let mut names = HashSet::new();
                for node in &owners {
                    for t in &self.index(store, node, runtime)?.tools {
                        if !mcp_only || t.mcp.is_some() {
                            names.insert(t.qualified_name.clone());
                        }
                    }
                }
                entries.push(IvyJson::object([
                    ("kind", "service".into()),
                    ("name", IvyJson::String(name.clone())),
                    ("serviceName", IvyJson::String(name)),
                    ("description", IvyJson::String(description)),
                    ("toolCount", IvyJson::Number(names.len() as f64)),
                    ("providerCount", IvyJson::Number(owners.len() as f64)),
                    (
                        "availableCount",
                        IvyJson::Number(
                            owners.iter().filter(|n| Registry::eligible(n)).count() as f64
                        ),
                    ),
                ]));
            }
        } else if params.get("providers").and_then(IvyJson::as_bool) == Some(true) {
            for node in &selected {
                let description = concat(&[
                    string(node, "hostId")?,
                    &": ".into(),
                    string(node, "serviceName")?,
                    &" ".into(),
                    string(node, "version")?,
                    &"; native ".into(),
                    node.get("nativeVersion")
                        .filter(|v| !v.is_null())
                        .and_then(IvyJson::as_js_string)
                        .unwrap_or(&"none".into()),
                    &".".into(),
                ]);
                entries.push(IvyJson::object([
                    ("kind", "provider".into()),
                    ("name", field(node, "serviceNodeId")?.clone()),
                    ("serviceName", field(node, "serviceName")?.clone()),
                    ("serviceNodeId", field(node, "serviceNodeId")?.clone()),
                    ("hostId", field(node, "hostId")?.clone()),
                    ("available", Registry::eligible(node).into()),
                    ("description", IvyJson::String(compact(&description))),
                ]));
            }
        } else {
            let mut records = vec![];
            if service
                .as_ref()
                .is_none_or(|v| v.0.is_empty() || v == &"hive".into())
            {
                for tool in &self.core_index {
                    records.push((JsString::from("hive"), tool.clone(), true));
                }
            }
            for node in &selected {
                for tool in &self.index(store, node, runtime)?.tools {
                    if !mcp_only || tool.mcp.is_some() {
                        records.push((
                            string(node, "serviceName")?.clone(),
                            tool.clone(),
                            Registry::eligible(node),
                        ));
                    }
                }
            }
            let query = if params
                .get("query")
                .and_then(IvyJson::as_js_string)
                .is_some_and(|v| !v.0.is_empty())
            {
                let mut seen = HashSet::new();
                words(string(params, "query")?, runtime)?
                    .into_iter()
                    .filter(|v| seen.insert(v.clone()))
                    .collect::<Vec<_>>()
            } else {
                vec![]
            };
            require(
                params
                    .get("query")
                    .and_then(IvyJson::as_js_string)
                    .is_none_or(|v| v.0.is_empty())
                    || !query.is_empty(),
                "invalid_arguments",
                "Search needs at least one meaningful term.",
            )?;
            let mut by_name: HashMap<(JsString, JsString), usize> = HashMap::new();
            for (name, tool, available) in records {
                if params
                    .get("group")
                    .and_then(IvyJson::as_js_string)
                    .is_some_and(|v| !v.0.is_empty())
                {
                    let group = string(params, "group")?;
                    if &tool.group != group && !starts(&tool.group, &concat(&[group, &"/".into()]))
                    {
                        continue;
                    }
                }
                let service_words = if query.is_empty() {
                    vec![]
                } else {
                    words(&name, runtime)?
                };
                if !query.iter().all(|term| {
                    tool.words
                        .iter()
                        .chain(service_words.iter())
                        .any(|word| includes(word, term))
                }) {
                    continue;
                }
                let key = (name.clone(), tool.qualified_name.clone());
                let names = words(&tool.qualified_name, runtime)?;
                let summary = words(&tool.description, runtime)?;
                let score = query
                    .iter()
                    .map(|term| {
                        if names.contains(term) {
                            20
                        } else if names.iter().any(|w| includes(w, term)) {
                            10
                        } else if summary.contains(term) {
                            5
                        } else {
                            1
                        }
                    })
                    .sum::<usize>();
                scores
                    .entry(key.clone())
                    .and_modify(|s| *s = (*s).max(score))
                    .or_insert(score);
                if let Some(i) = by_name.get(&key) {
                    let prior: &mut IvyJson = &mut entries[*i];
                    for k in ["providerCount", "availableCount"] {
                        if k == "providerCount" || available {
                            let n = prior
                                .get(k)
                                .and_then(IvyJson::as_f64)
                                .ok_or_else(IvyError::internal)?;
                            prior.insert(k, IvyJson::Number(n + 1.0));
                        }
                    }
                    if !is_text(prior.get("effect"), tool.effect) {
                        prior.insert("effect", "unknown".into());
                    }
                } else {
                    let mut entry = IvyJson::object([
                        ("kind", "tool".into()),
                        ("name", IvyJson::String(tool.qualified_name.clone())),
                        ("description", IvyJson::String(tool.description)),
                        ("serviceName", IvyJson::String(name.clone())),
                        ("namespace", IvyJson::String(tool.namespace)),
                        ("group", IvyJson::String(tool.group)),
                    ]);
                    let mcp = tool
                        .mcp
                        .as_ref()
                        .and_then(|v| v.get("name"))
                        .cloned()
                        .or_else(|| {
                            if name == "hive".into() {
                                meta("coreMcpNames")
                                    .get(&text(&JsString(tool.qualified_name.0[5..].to_vec())))
                                    .cloned()
                            } else {
                                None
                            }
                        });
                    if let Some(v) =
                        mcp.filter(|v| v.as_js_string().is_some_and(|s| !s.0.is_empty()))
                    {
                        entry.insert("mcpName", v);
                    }
                    entry.insert("effect", tool.effect.into());
                    entry.insert("providerCount", IvyJson::Number(1.0));
                    entry.insert(
                        "availableCount",
                        IvyJson::Number(if available { 1.0 } else { 0.0 }),
                    );
                    by_name.insert(key, entries.len());
                    entries.push(entry);
                }
            }
            if params
                .get("query")
                .and_then(IvyJson::as_js_string)
                .is_none_or(|v| v.0.is_empty())
                && entries.len() > 20
            {
                let prefix = if params
                    .get("group")
                    .and_then(IvyJson::as_js_string)
                    .is_some_and(|v| !v.0.is_empty())
                {
                    concat(&[string(params, "group")?, &"/".into()])
                } else {
                    "".into()
                };
                let mut grouped: Vec<IvyJson> = vec![];
                let mut keys: HashMap<JsString, usize> = HashMap::new();
                for entry in entries {
                    let group = string(&entry, "group")?;
                    if entry.get("group") == params.get("group") {
                        let name = string(&entry, "name")?.clone();
                        if let Some(i) = keys.get(&name) {
                            grouped[*i] = entry;
                        } else {
                            keys.insert(name, grouped.len());
                            grouped.push(entry);
                        }
                        continue;
                    }
                    let group = concat(&[
                        &prefix,
                        &segment(
                            &JsString(group.0[prefix.0.len().min(group.0.len())..].to_vec()),
                            47,
                        ),
                    ]);
                    if let Some(i) = keys.get(&group) {
                        let n = field(&grouped[*i], "toolCount")?
                            .as_f64()
                            .ok_or_else(IvyError::internal)?;
                        grouped[*i].insert("toolCount", IvyJson::Number(n + 1.0));
                    } else {
                        let mut description = if is_text(entry.get("serviceName"), "hive") {
                            meta("coreGroups")
                                .get_key(&group)
                                .and_then(IvyJson::as_js_string)
                                .cloned()
                        } else {
                            None
                        };
                        if !is_text(entry.get("serviceName"), "hive") {
                            for node in &selected {
                                let index = self.index(store, node, runtime)?;
                                if let Some((_, d)) = index
                                    .groups
                                    .iter()
                                    .find(|(k, d)| k == &group && !d.0.is_empty())
                                {
                                    description = Some(d.clone());
                                    break;
                                }
                            }
                        }
                        let description =
                            compact(&description.unwrap_or_else(|| default_group(&group)));
                        keys.insert(group.clone(), grouped.len());
                        grouped.push(IvyJson::object([
                            ("kind", "group".into()),
                            ("name", IvyJson::String(group.clone())),
                            ("group", IvyJson::String(group)),
                            ("serviceName", field(&entry, "serviceName")?.clone()),
                            ("description", IvyJson::String(description)),
                            ("toolCount", IvyJson::Number(1.0)),
                        ]));
                    }
                }
                entries = grouped;
            }
        }
        let key = |e: &IvyJson| -> (JsString, JsString) {
            (
                string(e, "serviceName").unwrap().clone(),
                string(e, "name").unwrap().clone(),
            )
        };
        entries.sort_by(|a, b| {
            scores
                .get(&key(b))
                .unwrap_or(&0)
                .cmp(scores.get(&key(a)).unwrap_or(&0))
                .then_with(|| {
                    let a = key(a);
                    let b = key(b);
                    text(&concat(&[&a.0, &"\0".into(), &a.1]))
                        .into_bytes()
                        .cmp(&text(&concat(&[&b.0, &"\0".into(), &b.1])).into_bytes())
                })
        });
        let mut selectors = params.clone();
        selectors.remove("cursor");
        selectors.remove("limit");
        let identity = IvyJson::object([
            ("method", "discovery.list".into()),
            ("mcpOnly", mcp_only.into()),
            ("selectors", selectors),
            ("catalogs", IvyJson::Array(identity)),
            (
                "entries",
                hash_json(&IvyJson::Array(entries.clone()))?.into(),
            ),
        ]);
        let boundary = if params
            .get("cursor")
            .and_then(IvyJson::as_js_string)
            .is_some_and(|v| !v.0.is_empty())
        {
            store.read_cursor(&identity, string(params, "cursor")?)?
        } else {
            Some(IvyJson::Number(0.0))
        };
        let offset = boundary
            .as_ref()
            .and_then(IvyJson::as_f64)
            .filter(|v| {
                v.is_finite()
                    && v.fract() == 0.0
                    && *v >= 0.0
                    && *v <= 9_007_199_254_740_991.0
                    && *v <= entries.len() as f64
            })
            .ok_or_else(|| IvyError::new("invalid_cursor", "Invalid discovery boundary."))?
            as usize;
        let mut items = vec![];
        let mut next = IvyJson::Null;
        let limit = params
            .get("limit")
            .filter(|v| !v.is_null())
            .and_then(IvyJson::as_f64)
            .unwrap_or(20.0);
        for (i, entry) in entries.iter().enumerate().skip(offset) {
            if items.len() as f64 >= limit {
                break;
            }
            let candidate = if i + 1 < entries.len() {
                store
                    .cursor(&identity, &IvyJson::Number((i + 1) as f64))?
                    .into()
            } else {
                IvyJson::Null
            };
            let mut page = items.clone();
            page.push(entry.clone());
            if stringify(
                &IvyJson::object([
                    ("items", IvyJson::Array(page)),
                    ("nextCursor", candidate.clone()),
                ]),
                usize::MAX,
            )?
            .len()
                > 8192
            {
                break;
            }
            items.push(entry.clone());
            next = candidate;
        }
        require(
            !items.is_empty() || offset == entries.len(),
            "result_too_large",
            "A discovery entry exceeds the page limit.",
        )?;
        Ok(IvyJson::object([
            ("items", IvyJson::Array(items)),
            ("nextCursor", next),
        ]))
    }
    pub fn describe(
        &mut self,
        store: &Store,
        registry: &Registry,
        params: &IvyJson,
        mcp_only: bool,
        runtime: &dyn Runtime,
        now: f64,
    ) -> IvyResult<IvyJson> {
        if is_text(params.get("serviceName"), "hive") {
            require(
                params
                    .get("serviceNodeId")
                    .and_then(IvyJson::as_js_string)
                    .is_none_or(|v| v.0.is_empty()),
                "target_conflict",
                "Hive core operations do not have a service node.",
            )?;
            let items = array(params, "tools")?
                .iter()
                .map(|n| self.core_binding(n.as_js_string().ok_or_else(IvyError::internal)?))
                .collect::<IvyResult<Vec<_>>>()?;
            return details(IvyJson::object([
                ("serviceName", "hive".into()),
                ("provider", IvyJson::Null),
                (
                    "guides",
                    IvyJson::Array(vec![IvyJson::object([
                        ("namespace", "hive".into()),
                        ("guideMarkdown", meta("coreGuide").clone()),
                    ])]),
                ),
                ("items", IvyJson::Array(items)),
            ]));
        }
        let mut owner = params
            .get("serviceNodeId")
            .and_then(IvyJson::as_js_string)
            .cloned();
        let mut guides: Vec<(JsString, IvyJson)> = vec![];
        let mut namespaces: HashMap<JsString, IvyJson> = HashMap::new();
        let mut items = vec![];
        for qualified in array(params, "tools")? {
            let qualified = qualified.as_js_string().ok_or_else(IvyError::internal)?;
            let namespace = segment(qualified, 46);
            if !namespaces.contains_key(&namespace) {
                let mut target =
                    IvyJson::object([("serviceName", field(params, "serviceName")?.clone())]);
                if let Some(owner) = owner.as_ref().filter(|v| !v.0.is_empty()) {
                    target.insert("serviceNodeId", IvyJson::String(owner.clone()));
                }
                let node = registry.select(store, &namespace, &target, runtime, now)?;
                let id = string(&node, "serviceNodeId")?.clone();
                namespaces.insert(
                    namespace.clone(),
                    registry.namespace(store, &id, &namespace)?,
                );
                owner = Some(id);
            }
            let ns = &namespaces[&namespace];
            let definition = array(ns, "tools")?
                .iter()
                .find(|v| {
                    string(v, "name")
                        .is_ok_and(|name| concat(&[&namespace, &".".into(), name]) == *qualified)
                })
                .ok_or_else(|| {
                    IvyError::new(
                        "not_found",
                        "Exact tool is absent from the selected provider catalog.",
                    )
                })?;
            require(
                !mcp_only || mcp_name(definition).is_some_and(|v| v.as_object().is_some()),
                "not_found",
                "Tool is not published through MCP.",
            )?;
            let markdown = field(ns, "guideMarkdown")?.clone();
            if let Some((_, prior)) = guides.iter_mut().find(|(k, _)| k == &namespace) {
                *prior = markdown;
            } else {
                guides.push((namespace, markdown));
            }
            items.push(IvyJson::object([
                ("qualifiedName", IvyJson::String(qualified.clone())),
                ("definition", definition.clone()),
                ("definitionHash", tool_definition_hash(definition)?.into()),
            ]));
        }
        let node = registry.node(store, &owner.unwrap_or_default(), runtime, now)?;
        details(IvyJson::object([
            ("serviceName", field(params, "serviceName")?.clone()),
            ("provider", provider(&node)?),
            (
                "guides",
                IvyJson::Array(
                    guides
                        .into_iter()
                        .map(|(ns, g)| {
                            IvyJson::object([
                                ("namespace", IvyJson::String(ns)),
                                ("guideMarkdown", g),
                            ])
                        })
                        .collect(),
                ),
            ),
            ("items", IvyJson::Array(items)),
        ]))
    }
    pub fn resolve_call(&mut self, params: &IvyJson) -> IvyResult<IvyJson> {
        if is_text(params.get("serviceName"), "hive") {
            return self.core_bindings.resolve(params);
        }
        require(
            params
                .get("serviceNodeId")
                .and_then(IvyJson::as_js_string)
                .is_some_and(|v| !v.0.is_empty()),
            "invalid_arguments",
            "Service calls require the exact described serviceNodeId.",
        )?;
        Ok(IvyJson::object([
            ("method", "tools.call".into()),
            ("params", params.clone()),
        ]))
    }
    pub fn instructions(
        &mut self,
        store: &Store,
        registry: &mut Registry,
        runtime: &dyn Runtime,
        now: f64,
    ) -> IvyResult<IvyJson> {
        let catalog = self.mcp_catalog(store, registry, None, runtime, now)?;
        let mut guides = vec![];
        for binding in &catalog {
            if (binding
                .get("provider")
                .is_some_and(|v| v.as_object().is_some()))
                && binding
                    .get("guideMarkdown")
                    .and_then(IvyJson::as_js_string)
                    .is_some_and(|v| !v.0.is_empty())
            {
                guides.push(concat(&[
                    string(binding, "serviceName")?,
                    &"/".into(),
                    string(field(binding, "definition")?, "namespace")?,
                    &": ".into(),
                    string(binding, "guideMarkdown")?,
                ]));
            }
        }
        let mut hints: Vec<(JsString, Vec<JsString>)> = vec![];
        for node in registry.nodes(store, runtime, now)? {
            if !self
                .index(store, &node, runtime)?
                .tools
                .iter()
                .any(|t| t.mcp.is_some())
            {
                continue;
            }
            let Some(hint) = registry
                .get_registry(store, string(&node, "serviceNodeId")?)?
                .as_ref()
                .and_then(|v| v.get("discoveryHint"))
                .and_then(IvyJson::as_js_string)
                .map(JsString::trim_ecmascript)
                .filter(|v| !v.0.is_empty())
            else {
                continue;
            };
            let name = string(&node, "serviceName")?.clone();
            if let Some((_, values)) = hints.iter_mut().find(|(n, _)| n == &name) {
                if !values.contains(&hint) {
                    values.push(hint);
                }
            } else {
                hints.push((name, vec![hint]));
            }
        }
        hints.sort_by(|a, b| runtime.compare_english(&a.0, &b.0));
        let mut value = JsString::from(include_str!(
            "../../../../instructions/hive-mcp-instructions.md"
        ))
        .trim_ecmascript();
        if !hints.is_empty() {
            value = concat(&[
                &value,
                &"\n\nServices registered to this Hive provide the following capabilities:\n"
                    .into(),
            ]);
            let mut lines = vec![];
            for (name, mut values) in hints {
                values.sort_by(|a, b| runtime.compare_english(a, b));
                for hint in values {
                    lines.push(concat(&[&"- ".into(), &name, &": ".into(), &hint]));
                }
            }
            value = concat(&[&value, &joined(&lines, "\n")]);
        }
        let mut seen = HashSet::new();
        guides.retain(|v| seen.insert(v.clone()));
        guides.sort_by(|a, b| runtime.compare_english(a, b));
        if !guides.is_empty() {
            value = concat(&[
                &value,
                &"\n\nTool workflows:\n".into(),
                &joined(
                    &guides
                        .iter()
                        .map(|v| concat(&[&"- ".into(), v]))
                        .collect::<Vec<_>>(),
                    "\n",
                ),
            ]);
        }
        require(
            text(&value).len() <= 32768,
            "limit_exceeded",
            "Composed Hive instructions exceed 32 KiB.",
        )?;
        Ok(IvyJson::object([("instructions", IvyJson::String(value))]))
    }
    pub fn instructions_page(
        &mut self,
        store: &Store,
        registry: &mut Registry,
        params: &IvyJson,
        runtime: &dyn Runtime,
        now: f64,
    ) -> IvyResult<IvyJson> {
        let view = params
            .get("view")
            .filter(|v| !v.is_null())
            .cloned()
            .unwrap_or_else(|| "instructions".into());
        let value = if is_text(Some(&view), "examples") {
            JsString::from(include_str!(
                "../../../../instructions/hive-mcp-examples.md"
            ))
            .trim_ecmascript()
        } else {
            string(
                &self.instructions(store, registry, runtime, now)?,
                "instructions",
            )?
            .clone()
        };
        let bytes = text(&value).into_bytes();
        let hash = digest(&bytes);
        let mut start = 0;
        if params
            .get("cursor")
            .and_then(IvyJson::as_js_string)
            .is_some_and(|v| !v.0.is_empty())
        {
            let cursor = text(string(params, "cursor")?);
            let parts = cursor.rsplit_once(':');
            require(
                parts.is_some_and(|(h, n)| {
                    h == hash
                        && n.len() <= 5
                        && !n.is_empty()
                        && n.bytes().all(|v| v.is_ascii_digit())
                }),
                "invalid_cursor",
                "Instructions changed; restart from the first page.",
            )?;
            start = parts
                .unwrap()
                .1
                .parse::<usize>()
                .map_err(|_| IvyError::internal())?;
            require(
                start > 0 && start < bytes.len() && (bytes[start] & 0xc0) != 0x80,
                "invalid_cursor",
                "Invalid instructions page boundary.",
            )?;
        }
        let mut end = bytes.len().min(start + 8192);
        while end < bytes.len() && (bytes[end] & 0xc0) == 0x80 {
            end -= 1;
        }
        Ok(IvyJson::object([
            ("view", view),
            (
                "text",
                String::from_utf8(bytes[start..end].to_vec())
                    .map_err(|_| IvyError::internal())?
                    .into(),
            ),
            ("sha256", hash.clone().into()),
            ("totalBytes", IvyJson::Number(bytes.len() as f64)),
            ("startByte", IvyJson::Number(start as f64)),
            ("endByte", IvyJson::Number(end as f64)),
            ("complete", (start == 0 && end == bytes.len()).into()),
            (
                "nextCursor",
                if end < bytes.len() {
                    format!("{hash}:{end}").into()
                } else {
                    IvyJson::Null
                },
            ),
        ]))
    }
}
fn details(result: IvyJson) -> IvyResult<IvyJson> {
    require(
        canonical(&result, usize::MAX)?.len() <= 512 * 1024,
        "result_too_large",
        "Selected definitions exceed the detail budget. Request fewer tools; schemas are never truncated.",
    )?;
    Ok(result)
}
