use super::entries;
use crate::{IvyError, IvyJson, IvyResult, JsString};
use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

/// One object visited by the pinned compiler's all-keys schema traversal.
pub struct SchemaResourceNode<'a> {
    pub pointer: JsString,
    pub parent: Option<usize>,
    pub schema: &'a IvyJson,
}

pub struct SchemaResourceGraph<'a> {
    /// Any own $id, even inside ignored annotation values, isolates the compiler.
    pub isolated: bool,
    /// Preorder locations. A child's parent is the containing schema, not its map.
    pub nodes: Vec<SchemaResourceNode<'a>>,
}

pub struct SchemaResourceIndex<'a> {
    pub graph: SchemaResourceGraph<'a>,
    pub base_id: JsString,
    pub path_prefix: JsString,
    /// Effective base ID for each preorder node in graph.nodes.
    pub scopes: Vec<JsString>,
    /// Declarations in traversal order, including declarations at their own URI.
    pub references: Vec<SchemaResourceReference>,
    /// Set by compiler registration when the root is retained under base_id.
    pub registered_root: bool,
}

pub struct SchemaResourceReference {
    pub id: JsString,
    pub node: usize,
    pub full_path: JsString,
    /// Whether this declaration installs an alias, after registration checks.
    pub alias: bool,
}

pub(super) fn index(schema: &IvyJson) -> IvyResult<SchemaResourceIndex<'_>> {
    use super::uri::{full_path, normalize_id, resolve};
    let invalid = || IvyError::new("registry_invalid", "Invalid or unsupported JSON schema.");
    let graph = collect(schema);
    let base_id = match schema.get("$id") {
        Some(IvyJson::String(id)) => normalize_id(id),
        None | Some(IvyJson::Null | IvyJson::Bool(false)) => JsString::default(),
        Some(IvyJson::Number(n)) if *n == 0.0 => JsString::default(),
        _ => return Err(invalid()),
    };
    let path_prefix = full_path(&base_id)?;
    let mut bases = HashMap::from([(JsString::default(), base_id.clone())]);
    let mut scopes = Vec::new();
    let mut references = Vec::new();
    let mut seen = HashSet::new();
    for (node, location) in graph.nodes.iter().enumerate() {
        let Some(parent) = location.parent else {
            scopes.push(base_id.clone());
            continue;
        };
        // The original traversal stores scopes by emitted pointer. Annotation
        // keys can collide with structural paths, so parent indices alone are
        // insufficient to reproduce which scope a later node inherits.
        let mut scope = bases[&graph.nodes[parent].pointer].clone();
        let mut path = path_prefix.clone();
        path.0.extend(&location.pointer.0);
        let mut declare = |id: &JsString, scope: &JsString| -> IvyResult<JsString> {
            let id = normalize_id(&if scope.0.is_empty() {
                id.clone()
            } else {
                resolve(scope, id)?
            });
            if !seen.insert(id.clone()) {
                return Err(invalid());
            }
            references.push(SchemaResourceReference {
                alias: id != normalize_id(&path),
                id: id.clone(),
                node,
                full_path: path.clone(),
            });
            Ok(id)
        };
        if let Some(IvyJson::String(id)) = location.schema.get("$id") {
            scope = declare(id, &scope)?;
        }
        for keyword in ["$anchor", "$dynamicAnchor"] {
            if let Some(IvyJson::String(anchor)) = location.schema.get(keyword) {
                if !anchor
                    .0
                    .first()
                    .is_some_and(|c| matches!(c,65..=90|97..=122|95))
                    || !anchor
                        .0
                        .iter()
                        .all(|c| matches!(c,65..=90|97..=122|48..=57|95|45|46))
                {
                    return Err(invalid());
                }
                let mut id = JsString::from("#");
                id.0.extend(&anchor.0);
                declare(&id, &scope)?;
            }
        }
        bases.insert(location.pointer.clone(), scope.clone());
        scopes.push(scope);
    }
    Ok(SchemaResourceIndex {
        graph,
        base_id,
        path_prefix,
        scopes,
        references,
        registered_root: false,
    })
}

/// Register against a fresh compiler's meta-schemas and ordinary-object names.
/// Explicit IDs select an isolated compiler; anonymous roots stay unregistered.
pub(super) fn register(schema: &IvyJson) -> IvyResult<SchemaResourceIndex<'_>> {
    enum Registration<'a> {
        Schema(&'a IvyJson),
        Alias(JsString),
        Prototype,
        Function,
    }
    fn lookup<'a, 'b>(
        refs: &'b HashMap<JsString, Registration<'a>>,
        id: &JsString,
    ) -> Option<&'b Registration<'a>> {
        refs.get(id).or_else(|| {
            if id == &JsString::from("__proto__") {
                Some(&Registration::Prototype)
            } else if inherited(id) {
                Some(&Registration::Function)
            } else {
                None
            }
        })
    }
    static DOCUMENTS: OnceLock<IvyResult<IvyJson>> = OnceLock::new();
    let data = DOCUMENTS
        .get_or_init(|| crate::json::parse(include_str!("schema-meta-data.json"), 1024 * 1024))
        .as_ref()
        .map_err(Clone::clone)?;
    let documents = data
        .get("schemas")
        .and_then(IvyJson::as_array)
        .ok_or_else(IvyError::internal)?;
    let mut refs = HashMap::new();
    for document in documents {
        let id = document
            .get("$id")
            .and_then(IvyJson::as_js_string)
            .ok_or_else(IvyError::internal)?;
        refs.insert(id.clone(), Registration::Schema(document));
    }
    refs.insert(
        "http://json-schema.org/schema".into(),
        Registration::Alias("https://json-schema.org/draft/2020-12/schema".into()),
    );
    let invalid = || IvyError::new("registry_invalid", "Invalid or unsupported JSON schema.");
    let mut index = index(schema)?;
    for reference in &mut index.references {
        let existing = match lookup(&refs, &reference.id) {
            Some(Registration::Alias(target)) => lookup(&refs, target),
            other => other,
        };
        match existing {
            Some(Registration::Schema(registered)) => {
                if !crate::builtin_schema::json_equal(
                    index.graph.nodes[reference.node].schema,
                    registered,
                ) {
                    return Err(invalid());
                }
                reference.alias = false;
            }
            // Object.prototype has no schema member, so the compiler treats it
            // as an existing object without a conflicting schema or new alias.
            Some(Registration::Prototype) => reference.alias = false,
            _ if reference.alias && reference.id.0.first() != Some(&35) => {
                refs.insert(
                    reference.id.clone(),
                    Registration::Alias(reference.full_path.clone()),
                );
            }
            _ => {}
        }
    }
    if index.graph.isolated && index.base_id.0.first() != Some(&35) {
        if !index.base_id.0.is_empty() && lookup(&refs, &index.base_id).is_some() {
            return Err(invalid());
        }
        index.registered_root = true;
        // An empty root ID overwrites an earlier empty-ID child alias.
        for reference in &mut index.references {
            if reference.id == index.base_id {
                reference.alias = false;
            }
        }
    }
    Ok(index)
}

fn contains_id(schema: &IvyJson) -> bool {
    let mut pending = vec![schema];
    while let Some(value) = pending.pop() {
        match value {
            IvyJson::Object(values) => {
                if value.get("$id").is_some() {
                    return true;
                }
                pending.extend(values.iter().map(|(_, child)| child));
            }
            IvyJson::Array(values) => pending.extend(values),
            _ => {}
        }
    }
    false
}

fn member(key: &JsString, names: &[&str]) -> bool {
    names.iter().any(|name| key == &JsString::from(*name))
}

// The original traversal uses `key in keywordTable` on ordinary objects.
// Annotation objects may contain these inherited names despite schema admission.
fn inherited(key: &JsString) -> bool {
    member(
        key,
        &[
            "constructor",
            "__defineGetter__",
            "__defineSetter__",
            "hasOwnProperty",
            "__lookupGetter__",
            "__lookupSetter__",
            "isPrototypeOf",
            "propertyIsEnumerable",
            "toString",
            "valueOf",
            "__proto__",
            "toLocaleString",
        ],
    )
}

pub(super) fn append(pointer: &JsString, key: &JsString, escape: bool) -> JsString {
    let mut result = pointer.clone();
    result.0.push(47);
    for unit in &key.0 {
        match (*unit, escape) {
            (126, true) => result.0.extend([126, 48]),
            (47, true) => result.0.extend([126, 49]),
            _ => result.0.push(*unit),
        }
    }
    result
}

pub(super) fn collect(schema: &IvyJson) -> SchemaResourceGraph<'_> {
    let mut nodes = Vec::new();
    let mut pending = vec![(schema, JsString::default(), None)];
    while let Some((schema, pointer, parent)) = pending.pop() {
        if schema.as_object().is_none() {
            continue;
        }
        let index = nodes.len();
        for (key, value) in entries(schema).into_iter().rev() {
            let path = append(&pointer, key, false);
            if let Some(values) = value.as_array() {
                if inherited(key) || member(key, &["items", "allOf", "anyOf", "oneOf"]) {
                    for (i, child) in values.iter().enumerate().rev() {
                        pending.push((
                            child,
                            append(&path, &i.to_string().into(), false),
                            Some(index),
                        ));
                    }
                }
            } else if inherited(key)
                || member(
                    key,
                    &[
                        "$defs",
                        "definitions",
                        "properties",
                        "patternProperties",
                        "dependencies",
                    ],
                )
            {
                for (name, child) in entries(value).into_iter().rev() {
                    pending.push((child, append(&path, name, true), Some(index)));
                }
            } else if !member(
                key,
                &[
                    "default",
                    "enum",
                    "const",
                    "required",
                    "maximum",
                    "minimum",
                    "exclusiveMaximum",
                    "exclusiveMinimum",
                    "multipleOf",
                    "maxLength",
                    "minLength",
                    "pattern",
                    "format",
                    "maxItems",
                    "minItems",
                    "uniqueItems",
                    "maxProperties",
                    "minProperties",
                ],
            ) {
                pending.push((value, path, Some(index)));
            }
        }
        nodes.push(SchemaResourceNode {
            pointer,
            parent,
            schema,
        });
    }
    SchemaResourceGraph {
        isolated: contains_id(schema),
        nodes,
    }
}
