//! Per-node checks applied by the pinned compiler after schema admission.
use crate::{IvyError, IvyJson, IvyResult, JsString};

fn invalid() -> IvyError {
    IvyError::new("registry_invalid", "Invalid or unsupported JSON schema.")
}

/// Check keyword values at one compiled schema node. The caller selects active
/// branches and resolves references; annotations and unused definitions are not
/// recursively interpreted here. Admission and meta-validation are separate.
pub fn validate_compiler_keyword_values(node: &IvyJson) -> IvyResult<()> {
    if matches!(node, IvyJson::Bool(_)) {
        return Ok(());
    }
    if matches!(node, IvyJson::Null) {
        return Err(invalid());
    }
    // Without meta-validation, scalar/array inline alternatives are treated as
    // empty schemas by the original compiler. Submitted roots still pass the
    // separate admission policy before reaching this stage.
    if node.as_object().is_none() {
        return Ok(());
    }
    if node.get("id").is_some() {
        return Err(invalid());
    }
    if node.get("$async").is_some_and(|value| match value {
        IvyJson::Null | IvyJson::Bool(false) => false,
        IvyJson::Number(n) => *n != 0.0,
        IvyJson::String(s) => !s.0.is_empty(),
        _ => true,
    }) {
        return Err(invalid());
    }
    // The type keyword's own schemaType check still rejects non-string falsy
    // values after getSchemaTypes has treated them as an absent type.
    if let Some(types) = node.get("type") {
        let valid = match types {
            IvyJson::Array(types) => types.iter().all(json_type),
            IvyJson::String(s) if s.0.is_empty() => true,
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
        if types.is_empty()
            || types.iter().all(|value| value == &&IvyJson::from(""))
            || (!nullable && types.contains(&&IvyJson::from("null")))
        {
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
            if keyword == "required" && values.len() < 64 {
                for key in values.iter().filter(|key| key.as_array().is_some()) {
                    crate::json::try_js_string(key).map_err(|_| invalid())?;
                }
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
            for (key, deps) in super::entries(map) {
                if key != &JsString::from("__proto__") && deps.as_array().is_some() {
                    crate::json::try_js_string(deps).map_err(|_| invalid())?;
                }
            }
        }
    }
    for keyword in [
        "if",
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
