//! Packaged public schemas, documented defaults and Collector-specific constraints.
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString, builtin_schema::SchemaBundle, json::parse,
};
use std::{collections::HashSet, sync::LazyLock};

const SCHEMAS: &str = include_str!("../assets/schemas.json");
const REGISTRY: &str = include_str!("../assets/registry.json");
static DOCUMENT: LazyLock<IvyJson> =
    LazyLock::new(|| parse(SCHEMAS, usize::MAX).expect("static collector schemas"));
static BUNDLE: LazyLock<SchemaBundle> =
    LazyLock::new(|| SchemaBundle::new(&[SCHEMAS]).expect("static collector schemas"));
static REGISTRY_VALUE: LazyLock<IvyJson> =
    LazyLock::new(|| parse(REGISTRY, usize::MAX).expect("static collector registry"));

pub fn registry() -> IvyJson {
    REGISTRY_VALUE.clone()
}

/// Keep validation diagnostics and configured secret values out of public errors.
pub fn parse_schema(name: &str, input: &IvyJson) -> IvyResult<IvyJson> {
    let path = format!("#/$defs/{name}");
    if DOCUMENT.get("$defs").and_then(|v| v.get(name)).is_none() {
        return Err(IvyError::internal());
    }
    normalize(&path, input)
}

fn schema(path: &str) -> IvyResult<&'static IvyJson> {
    let mut value = &*DOCUMENT;
    for key in path
        .strip_prefix("#/")
        .ok_or_else(IvyError::internal)?
        .split('/')
    {
        let key = key.replace("~1", "/").replace("~0", "~");
        value = if let IvyJson::Array(items) = value {
            items.get(key.parse::<usize>().map_err(|_| IvyError::internal())?)
        } else {
            value.get(&key)
        }
        .ok_or_else(IvyError::internal)?;
    }
    Ok(value)
}
fn child(path: &str, group: &str, key: &JsString) -> IvyResult<String> {
    Ok(format!(
        "{path}/{group}/{}",
        key.to_utf8()?.replace('~', "~0").replace('/', "~1")
    ))
}

fn normalize(path: &str, input: &IvyJson) -> IvyResult<IvyJson> {
    if input.as_f64().is_some_and(|n| !n.is_finite()) {
        return Err(IvyError::internal());
    }
    let s = schema(path)?;
    if let Some(reference) = s.get("$ref") {
        return normalize(&reference.as_str()?, input);
    }
    if s.get("x-collector-json") == Some(&IvyJson::Bool(true)) {
        return json_value(input);
    }
    // The source's oneOf nodes are discriminated unions with disjoint literals.
    for union in ["anyOf", "oneOf"] {
        if let Some(IvyJson::Array(branches)) = s.get(union) {
            for index in 0..branches.len() {
                if let Ok(value) = normalize(&format!("{path}/{union}/{index}"), input) {
                    return Ok(value);
                }
            }
            return Err(IvyError::internal());
        }
    }
    let result = match input {
        IvyJson::Object(entries) => {
            let mut result = IvyJson::Object(Vec::new());
            if let Some(IvyJson::Object(properties)) = s.get("properties") {
                if s.get("additionalProperties") == Some(&IvyJson::Bool(false))
                    && entries
                        .iter()
                        .any(|(key, _)| !properties.iter().any(|(name, _)| name == key))
                {
                    return Err(IvyError::internal());
                }
                for (key, property) in properties {
                    if let Some(value) = input.get_key(key) {
                        result.insert(
                            key.clone(),
                            normalize(&child(path, "properties", key)?, value)?,
                        );
                    } else if let Some(value) = property.get("default") {
                        result.insert(key.clone(), value.clone());
                    }
                }
            } else if s
                .get("additionalProperties")
                .is_some_and(|v| matches!(v, IvyJson::Object(_)))
            {
                for (key, value) in entries {
                    let value = normalize(&format!("{path}/additionalProperties"), value)?;
                    result.insert(key.clone(), value);
                }
            } else {
                result = input.clone();
            }
            result
        }
        IvyJson::Array(values) if s.get("items").is_some() => IvyJson::Array(
            values
                .iter()
                .map(|v| normalize(&format!("{path}/items"), v))
                .collect::<IvyResult<Vec<_>>>()?,
        ),
        _ => input.clone(),
    };
    if !BUNDLE.matches(path, &result)? {
        return Err(IvyError::internal());
    }
    if s.get("x-collector-distinct-tools") == Some(&IvyJson::Bool(true)) {
        let IvyJson::Array(tools) = result.required("tools")? else {
            return Err(IvyError::internal());
        };
        let mut seen = HashSet::new();
        for tool in tools {
            if !seen.insert(tool.as_js_string().ok_or_else(IvyError::internal)?) {
                return Err(IvyError::internal());
            }
        }
    }
    if s.get("x-collector-input-auth") == Some(&IvyJson::Bool(true))
        && result.get("allowUnauthenticatedInput") == Some(&IvyJson::Bool(true))
        && result.get("inputSecretName").is_some_and(|v| !v.is_null())
    {
        return Err(IvyError::internal());
    }
    Ok(result)
}

// JSON data has ordinary member names, including names used by language runtimes.
fn json_value(input: &IvyJson) -> IvyResult<IvyJson> {
    let mut pending = vec![input];
    while let Some(value) = pending.pop() {
        match value {
            IvyJson::Number(n) if !n.is_finite() => return Err(IvyError::internal()),
            IvyJson::Array(items) => pending.extend(items),
            IvyJson::Object(entries) => pending.extend(entries.iter().map(|(_, value)| value)),
            _ => {}
        }
    }
    Ok(input.clone())
}
