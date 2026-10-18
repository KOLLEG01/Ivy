use super::entries;
use crate::{IvyError, IvyJson, IvyResult, JsString};
use std::collections::{HashMap, HashSet};

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
}

pub struct SchemaResourceReference {
    pub id: JsString,
    pub node: usize,
    pub full_path: JsString,
    /// False when the declared ID already equals its normalized document path.
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
    })
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

fn append(pointer: &JsString, key: &JsString, escape: bool) -> JsString {
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
