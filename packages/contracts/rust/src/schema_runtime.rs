//! Submitted schema compilation and bounded validator ownership. Pattern matching
//! deliberately uses the admitted RE2 engine, separately from built-in schemas.
use super::{admit_schema, compiler_plan, entries};
use crate::{
    IvyError, IvyJson, IvyResult, JsString, builtin_schema::json_equal, hash::hash_json,
    json::canonical, re2::Re2Pattern,
};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};
#[path = "schema_runtime_eval.rs"]
mod evaluation;

#[derive(Default)]
struct Cache {
    validators: Vec<CachedValidator>,
    bytes: usize,
}
struct CachedValidator {
    identity: String,
    validator: Arc<CompiledSchema>,
}
const CACHE_BYTES: usize = 8 * 1024 * 1024;
const ENTRY_BYTES: usize = 2 * 1024 * 1024;

/// Content-keyed validators for the lossless JSON domain. Retained compiled
/// handles remain usable after byte-budget eviction. The budget covers cache
/// ownership; callers can retain handles independently. Failed compiles retain
/// nothing and do not evict valid entries.
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
        let mut position = match cache
            .validators
            .binary_search_by(|entry| entry.identity.cmp(&identity))
        {
            Ok(position) => return Ok(cache.validators[position].validator.clone()),
            Err(position) => position,
        };
        let admitted = admit_schema(schema)?;
        // Registration precedes meta-validation in the original compiler. Both
        // use registry_invalid, while admission keeps its own precise errors.
        admitted.registered_resources()?;
        admitted.validate_meta_schema()?;
        let validator = Arc::new(compile(admitted.schema())?);
        let bytes =
            identity.capacity() + validator.retained_bytes() + 2 * std::mem::size_of::<usize>(); // Arc's strong/weak counters.
        if bytes + std::mem::size_of::<CachedValidator>() <= ENTRY_BYTES {
            cache.validators.reserve(1);
            if cache.bytes
                + bytes
                + cache.validators.capacity() * std::mem::size_of::<CachedValidator>()
                > CACHE_BYTES
            {
                // Drop the backing allocation too, rather than retaining its
                // high-water capacity after clearing a batch.
                *cache = Cache::default();
                cache.validators.reserve(1);
                position = 0;
            }
            cache.bytes += bytes;
            cache.validators.insert(
                position,
                CachedValidator {
                    identity,
                    validator: validator.clone(),
                },
            );
        }
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
    patterns: Vec<(JsString, Re2Pattern)>,
    dynamic_targets: Vec<(usize, usize)>,
}
struct Node {
    schema: IvyJson,
    children: Vec<(&'static str, Vec<(JsString, usize)>)>,
    reference: Option<usize>,
    static_all: u8,
}
impl Node {
    fn child_position(&self, keyword: &str) -> Option<usize> {
        self.children
            .binary_search_by_key(&keyword, |(key, _)| *key)
            .ok()
    }
    fn has_children(&self, keyword: &str) -> bool {
        self.child_position(keyword).is_some()
    }
    fn children(&self, keyword: &str) -> &[(JsString, usize)] {
        self.child_position(keyword)
            .map(|position| self.children[position].1.as_slice())
            .unwrap_or(&[])
    }
    fn child(&self, keyword: &str) -> Option<usize> {
        self.children(keyword).first().map(|(_, id)| *id)
    }
}
impl CompiledSchema {
    /// Owned allocations, including container capacities and shared RE2 classes
    /// once per allocation. Excludes an enclosing Arc and evaluation workspaces.
    pub fn retained_bytes(&self) -> usize {
        use std::mem::size_of;
        let mut bytes = size_of::<Self>()
            + self.nodes.capacity() * size_of::<Node>()
            + self.patterns.capacity() * size_of::<(JsString, Re2Pattern)>()
            + self.dynamic_targets.capacity() * size_of::<(usize, usize)>();
        for node in &self.nodes {
            bytes += node.schema.retained_bytes() - size_of::<IvyJson>()
                + node.children.capacity() * size_of::<(&str, Vec<(JsString, usize)>)>();
            for (_, rows) in &node.children {
                bytes += rows.capacity() * size_of::<(JsString, usize)>();
                bytes += rows
                    .iter()
                    .map(|(key, _)| key.0.capacity() * size_of::<u16>())
                    .sum::<usize>();
            }
        }
        for (text, pattern) in &self.patterns {
            bytes += text.0.capacity() * size_of::<u16>() + pattern.retained_bytes()
                - size_of::<Re2Pattern>();
        }
        bytes
    }
    fn pattern(&self, pattern: &JsString) -> &Re2Pattern {
        &self.patterns[self
            .patterns
            .binary_search_by(|(key, _)| key.cmp(pattern))
            .expect("compiled pattern")]
        .1
    }
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
        children: Vec::new(),
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
            .map(|(key, value)| {
                let value = if key == &JsString::from("dependencies") {
                    IvyJson::object(
                        entries(value)
                            .into_iter()
                            .filter(|(_, value)| value.as_array().is_some())
                            .map(|(key, value)| (key.clone(), value.clone())),
                    )
                } else {
                    value.clone()
                };
                (key.clone(), value)
            });
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
        nodes[index] = Node {
            schema: IvyJson::object(fields),
            children: {
                let mut children = children.into_iter().collect::<Vec<_>>();
                children.sort_unstable_by_key(|(key, _)| *key);
                children
            },
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
            if let Some(position) = nodes[index].child_position("patternProperties") {
                nodes[index].children[position].1 = Vec::new();
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
    // Mutable lookup tables are compilation workspaces. Retain compact sorted
    // arrays so their allocation capacities can be budgeted directly.
    let mut patterns = patterns.into_iter().collect::<Vec<_>>();
    patterns.sort_unstable_by(|(a, _), (b, _)| a.cmp(b));
    let mut dynamic_targets = dynamic_targets(&nodes, roots[0])
        .into_iter()
        .collect::<Vec<_>>();
    dynamic_targets.sort_unstable();
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
            if anchors.contains(&anchor) {
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

fn type_matches(schema: &IvyJson, value: &IvyJson) -> bool {
    let Some(types) = schema.get("type") else {
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
    types
        .as_array()
        .map_or_else(|| matches(types), |types| types.iter().any(matches))
}
