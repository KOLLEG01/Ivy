//! Compiler branch selection before reference targets are added to the worklist.
use super::{entries, resources, uri, validate_compiler_keyword_values};
use crate::{IvyError, IvyJson, IvyResult, JsString};

pub struct CompilerNode<'a> {
    pub schema: &'a IvyJson,
    pub pointer: JsString,
    pub base_id: JsString,
    /// Reference targets must be compiled separately under this base ID.
    pub reference: Option<&'a JsString>,
}

pub struct CompilerWalk<'a> {
    /// Registered root identity, distinct from the effective '#' anonymous scope.
    pub root_base_id: JsString,
    /// Root and selected object subschemas in structural order. This is not the
    /// validation execution order; boolean branches remain in their parent schema.
    pub nodes: Vec<CompilerNode<'a>>,
}

fn has_rules(schema: &IvyJson) -> bool {
    has_rules_except_ref(schema, false)
}

pub(super) fn has_rules_but_ref(schema: &IvyJson) -> bool {
    has_rules_except_ref(schema, true)
}

fn has_rules_except_ref(schema: &IvyJson, without_ref: bool) -> bool {
    const RULES: &[&str] = &[
        "$dynamicAnchor",
        "$dynamicRef",
        "$recursiveAnchor",
        "$recursiveRef",
        "$comment",
        "id",
        "nullable",
        "$ref",
        "type",
        "const",
        "enum",
        "not",
        "anyOf",
        "oneOf",
        "allOf",
        "if",
        "then",
        "else",
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
        "minProperties",
        "maxProperties",
        "required",
        "additionalProperties",
        "properties",
        "items",
        "prefixItems",
        "contains",
        "minContains",
        "maxContains",
        "propertyNames",
        "patternProperties",
        "dependencies",
        "dependentRequired",
        "dependentSchemas",
        "unevaluatedProperties",
        "unevaluatedItems",
    ];
    entries(schema).iter().any(|(key, _)| {
        (!without_ref || *key != &JsString::from("$ref"))
            && RULES.iter().any(|rule| *key == &JsString::from(*rule))
    })
}

fn always_valid(schema: &IvyJson) -> bool {
    match schema {
        IvyJson::Bool(value) => *value,
        IvyJson::Null => false,
        _ => !has_rules(schema),
    }
}

/// Select active inline compiler branches and check their keyword values.
/// Admission, meta-validation and resource registration retain their own stages;
/// this walk performs registration but leaves reference targets to the resolver.
pub(super) fn walk<'a>(schema: &'a IvyJson) -> IvyResult<CompilerWalk<'a>> {
    let registered = resources::register(schema)?;
    let scope = if registered.base_id.0.is_empty() {
        registered.path_prefix.clone()
    } else {
        registered.base_id.clone()
    };
    walk_scoped(schema, registered.base_id, scope)
}

pub(super) fn walk_scoped<'a>(
    schema: &'a IvyJson,
    root_base_id: JsString,
    scope: JsString,
) -> IvyResult<CompilerWalk<'a>> {
    let mut pending = vec![(schema, JsString::default(), scope, true)];
    let mut nodes = Vec::new();
    while let Some((node, pointer, mut base_id, root)) = pending.pop() {
        // Boolean subschemas have no keyword checks or effective scope changes.
        if !root && matches!(node, IvyJson::Bool(_)) {
            continue;
        }
        if !root && has_rules(node) {
            if let Some(id) = node
                .get("$id")
                .and_then(IvyJson::as_js_string)
                .filter(|id| !id.0.is_empty())
            {
                base_id = uri::resolve(&base_id, &uri::normalize_id(id))?;
            }
        }
        validate_compiler_keyword_values(node)?;
        let reference = node.get("$ref").and_then(IvyJson::as_js_string);
        nodes.push(CompilerNode {
            schema: node,
            pointer: pointer.clone(),
            base_id: base_id.clone(),
            reference,
        });
        let mut children = Vec::new();
        let mut add = |keyword: &str, child: &'a IvyJson, key: Option<JsString>| {
            let path = resources::append(&pointer, &keyword.into(), true);
            let path = key.map_or_else(|| path.clone(), |key| resources::append(&path, &key, true));
            children.push((child, path, base_id.clone(), false));
        };
        if let Some(child) = node.get("not").filter(|child| !always_valid(child)) {
            add("not", child, None);
        }
        for keyword in ["anyOf", "oneOf", "allOf", "prefixItems"] {
            if let Some(items) = node.get(keyword).and_then(IvyJson::as_array) {
                for (index, child) in items.iter().enumerate() {
                    // Draft 2020 tracks evaluated properties/items, so anyOf
                    // still compiles every branch even beside an empty schema.
                    if keyword == "anyOf" || !always_valid(child) {
                        add(keyword, child, Some(index.to_string().into()));
                    }
                }
            }
        }
        let active_then = node.get("then").filter(|child| !always_valid(child));
        let active_else = node.get("else").filter(|child| !always_valid(child));
        if let Some(condition) = node
            .get("if")
            .filter(|_| active_then.is_some() || active_else.is_some())
        {
            add("if", condition, None);
            if let Some(child) = active_then {
                add("then", child, None);
            }
            if let Some(child) = active_else {
                add("else", child, None);
            }
        }
        if let Some(child) = node.get("items").filter(|child| !always_valid(child)) {
            add("items", child, None);
        }
        if let Some(child) = node.get("contains").filter(|child| !always_valid(child)) {
            let min = node
                .get("minContains")
                .and_then(IvyJson::as_f64)
                .unwrap_or(1.0);
            let max = node.get("maxContains").and_then(IvyJson::as_f64);
            if !(max.is_none() && min == 0.0 || max.is_some_and(|max| min > max)) {
                add("contains", child, None);
            }
        }
        for keyword in ["propertyNames", "unevaluatedProperties", "unevaluatedItems"] {
            if let Some(child) = node.get(keyword).filter(|child| !always_valid(child)) {
                add(keyword, child, None);
            }
        }
        if let Some(child) = node
            .get("additionalProperties")
            .filter(|child| !always_valid(child))
        {
            add("additionalProperties", child, None);
        }
        for keyword in [
            "properties",
            "patternProperties",
            "dependencies",
            "dependentSchemas",
        ] {
            if let Some(properties) = node.get(keyword) {
                for (key, child) in entries(properties)
                    .into_iter()
                    .filter(|(key, _)| *key != &JsString::from("__proto__"))
                {
                    if keyword == "dependencies" && child.as_array().is_some() {
                        continue;
                    }
                    if !always_valid(child) {
                        // getSubschema builds an encoded error path only for an
                        // active property; encodeURIComponent rejects lone units.
                        if String::from_utf16(&key.0).is_err() {
                            return Err(IvyError::new(
                                "registry_invalid",
                                "Invalid or unsupported JSON schema.",
                            ));
                        }
                        add(keyword, child, Some(key.clone()));
                    }
                }
            }
        }
        pending.extend(children.into_iter().rev());
    }
    Ok(CompilerWalk {
        root_base_id,
        nodes,
    })
}
