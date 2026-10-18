use super::entries;
use crate::{IvyJson, JsString};

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
