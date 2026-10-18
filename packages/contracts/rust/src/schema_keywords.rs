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
    if node.as_object().is_none() {
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
    ] {
        if node
            .get(keyword)
            .is_some_and(|v| !matches!(v, IvyJson::Number(_)))
        {
            return Err(invalid());
        }
    }
    for keyword in ["pattern", "format"] {
        if node
            .get(keyword)
            .is_some_and(|v| v.as_js_string().is_none())
        {
            return Err(invalid());
        }
    }
    if node
        .get("uniqueItems")
        .is_some_and(|v| v.as_bool().is_none())
    {
        return Err(invalid());
    }
    for keyword in ["enum", "required", "allOf", "anyOf", "oneOf"] {
        if let Some(value) = node.get(keyword) {
            let values = value.as_array().ok_or_else(invalid)?;
            if keyword == "enum" && values.is_empty() {
                return Err(invalid());
            }
        }
    }
    for keyword in ["properties", "$defs"] {
        if node.get(keyword).is_some_and(|v| v.as_object().is_none()) {
            return Err(invalid());
        }
    }
    for keyword in ["if", "not", "items", "additionalProperties"] {
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
