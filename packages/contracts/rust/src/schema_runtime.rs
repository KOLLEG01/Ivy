//! Submitted schema compilation and bounded validator ownership. Pattern matching
//! deliberately uses the admitted RE2 engine, separately from built-in schemas.
use super::{admit_schema, compiler_plan, entries};
use crate::{
    IvyError, IvyJson, IvyResult, JsString, hash::hash_json, json::canonical, re2::Re2Pattern,
};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};
#[path = "schema_runtime_eval.rs"]
mod evaluation;

#[derive(Default)]
struct Cache {
    validators: HashMap<String, Arc<CompiledSchema>>,
    compilations: usize,
}

/// Content-keyed validators for the lossless JSON domain. Retained compiled
/// handles remain usable after a batch is evicted. Failed compiler attempts count
/// toward the same 256-attempt batch; failed admission does not.
#[derive(Default)]
pub struct SchemaValidators {
    cache: Mutex<Cache>,
}
impl SchemaValidators {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn compile(&self, schema: &IvyJson) -> IvyResult<Arc<CompiledSchema>> {
        let identity = hash_json(schema)?;
        let mut cache = self.cache.lock().map_err(|_| IvyError::internal())?;
        if let Some(existing) = cache.validators.get(&identity) {
            return Ok(existing.clone());
        }
        let admitted = admit_schema(schema)?;
        if cache.compilations == 256 {
            cache.validators.clear();
            cache.compilations = 0;
        }
        cache.compilations += 1;
        // Registration precedes meta-validation in the original compiler. Both
        // use registry_invalid, while admission keeps its own precise errors.
        admitted.registered_resources()?;
        admitted.validate_meta_schema()?;
        let validator = Arc::new(compile(admitted.schema())?);
        cache.validators.insert(identity, validator.clone());
        Ok(validator)
    }

    pub fn validate(
        &self,
        schema: &IvyJson,
        value: &IvyJson,
        maximum_bytes: usize,
    ) -> IvyResult<()> {
        canonical(value, maximum_bytes)?;
        if self.compile(schema)?.matches(value)? {
            Ok(())
        } else {
            Err(IvyError::new(
                "invalid_arguments",
                "Content does not satisfy its exact contract.",
            ))
        }
    }
}

pub struct CompiledSchema {
    nodes: Vec<Node>,
    root: usize,
    patterns: HashMap<JsString, Re2Pattern>,
    dynamic_targets: std::collections::HashSet<(usize, usize)>,
}
struct Node {
    schema: IvyJson,
    children: HashMap<&'static str, Vec<(JsString, usize)>>,
    additional_names: Vec<JsString>,
    reference: Option<usize>,
    static_all: u8,
}
impl Node {
    fn children(&self, keyword: &str) -> &[(JsString, usize)] {
        self.children.get(keyword).map(Vec::as_slice).unwrap_or(&[])
    }
    fn child(&self, keyword: &str) -> Option<usize> {
        self.children(keyword).first().map(|(_, id)| *id)
    }
}
impl CompiledSchema {
    /// Invoke the compiled predicate. Schema validation and value byte limits
    /// belong to SchemaValidators::validate, just as they do at the JS wrapper.
    pub fn matches(&self, value: &IvyJson) -> IvyResult<bool> {
        self.evaluate(value)
    }
}
fn invalid() -> IvyError {
    IvyError::new("registry_invalid", "Invalid or unsupported JSON schema.")
}
fn compile(schema: &IvyJson) -> IvyResult<CompiledSchema> {
    let plan = compiler_plan::plan(schema)?;
    let constant = |accept| Node {
        schema: IvyJson::Bool(accept),
        children: HashMap::new(),
        additional_names: Vec::new(),
        reference: None,
        static_all: 0,
    };
    let mut nodes = vec![constant(true), constant(false)];
    let mut locations = Vec::new();
    let mut indices = HashMap::new();
    for (unit, target) in plan.units.iter().enumerate() {
        for node in &target.nodes {
            let key = (unit, node.schema as *const IvyJson as usize);
            if indices.contains_key(&key) {
                continue;
            }
            indices.insert(key, nodes.len());
            locations.push((unit, node.schema));
            nodes.push(constant(true));
        }
    }
    let id = |unit, schema: &IvyJson| match schema {
        IvyJson::Bool(true) => 0,
        IvyJson::Bool(false) => 1,
        _ => *indices
            .get(&(unit, schema as *const IvyJson as usize))
            .unwrap_or(&0),
    };
    let roots: Vec<usize> = plan
        .units
        .iter()
        .enumerate()
        .map(|(unit, target)| id(unit, target.target.schema))
        .collect();
    let mut patterns = HashMap::new();
    for (offset, (unit, source)) in locations.iter().enumerate() {
        let index = offset + 2;
        let mut children = HashMap::new();
        for keyword in [
            "properties",
            "patternProperties",
            "dependentSchemas",
            "dependencies",
        ] {
            if let Some(map) = source.get(keyword) {
                let rows = entries(map)
                    .into_iter()
                    .filter(|(key, _)| *key != &JsString::from("__proto__"))
                    .filter(|(_, child)| keyword != "dependencies" || child.as_array().is_none())
                    .map(|(key, child)| (key.clone(), id(*unit, child)))
                    .collect::<Vec<_>>();
                children.insert(keyword, rows);
            }
        }
        for keyword in ["anyOf", "oneOf", "allOf", "prefixItems"] {
            if let Some(items) = source.get(keyword).and_then(IvyJson::as_array) {
                children.insert(
                    keyword,
                    items
                        .iter()
                        .enumerate()
                        .map(|(i, child)| (i.to_string().into(), id(*unit, child)))
                        .collect(),
                );
            }
        }
        for keyword in [
            "not",
            "if",
            "then",
            "else",
            "items",
            "contains",
            "additionalProperties",
            "propertyNames",
            "unevaluatedProperties",
            "unevaluatedItems",
        ] {
            if let Some(child) = source.get(keyword) {
                children.insert(keyword, vec![(JsString::default(), id(*unit, child))]);
            }
        }
        let reference = plan
            .references
            .iter()
            .find(|reference| {
                reference.from_unit == *unit
                    && std::ptr::eq(plan.units[*unit].nodes[reference.from_node].schema, *source)
            })
            .map(|r| roots[r.to_unit]);
        // Structural children and annotation payloads are represented by graph
        // edges, never copied at each ancestor of a recursive/large schema.
        let fields = entries(source)
            .into_iter()
            .filter(|(key, _)| {
                [
                    "type",
                    "nullable",
                    "const",
                    "enum",
                    "required",
                    "dependentRequired",
                    "dependencies",
                    "minimum",
                    "maximum",
                    "exclusiveMinimum",
                    "exclusiveMaximum",
                    "multipleOf",
                    "minLength",
                    "maxLength",
                    "pattern",
                    "format",
                    "minItems",
                    "maxItems",
                    "uniqueItems",
                    "minContains",
                    "maxContains",
                    "minProperties",
                    "maxProperties",
                    "$dynamicAnchor",
                    "$dynamicRef",
                    "$recursiveAnchor",
                    "$recursiveRef",
                ]
                .iter()
                .any(|name| *key == &JsString::from(*name))
            })
            .map(|(key, value)| (key.clone(), value.clone()));
        let mut static_all = 0;
        if source.get("additionalProperties").is_some()
            || source.get("unevaluatedProperties").is_some()
        {
            static_all |= 1;
        }
        if source.get("items").is_some()
            || source.get("unevaluatedItems").is_some()
            || source.get("contains").is_some_and(|child| {
                !matches!(child, IvyJson::Bool(true))
                    && child.as_object().is_none_or(|items| !items.is_empty())
            }) && !(source.get("minContains").and_then(IvyJson::as_f64) == Some(0.0)
                && source.get("maxContains").is_none())
        {
            static_all |= 2;
        }
        let declared = children.get("properties").map(Vec::as_slice).unwrap_or(&[]);
        // For more than eight declarations the original additional-property
        // check uses hasOwnProperty on the unfiltered submitted map.
        let additional_names = if declared.len() > 8 {
            source
                .get("properties")
                .map(entries)
                .unwrap_or_default()
                .into_iter()
                .map(|(key, _)| key.clone())
                .collect()
        } else {
            declared.iter().map(|(key, _)| key.clone()).collect()
        };
        nodes[index] = Node {
            schema: IvyJson::object(fields),
            children,
            additional_names,
            reference,
            static_all,
        };
    }
    // Propagate unconditional all-properties/items metadata through refs/allOf.
    // A worklist bounds this to two bit additions per node, including cycles.
    let mut parents = vec![Vec::new(); nodes.len()];
    for (parent, node) in nodes.iter().enumerate() {
        for child in node.reference.into_iter().chain(
            ["allOf"]
                .into_iter()
                .flat_map(|key| node.children(key).iter().map(|(_, id)| *id)),
        ) {
            parents[child].push(parent);
        }
    }
    let mut pending = (0..nodes.len()).collect::<Vec<_>>();
    while let Some(child) = pending.pop() {
        for parent in &parents[child] {
            let flags = nodes[*parent].static_all | nodes[child].static_all;
            if flags != nodes[*parent].static_all {
                nodes[*parent].static_all = flags;
                pending.push(*parent);
            }
        }
    }
    for index in 0..nodes.len() {
        let all_patterns_valid = nodes[index]
            .children("patternProperties")
            .iter()
            .all(|(_, id)| *id == 0);
        let prior_all = nodes[index]
            .reference
            .is_some_and(|id| nodes[id].static_all & 1 != 0);
        let additional = nodes[index].child("additionalProperties");
        let skip_patterns =
            all_patterns_valid && (additional == Some(0) || (additional.is_none() && prior_all));
        if skip_patterns {
            if let Some(children) = nodes[index].children.get_mut("patternProperties") {
                children.clear();
            }
        }
        let selected = nodes[index]
            .schema
            .get("pattern")
            .and_then(IvyJson::as_js_string)
            .into_iter()
            .chain(
                nodes[index]
                    .children("patternProperties")
                    .iter()
                    .map(|(key, _)| key),
            );
        for pattern in selected {
            if !patterns.contains_key(pattern) {
                patterns.insert(
                    pattern.clone(),
                    Re2Pattern::compile(pattern).map_err(|_| invalid())?,
                );
            }
        }
    }
    let dynamic_targets = dynamic_targets(&nodes, roots[0]);
    Ok(CompiledSchema {
        nodes,
        root: roots[0],
        patterns,
        dynamic_targets,
    })
}

// A dynamic reference only consults the runtime anchor table if its name was
// registered when this particular validator function was compiled. Inline
// anchors first compile their callable entry, before the surrounding function
// resumes. Preserve that ordering without generating or executing JavaScript.
fn dynamic_targets(nodes: &[Node], root: usize) -> std::collections::HashSet<(usize, usize)> {
    use std::collections::HashSet;
    let mut targets = HashSet::new();
    let mut anchors = HashSet::new();
    let mut functions = HashSet::from([root]);
    let mut visited = HashSet::new();
    let mut pending = vec![(root, root, false)];
    while let Some((id, entry, resumed)) = pending.pop() {
        let node = &nodes[id];
        if !resumed {
            if !visited.insert((id, entry)) {
                continue;
            }
            let anchor = node
                .schema
                .get("$dynamicAnchor")
                .and_then(IvyJson::as_js_string)
                .cloned()
                .or_else(|| {
                    (node
                        .schema
                        .get("$recursiveAnchor")
                        .and_then(IvyJson::as_bool)
                        == Some(true))
                    .then(JsString::default)
                });
            if let Some(anchor) = anchor {
                anchors.insert(anchor);
                if id != entry && functions.insert(id) {
                    pending.push((id, entry, true));
                    pending.push((id, id, false));
                    continue;
                }
            }
        }
        if let Some(reference) = node
            .schema
            .get("$dynamicRef")
            .or_else(|| node.schema.get("$recursiveRef"))
            .and_then(IvyJson::as_js_string)
        {
            let anchor = JsString(reference.0[1..].to_vec());
            if anchors.contains(&anchor) || evaluation::inherited(&anchor) {
                targets.insert((id, entry));
            }
        }
        let mut children = Vec::new();
        if let Some(reference) = node.reference {
            if functions.insert(reference) {
                children.push((reference, reference, false));
            }
        }
        for keyword in [
            "not",
            "anyOf",
            "oneOf",
            "allOf",
            "if",
            "then",
            "else",
            "prefixItems",
            "items",
            "contains",
            "propertyNames",
            "additionalProperties",
            "dependencies",
            "properties",
            "patternProperties",
            "dependentRequired",
            "dependentSchemas",
            "unevaluatedProperties",
            "unevaluatedItems",
        ] {
            children.extend(
                node.children(keyword)
                    .iter()
                    .map(|(_, child)| (*child, entry, false)),
            );
        }
        pending.extend(children.into_iter().rev());
    }
    targets
}

/// The original compiler uses fast-deep-equal, whose constructor/coercion checks
/// differ observably from structural JSON equality for own shadow properties.
fn equal(left: &IvyJson, right: &IvyJson) -> IvyResult<bool> {
    fn strict(left: &IvyJson, right: &IvyJson) -> bool {
        match (left, right) {
            (IvyJson::Object(_) | IvyJson::Array(_), _)
            | (_, IvyJson::Object(_) | IvyJson::Array(_)) => std::ptr::eq(left, right),
            _ => left == right,
        }
    }
    let mut pending = vec![(left, right)];
    while let Some((left, right)) = pending.pop() {
        if strict(left, right) {
            continue;
        }
        match (left, right) {
            (IvyJson::Array(a), IvyJson::Array(b)) => {
                if a.len() != b.len() {
                    return Ok(false);
                }
                pending.extend(a.iter().zip(b));
            }
            (IvyJson::Object(a), IvyJson::Object(b)) => {
                match (left.get("constructor"), right.get("constructor")) {
                    (Some(a), Some(b)) if strict(a, b) => {}
                    (None, None) => {}
                    _ => return Ok(false),
                }
                if left.get("valueOf").is_some() || left.get("toString").is_some() {
                    return Err(IvyError::internal());
                }
                if a.len() != b.len() {
                    return Ok(false);
                }
                let keys = entries(left);
                for (key, _) in keys.iter().rev() {
                    if right.get_key(key).is_none() {
                        return Ok(false);
                    }
                }
                for (key, child) in keys {
                    pending.push((child, right.get_key(key).unwrap()));
                }
            }
            _ => return Ok(false),
        }
    }
    Ok(true)
}

fn type_matches(schema: &IvyJson, value: &IvyJson) -> bool {
    let Some(types) = schema
        .get("type")
        .filter(|types| types != &&IvyJson::String(JsString::default()))
    else {
        return true;
    };
    let matches = |kind: &IvyJson| match kind.as_str().ok().as_deref() {
        Some("null") => value.is_null(),
        Some("boolean") => value.as_bool().is_some(),
        Some("number") => value.as_f64().is_some(),
        Some("integer") => value.as_f64().is_some_and(|n| n.fract() == 0.0),
        Some("string") => value.as_js_string().is_some(),
        Some("array") => value.as_array().is_some(),
        Some("object") => value.as_object().is_some(),
        _ => false,
    };
    if value.is_null() && schema.get("nullable").and_then(IvyJson::as_bool) == Some(true) {
        return true;
    }
    types.as_array().map_or_else(
        || matches(types),
        |types| types.is_empty() || types.iter().any(matches),
    )
}
