//! Assets are derived from src/schema.ts by the schema oracle. The public
//! registry remains unchanged; private annotations preserve Zod parsing rules.
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString, builtin_schema::SchemaBundle, json::parse};
use std::{collections::HashSet, sync::LazyLock};

const SCHEMAS: &str = include_str!("../assets/schemas.json");
const REGISTRY: &str = include_str!("../assets/registry.json");
static DOCUMENT: LazyLock<IvyJson> = LazyLock::new(|| parse(SCHEMAS, usize::MAX).expect("static collector schemas"));
static BUNDLE: LazyLock<SchemaBundle> = LazyLock::new(|| SchemaBundle::new(&[SCHEMAS]).expect("static collector schemas"));
static REGISTRY_VALUE: LazyLock<IvyJson> = LazyLock::new(|| parse(REGISTRY, usize::MAX).expect("static collector registry"));

pub fn registry() -> IvyJson { REGISTRY_VALUE.clone() }

/// Zod parser failures cross the original IvyError.from boundary as an internal
/// error with unknown outcome. Do not expose schema diagnostics or secret values.
pub fn parse_schema(name: &str, input: &IvyJson) -> IvyResult<IvyJson> {
    let path = format!("#/{name}");
    if DOCUMENT.get(name).is_none() { return Err(IvyError::internal()); }
    normalize(&path, input)
}

fn schema(path: &str) -> IvyResult<&'static IvyJson> {
    let mut value = &*DOCUMENT;
    for key in path.strip_prefix("#/").ok_or_else(IvyError::internal)?.split('/') {
        let key = key.replace("~1", "/").replace("~0", "~");
        value = if let IvyJson::Array(items) = value {
            items.get(key.parse::<usize>().map_err(|_| IvyError::internal())?)
        } else { value.get(&key) }.ok_or_else(IvyError::internal)?;
    }
    Ok(value)
}
fn child(path: &str, group: &str, key: &JsString) -> IvyResult<String> {
    Ok(format!("{path}/{group}/{}", key.to_utf8()?.replace('~', "~0").replace('/', "~1")))
}

fn normalize(path: &str, input: &IvyJson) -> IvyResult<IvyJson> {
    let s = schema(path)?;
    if let Some(reference) = s.get("$ref") { return normalize(&reference.as_str()?, input); }
    if s.get("x-collector-json") == Some(&IvyJson::Bool(true)) { return json_value(input); }
    if let Some(IvyJson::Array(branches)) = s.get("anyOf") {
        for index in 0..branches.len() {
            if let Ok(value) = normalize(&format!("{path}/anyOf/{index}"), input) { return Ok(value); }
        }
        return Err(IvyError::internal());
    }
    if let Some(text) = input.as_js_string() {
        for (key, too_short) in [("x-utf16-minLength", true), ("x-utf16-maxLength", false)] {
            if let Some(bound) = s.get(key).and_then(IvyJson::as_f64) {
                let length = text.utf16_len() as f64;
                if if too_short { length < bound } else { length > bound } { return Err(IvyError::internal()); }
            }
        }
    }
    let result = match input {
        IvyJson::Object(entries) => {
            let mut result = IvyJson::Object(Vec::new());
            if let Some(IvyJson::Object(properties)) = s.get("properties") {
                if s.get("additionalProperties") == Some(&IvyJson::Bool(false))
                    && entries.iter().any(|(key, _)| !properties.iter().any(|(name, _)| name == key)) {
                    return Err(IvyError::internal());
                }
                for (key, property) in properties {
                    if let Some(value) = input.get_key(key) {
                        result.insert(key.clone(), normalize(&child(path, "properties", key)?, value)?);
                    } else if let Some(value) = property.get("default") {
                        result.insert(key.clone(), value.clone());
                    }
                }
            } else if s.get("additionalProperties").is_some_and(|v| matches!(v, IvyJson::Object(_))) {
                for (key, value) in entries {
                    // Zod skips __proto__ before validating the key or value.
                    if key == &JsString::from("__proto__") { continue; }
                    let value = normalize(&format!("{path}/additionalProperties"), value)?;
                    result.insert(key.clone(), value);
                }
            } else { result = input.clone(); }
            result
        }
        IvyJson::Array(values) if s.get("items").is_some() => IvyJson::Array(values.iter()
            .map(|v| normalize(&format!("{path}/items"), v)).collect::<IvyResult<Vec<_>>>()?),
        _ => input.clone(),
    };
    if !BUNDLE.matches(path, &result)? { return Err(IvyError::internal()); }
    if s.get("x-collector-distinct-tools") == Some(&IvyJson::Bool(true)) {
        let IvyJson::Array(tools) = result.required("tools")? else { return Err(IvyError::internal()); };
        let mut seen = HashSet::new();
        for tool in tools { if !seen.insert(tool.as_js_string().ok_or_else(IvyError::internal)?) { return Err(IvyError::internal()); } }
    }
    if s.get("x-collector-input-auth") == Some(&IvyJson::Bool(true))
        && result.get("allowUnauthenticatedInput") == Some(&IvyJson::Bool(true))
        && result.get("inputSecretName").is_some_and(|v| !v.is_null()) {
        return Err(IvyError::internal());
    }
    Ok(result)
}

// User JSON may be deeply nested. Normalize record keys without native recursion.
fn json_value(input: &IvyJson) -> IvyResult<IvyJson> {
    enum Step<'a> { Visit(&'a IvyJson), Array(usize), Object(Vec<JsString>) }
    let mut pending = vec![Step::Visit(input)];
    let mut values = Vec::new();
    while let Some(step) = pending.pop() {
        match step {
            Step::Visit(IvyJson::Number(n)) if !n.is_finite() => return Err(IvyError::internal()),
            Step::Visit(IvyJson::Array(items)) => {
                pending.push(Step::Array(items.len()));
                pending.extend(items.iter().rev().map(Step::Visit));
            }
            Step::Visit(IvyJson::Object(entries)) => {
                let entries: Vec<_> = entries.iter().filter(|(key, _)| key != &JsString::from("__proto__")).collect();
                pending.push(Step::Object(entries.iter().map(|(key, _)| key.clone()).collect()));
                pending.extend(entries.into_iter().rev().map(|(_, value)| Step::Visit(value)));
            }
            Step::Visit(value) => values.push(value.clone()),
            Step::Array(count) => { let items = values.split_off(values.len() - count); values.push(IvyJson::Array(items)); }
            Step::Object(keys) => {
                let items = values.split_off(values.len() - keys.len());
                values.push(IvyJson::Object(keys.into_iter().zip(items).filter(|(key, _)| key != &JsString::from("__proto__")).collect()));
            }
        }
    }
    values.pop().ok_or_else(IvyError::internal)
}
