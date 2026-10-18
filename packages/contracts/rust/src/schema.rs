//! Lossless local-schema references, bundling and conservative query-path inference.
use crate::{IvyError, IvyJson, IvyResult, JsString, json::array_index};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};
#[path = "schema_keywords.rs"]
mod keywords;
pub use keywords::validate_compiler_keyword_values;
#[path = "schema_compiler_walk.rs"]
mod compiler_walk;
pub use compiler_walk::{CompilerNode, CompilerWalk};
#[path = "schema_resolution.rs"]
mod resolution;
pub use resolution::{CompilerResources, CompilerTarget};
#[path = "schema_meta.rs"]
mod meta;
#[path = "schema_resources.rs"]
mod resources;
pub use resources::{
    SchemaResourceGraph, SchemaResourceIndex, SchemaResourceNode, SchemaResourceReference,
};
#[path = "schema_uri.rs"]
pub mod uri;

/// An admitted finite schema graph. Compilation separately checks keyword values.
pub struct AdmittedSchema {
    schema: IvyJson,
}
impl AdmittedSchema {
    pub fn schema(&self) -> &IvyJson {
        &self.schema
    }
    /// Check keyword values against the selected pinned meta-schema. This is one
    /// compilation stage; it does not resolve resource IDs or compile validators.
    pub fn validate_meta_schema(&self) -> IvyResult<()> {
        meta::validate(&self.schema)
    }
    /// Enumerate compiler resource locations and select isolated compilation.
    /// URI resolution, duplicate-ID checks and validator compilation are separate.
    pub fn resource_graph(&self) -> SchemaResourceGraph<'_> {
        resources::collect(&self.schema)
    }
    /// Index local resource scopes and declarations, rejecting duplicate IDs.
    /// Compiler-wide registrations and validator compilation are separate.
    pub fn resource_index(&self) -> IvyResult<SchemaResourceIndex<'_>> {
        resources::index(&self.schema)
    }
    /// Check compiler-wide resource collisions and root registration against
    /// the bundled meta-schemas. Reference compilation remains a separate stage.
    pub fn registered_resources(&self) -> IvyResult<SchemaResourceIndex<'_>> {
        resources::register(&self.schema)
    }
    /// Select active inline branches and their compiler scopes. Reference target
    /// compilation, meta-validation and runtime evaluation remain separate.
    pub fn compiler_walk(&self) -> IvyResult<CompilerWalk<'_>> {
        compiler_walk::walk(&self.schema)
    }
    /// Resolve registered resource aliases and scoped compiler pointers before
    /// compiling or caching target validators.
    pub fn compiler_resources(&self) -> IvyResult<CompilerResources<'_>> {
        resolution::CompilerResources::new(&self.schema)
    }
}

/// Enforce the submitted-schema language, graph limits and RE2 pattern syntax.
pub fn admit_schema(schema: &IvyJson) -> IvyResult<AdmittedSchema> {
    crate::json::canonical(schema, 256 * 1024)?;
    const KEYWORDS: &[&str] = &[
        "$schema",
        "$id",
        "$defs",
        "$ref",
        "title",
        "description",
        "default",
        "examples",
        "deprecated",
        "readOnly",
        "writeOnly",
        "format",
        "type",
        "properties",
        "required",
        "additionalProperties",
        "minProperties",
        "maxProperties",
        "items",
        "minItems",
        "maxItems",
        "uniqueItems",
        "enum",
        "const",
        "anyOf",
        "oneOf",
        "allOf",
        "if",
        "then",
        "else",
        "not",
        "minimum",
        "maximum",
        "exclusiveMinimum",
        "exclusiveMaximum",
        "multipleOf",
        "minLength",
        "maxLength",
        "pattern",
    ];
    #[derive(Clone)]
    struct State {
        depth: usize,
        consumption: usize,
        trail: Arc<HashSet<usize>>,
        refs: Arc<HashMap<usize, usize>>,
    }
    enum Work<'a> {
        Inspect(&'a IvyJson, State),
        Keyword(&'a IvyJson, &'static str, State),
    }
    let identity = |value: &IvyJson| value as *const IvyJson as usize;
    let state = State {
        depth: 0,
        consumption: 0,
        trail: Arc::new(HashSet::new()),
        refs: Arc::new(HashMap::from([(identity(schema), 0)])),
    };
    let mut work = vec![Work::Inspect(schema, state)];
    let mut visited = 0;
    while let Some(step) = work.pop() {
        match step {
            Work::Inspect(node, mut state) => {
                visited += 1;
                if visited > 8192 || state.depth > 64 {
                    return Err(IvyError::new(
                        "limit_exceeded",
                        "Schema exceeds the validation complexity limit.",
                    ));
                }
                if matches!(node, IvyJson::Bool(_)) {
                    continue;
                }
                if node.as_object().is_none() {
                    return Err(registry("Expected a JSON schema."));
                }
                for (key, _) in entries(node) {
                    if !KEYWORDS
                        .iter()
                        .any(|keyword| key == &JsString::from(*keyword))
                    {
                        let mut message = JsString::from("Unsupported schema keyword: ");
                        message.0.extend(key.0.iter().take(80));
                        return Err(
                            registry("Unsupported schema keyword.").with_js_message(message)
                        );
                    }
                }
                if state.trail.contains(&identity(node)) {
                    return Err(registry("Schema has a non-consuming recursive reference."));
                }
                Arc::make_mut(&mut state.trail).insert(identity(node));
                for keyword in [
                    "$ref",
                    "pattern",
                    "anyOf",
                    "oneOf",
                    "allOf",
                    "if",
                    "then",
                    "else",
                    "not",
                    "properties",
                    "$defs",
                    "items",
                    "additionalProperties",
                ]
                .into_iter()
                .rev()
                {
                    if let Some(value) = node.get(keyword) {
                        work.push(Work::Keyword(value, keyword, state.clone()));
                    }
                }
            }
            Work::Keyword(value, keyword, state) => {
                let mut child_state = state.clone();
                child_state.depth += 1;
                match keyword {
                    "$ref" => {
                        let reference = value
                            .as_js_string()
                            .ok_or_else(|| registry("Invalid schema reference."))?;
                        let target = resolve(schema, reference)?;
                        let id = identity(target);
                        if let Some(prior) = state.refs.get(&id) {
                            if state.consumption <= *prior {
                                return Err(registry(
                                    "Schema recursion must consume a value level.",
                                ));
                            }
                        } else {
                            Arc::make_mut(&mut child_state.refs).insert(id, state.consumption);
                            work.push(Work::Inspect(target, child_state));
                        }
                    }
                    "pattern" => {
                        let pattern = value
                            .as_js_string()
                            .filter(|pattern| pattern.0.len() <= 1024)
                            .ok_or_else(|| registry("Schema pattern exceeds its limit."))?;
                        crate::re2::Re2Pattern::compile(pattern)?;
                    }
                    "anyOf" | "oneOf" | "allOf" => {
                        let children = value
                            .as_array()
                            .filter(|values| !values.is_empty() && values.len() <= 32)
                            .ok_or_else(|| registry("Schema alternatives exceed their limit."))?;
                        for child in children.iter().rev() {
                            work.push(Work::Inspect(child, child_state.clone()));
                        }
                    }
                    "properties" | "$defs" => {
                        let children = value
                            .as_object()
                            .ok_or_else(|| registry("Invalid schema property map."))?;
                        if children.len() > 1024 || value.get("__proto__").is_some() {
                            return Err(registry("Unsupported schema property map."));
                        }
                        child_state.trail = Arc::new(HashSet::new());
                        if keyword == "properties" {
                            child_state.consumption += 1;
                        }
                        for (_, child) in entries(value).into_iter().rev() {
                            work.push(Work::Inspect(child, child_state.clone()));
                        }
                    }
                    "items" | "additionalProperties" => {
                        child_state.trail = Arc::new(HashSet::new());
                        child_state.consumption += 1;
                        work.push(Work::Inspect(value, child_state));
                    }
                    _ => work.push(Work::Inspect(value, child_state)),
                }
            }
        }
    }
    Ok(AdmittedSchema {
        schema: schema.clone(),
    })
}

pub fn pointer_parts(pointer: &JsString) -> IvyResult<Vec<JsString>> {
    if pointer.0.first() != Some(&47) {
        return Err(invalid_pointer());
    }
    pointer.0[1..]
        .split(|unit| *unit == 47)
        .map(|part| {
            let mut decoded = Vec::with_capacity(part.len());
            let mut index = 0;
            while index < part.len() {
                if part[index] == 126 {
                    index += 1;
                    decoded.push(match part.get(index) {
                        Some(48) => 126,
                        Some(49) => 47,
                        _ => return Err(invalid_pointer()),
                    });
                } else {
                    decoded.push(part[index]);
                }
                index += 1;
            }
            Ok(JsString(decoded))
        })
        .collect()
}

fn invalid_pointer() -> IvyError {
    IvyError::new("invalid_arguments", "Invalid JSON pointer.")
}
fn registry(message: &str) -> IvyError {
    IvyError::new("registry_invalid", message)
}

fn resolve<'a>(root: &'a IvyJson, reference: &JsString) -> IvyResult<&'a IvyJson> {
    if !reference.0.starts_with(&[35, 47]) {
        return Err(registry(
            "Only local JSON pointer schema references are supported.",
        ));
    }
    let parts = pointer_parts(&JsString(reference.0[1..].to_vec()))?;
    let mut value = root;
    for (index, key) in parts.iter().enumerate() {
        value = match value {
            IvyJson::Array(values) => {
                if key == &JsString::from("length") {
                    return Err(registry(if index + 1 == parts.len() {
                        "Reference must address a schema."
                    } else {
                        "Unresolved schema reference."
                    }));
                }
                array_index(key).and_then(|index| values.get(index as usize))
            }
            _ => value.get_key(key),
        }
        .ok_or_else(|| registry("Unresolved schema reference."))?;
    }
    if !matches!(value, IvyJson::Bool(_) | IvyJson::Object(_)) {
        return Err(registry("Reference must address a schema."));
    }
    Ok(value)
}

/// The returned unique names retain JavaScript Set insertion order.
/// Callers pass an admitted schema; this function does not validate its definition.
pub fn scalar_types(schema: &IvyJson, pointer: &JsString) -> IvyResult<Vec<String>> {
    let parts = pointer_parts(pointer)?;
    scalar_walk(schema, schema, &parts, 0, 0, false)
}

fn scalar_walk(
    root: &IvyJson,
    node: &IvyJson,
    parts: &[JsString],
    index: usize,
    depth: usize,
    omit_all_of: bool,
) -> IvyResult<Vec<String>> {
    if depth > 64 {
        return Err(IvyError::new(
            "invalid_arguments",
            "Queryable schema path is too complex.",
        ));
    }
    if matches!(node, IvyJson::Bool(_)) {
        return Ok(Vec::new());
    }
    if let Some(reference) = node.get("$ref").and_then(IvyJson::as_js_string) {
        return scalar_walk(
            root,
            resolve(root, reference)?,
            parts,
            index,
            depth + 1,
            false,
        );
    }
    for keyword in ["anyOf", "oneOf"] {
        if let Some(children) = node.get(keyword).and_then(IvyJson::as_array) {
            let mut union = Vec::new();
            for child in children {
                for kind in scalar_walk(root, child, parts, index, depth + 1, false)? {
                    if !union.contains(&kind) {
                        union.push(kind);
                    }
                }
            }
            return Ok(union);
        }
    }
    if !omit_all_of {
        if let Some(children) = node.get("allOf").and_then(IvyJson::as_array) {
            let mut intersection: Option<Vec<String>> = None;
            for (child, omit) in
                std::iter::once((node, true)).chain(children.iter().map(|child| (child, false)))
            {
                let types = scalar_walk(root, child, parts, index, depth + 1, omit)?;
                if types.is_empty() {
                    continue;
                }
                if let Some(prior) = &mut intersection {
                    let subtype = (has_type(prior, "number") && has_type(&types, "integer"))
                        || (has_type(prior, "integer") && has_type(&types, "number"));
                    prior.retain(|kind| types.contains(kind));
                    if subtype && !has_type(prior, "integer") {
                        prior.push("integer".into());
                    }
                } else {
                    intersection = Some(types);
                }
            }
            return Ok(intersection.unwrap_or_default());
        }
    }
    if index == parts.len() {
        let mut kinds = Vec::new();
        match node.get("type") {
            Some(IvyJson::String(kind)) => kinds.push(kind.clone()),
            Some(IvyJson::Array(types)) => {
                kinds.extend(types.iter().filter_map(IvyJson::as_js_string).cloned());
            }
            _ => {
                let literals: Vec<&IvyJson> = if let Some(value) = node.get("const") {
                    vec![value]
                } else {
                    node.get("enum")
                        .and_then(IvyJson::as_array)
                        .unwrap_or(&[])
                        .iter()
                        .collect()
                };
                for literal in literals {
                    kinds.push(JsString::from(match literal {
                        IvyJson::Null => "null",
                        IvyJson::Bool(_) => "boolean",
                        IvyJson::Number(n) if n.is_finite() && n.fract() == 0.0 => "integer",
                        IvyJson::Number(_) => "number",
                        IvyJson::String(_) => "string",
                        IvyJson::Array(_) => "array",
                        IvyJson::Object(_) => "object",
                    }));
                }
            }
        }
        if kinds
            .iter()
            .any(|kind| kind == &JsString::from("object") || kind == &JsString::from("array"))
        {
            return Ok(Vec::new());
        }
        let mut result = Vec::new();
        for kind in kinds {
            if let Ok(kind) = kind.to_utf8() {
                if ["null", "boolean", "integer", "number", "string"].contains(&kind.as_str())
                    && !result.contains(&kind)
                {
                    result.push(kind);
                }
            }
        }
        return Ok(result);
    }
    if let Some(child) = node
        .get("properties")
        .and_then(|properties| properties.get_key(&parts[index]))
    {
        return scalar_walk(root, child, parts, index + 1, depth + 1, false);
    }
    let key = &parts[index].0;
    let numeric = key == &[48]
        || (key.first().is_some_and(|unit| (49..=57).contains(unit))
            && key.iter().all(|unit| (48..=57).contains(unit)));
    if node.get("type").and_then(IvyJson::as_js_string) == Some(&JsString::from("array")) && numeric
    {
        if let Some(items) = node.get("items") {
            return scalar_walk(root, items, parts, index + 1, depth + 1, false);
        }
    }
    Ok(Vec::new())
}
fn has_type(types: &[String], kind: &str) -> bool {
    types.iter().any(|candidate| candidate == kind)
}

fn entries(value: &IvyJson) -> Vec<(&JsString, &IvyJson)> {
    let mut result: Vec<_> = value
        .as_object()
        .unwrap_or(&[])
        .iter()
        .map(|(key, value)| (key, value))
        .collect();
    result.sort_by_key(|(key, _)| array_index(key).map_or((1, 0), |index| (0, index)));
    result
}

/// Clone only reachable local definitions. Literal/default data is never interpreted as schema.
pub fn bundle_local_schema(document: &IvyJson, name: &JsString) -> IvyResult<IvyJson> {
    enum Work<'a> {
        Include(JsString),
        Visit(&'a IvyJson),
        Field(&'a JsString, &'a IvyJson),
    }
    let mut pending = vec![Work::Include(name.clone())];
    let mut included = HashSet::new();
    let mut definitions = IvyJson::object(Vec::<(JsString, IvyJson)>::new());
    while let Some(work) = pending.pop() {
        match work {
            Work::Include(key) => {
                if !included.insert(key.clone()) {
                    continue;
                }
                let value = document
                    .get("$defs")
                    .and_then(|defs| defs.get_key(&key))
                    .ok_or_else(|| registry("Missing bundled schema definition."))?;
                definitions.insert(key, value.clone());
                pending.push(Work::Visit(value));
            }
            Work::Visit(value) => match value {
                IvyJson::Array(values) => pending.extend(values.iter().rev().map(Work::Visit)),
                IvyJson::Object(_) => pending.extend(
                    entries(value)
                        .into_iter()
                        .rev()
                        .map(|(key, value)| Work::Field(key, value)),
                ),
                _ => {}
            },
            Work::Field(key, child) => {
                if key == &JsString::from("$ref") {
                    let reference = child.as_js_string().ok_or_else(|| {
                        registry("Only local definition references may be bundled.")
                    })?;
                    static LOCAL_REFERENCE: OnceLock<regress::Regex> = OnceLock::new();
                    let pattern = LOCAL_REFERENCE.get_or_init(|| {
                        regress::Regex::new(r"^#\/\$defs\/[^/]+(?:\/.*)?$")
                            .expect("fixed local-reference expression")
                    });
                    if pattern.find_from_utf16(&reference.0, 0).next().is_none() {
                        return Err(registry("Only local definition references may be bundled."));
                    }
                    let prefix = JsString::from("#/$defs/");
                    let suffix = reference
                        .0
                        .strip_prefix(prefix.0.as_slice())
                        .filter(|suffix| !suffix.is_empty() && suffix[0] != 47)
                        .ok_or_else(|| {
                            registry("Only local definition references may be bundled.")
                        })?;
                    // The TS bundler deliberately only decodes the definition-name
                    // segment. Admission later checks pointer escapes and the target.
                    let end = suffix
                        .iter()
                        .position(|unit| *unit == 47)
                        .unwrap_or(suffix.len());
                    let mut decoded = Vec::new();
                    let mut index = 0;
                    while index < end {
                        if suffix[index] == 126
                            && index + 1 < end
                            && [48, 49].contains(&suffix[index + 1])
                        {
                            decoded.push(if suffix[index + 1] == 49 { 47 } else { 126 });
                            index += 2;
                        } else {
                            decoded.push(suffix[index]);
                            index += 1;
                        }
                    }
                    pending.push(Work::Include(JsString(decoded)));
                } else if [JsString::from("properties"), JsString::from("$defs")].contains(key) {
                    match child {
                        IvyJson::Array(values) => {
                            pending.extend(values.iter().rev().map(Work::Visit))
                        }
                        IvyJson::Object(_) => pending.extend(
                            entries(child)
                                .into_iter()
                                .rev()
                                .map(|(_, value)| Work::Visit(value)),
                        ),
                        _ => {}
                    }
                } else if [
                    "items",
                    "additionalProperties",
                    "allOf",
                    "anyOf",
                    "oneOf",
                    "if",
                    "then",
                    "else",
                    "not",
                ]
                .iter()
                .any(|name| key == &JsString::from(*name))
                {
                    pending.push(Work::Visit(child));
                }
            }
        }
    }
    let mut result = IvyJson::object(Vec::<(JsString, IvyJson)>::new());
    if let Some(schema) = document.get("$schema") {
        if schema
            .as_js_string()
            .is_some_and(|value| !value.0.is_empty())
        {
            result.insert("$schema", schema.clone());
        }
    }
    let mut reference = JsString::from("#/$defs/");
    for unit in &name.0 {
        match unit {
            126 => reference.0.extend([126, 48]),
            47 => reference.0.extend([126, 49]),
            _ => reference.0.push(*unit),
        }
    }
    result.insert("$ref", IvyJson::String(reference));
    result.insert("$defs", definitions);
    Ok(result)
}
