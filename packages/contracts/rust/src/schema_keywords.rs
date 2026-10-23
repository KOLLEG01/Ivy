//! Native keyword checks for compiled JSON Schema nodes.
use crate::{IvyError, IvyJson, IvyResult, JsString};

fn invalid() -> IvyError {
    IvyError::new("registry_invalid", "Invalid or unsupported JSON schema.")
}

/// Check keyword values at one compiled schema node. The caller selects active
/// branches and resolves references; annotations and unused definitions are not
/// recursively interpreted here. Admission and meta-validation are separate.
pub fn validate_compiler_keyword_values(node: &IvyJson) -> IvyResult<()> {
    match node {
        IvyJson::Bool(_) => return Ok(()),
        IvyJson::Object(_) => {}
        _ => return Err(invalid()),
    }
    if node.get("id").is_some() {
        return Err(invalid());
    }
    if node
        .get("$async")
        .is_some_and(|value| value.as_bool() != Some(false))
    {
        return Err(invalid());
    }
    if let Some(types) = node.get("type") {
        let valid = match types {
            IvyJson::Array(types) => !types.is_empty() && types.iter().all(json_type),
            other => json_type(other),
        };
        if !valid {
            return Err(invalid());
        }
    }
    for keyword in [
        "minimum",
        "maximum",
        "exclusiveMinimum",
        "exclusiveMaximum",
        "multipleOf",
        "minLength",
        "maxLength",
        "minItems",
        "maxItems",
        "minProperties",
        "maxProperties",
        "minContains",
        "maxContains",
    ] {
        if node
            .get(keyword)
            .is_some_and(|v| !matches!(v, IvyJson::Number(_)))
        {
            return Err(invalid());
        }
    }
    for keyword in [
        "pattern",
        "format",
        "$dynamicAnchor",
        "$dynamicRef",
        "$recursiveRef",
    ] {
        if node
            .get(keyword)
            .is_some_and(|v| v.as_js_string().is_none())
        {
            return Err(invalid());
        }
    }
    for keyword in ["uniqueItems", "nullable", "$recursiveAnchor"] {
        if node.get(keyword).is_some_and(|v| v.as_bool().is_none()) {
            return Err(invalid());
        }
    }
    if let Some(nullable) = node.get("nullable").and_then(IvyJson::as_bool) {
        let types = node
            .get("type")
            .map(|types| {
                types
                    .as_array()
                    .map_or_else(|| vec![types], |types| types.iter().collect())
            })
            .unwrap_or_default();
        if types.is_empty() || (!nullable && types.contains(&&IvyJson::from("null"))) {
            return Err(invalid());
        }
    }
    for keyword in ["$dynamicRef", "$recursiveRef"] {
        if node
            .get(keyword)
            .and_then(IvyJson::as_js_string)
            .is_some_and(|s| s.0.first() != Some(&35))
        {
            return Err(invalid());
        }
    }
    for keyword in ["enum", "required", "allOf", "anyOf", "oneOf", "prefixItems"] {
        if let Some(value) = node.get(keyword) {
            let values = value.as_array().ok_or_else(invalid)?;
            if keyword == "enum" && values.is_empty() {
                return Err(invalid());
            }
            if keyword == "required" && values.iter().any(|key| key.as_js_string().is_none()) {
                return Err(invalid());
            }
            if matches!(keyword, "allOf" | "anyOf" | "oneOf" | "prefixItems")
                && (values.is_empty()
                    || values
                        .iter()
                        .any(|value| !matches!(value, IvyJson::Bool(_) | IvyJson::Object(_))))
            {
                return Err(invalid());
            }
        }
    }
    for keyword in [
        "properties",
        "$defs",
        "patternProperties",
        "dependencies",
        "dependentRequired",
        "dependentSchemas",
    ] {
        if node.get(keyword).is_some_and(|v| v.as_object().is_none()) {
            return Err(invalid());
        }
    }
    if let Some(map) = node.get("dependentRequired") {
        for (_, deps) in super::entries(map) {
            if deps.as_array().is_none() {
                return Err(invalid());
            }
        }
    }
    for keyword in ["dependencies", "dependentRequired"] {
        if let Some(map) = node.get(keyword) {
            for (_, deps) in super::entries(map) {
                if let Some(names) = deps.as_array() {
                    if names.iter().any(|name| name.as_js_string().is_none()) {
                        return Err(invalid());
                    }
                }
            }
        }
    }
    for keyword in [
        "if",
        "then",
        "else",
        "not",
        "items",
        "additionalProperties",
        "contains",
        "propertyNames",
        "unevaluatedProperties",
        "unevaluatedItems",
    ] {
        if node
            .get(keyword)
            .is_some_and(|v| !matches!(v, IvyJson::Bool(_) | IvyJson::Object(_)))
        {
            return Err(invalid());
        }
    }
    Ok(())
}

fn json_type(value: &IvyJson) -> bool {
    let Some(value) = value.as_js_string() else {
        return false;
    };
    [
        "string", "number", "integer", "boolean", "null", "object", "array",
    ]
    .iter()
    .any(|kind| value == &JsString::from(*kind))
}
