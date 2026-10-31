//! Per-tool MCP schema composition with provider-local reference identities.
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString, hash::hash_json};
use std::collections::HashMap;

fn empty() -> IvyJson {
    IvyJson::object(std::iter::empty::<(&str, IvyJson)>())
}
fn pointer_token(value: &JsString) -> JsString {
    let mut out = vec![];
    for c in &value.0 {
        match c {
            126 => out.extend("~0".encode_utf16()),
            47 => out.extend("~1".encode_utf16()),
            _ => out.push(*c),
        }
    }
    JsString(out)
}
fn pointer_key(value: &[u16]) -> JsString {
    let mut out = vec![];
    let mut i = 0;
    while i < value.len() {
        if value[i] == 126 && value.get(i + 1).is_some_and(|v| [48, 49].contains(v)) {
            out.push(if value[i + 1] == 49 { 47 } else { 126 });
            i += 2;
        } else {
            out.push(value[i]);
            i += 1;
        }
    }
    JsString(out)
}
fn cat(values: &[&JsString]) -> JsString {
    JsString(values.iter().flat_map(|v| v.0.iter().copied()).collect())
}
fn child_mut<'a>(value: &'a mut IvyJson, key: &str) -> Option<&'a mut IvyJson> {
    if let IvyJson::Object(values) = value {
        values
            .iter_mut()
            .find(|(k, _)| k == &JsString::from(key))
            .map(|(_, v)| v)
    } else {
        None
    }
}
fn values(value: &IvyJson) -> Vec<&IvyJson> {
    match value {
        IvyJson::Array(v) => v.iter().collect(),
        IvyJson::Object(v) => v.iter().map(|(_, v)| v).collect(),
        _ => vec![],
    }
}
fn visit(value: &IvyJson, f: &mut impl FnMut(&IvyJson) -> IvyResult<()>) -> IvyResult<()> {
    if value.as_object().is_none() {
        return Ok(());
    }
    f(value)?;
    for key in [
        "properties",
        "$defs",
        "patternProperties",
        "dependentSchemas",
    ] {
        if let Some(v) = value.get(key) {
            for child in values(v) {
                visit(child, f)?;
            }
        }
    }
    for key in ["allOf", "anyOf", "oneOf", "prefixItems"] {
        if let Some(v) = value.get(key).and_then(IvyJson::as_array) {
            for child in v {
                visit(child, f)?;
            }
        }
    }
    for key in [
        "items",
        "additionalProperties",
        "unevaluatedProperties",
        "not",
        "if",
        "then",
        "else",
        "contains",
        "propertyNames",
    ] {
        if let Some(v) = value.get(key) {
            visit(v, f)?;
        }
    }
    Ok(())
}
fn local_target<'a>(source: &'a IvyJson, reference: &JsString) -> IvyResult<&'a IvyJson> {
    let fail = || {
        IvyError::new(
            "registry_invalid",
            "MCP schema reference has no local target.",
        )
    };
    let mut target = source;
    if reference.0 == [35] {
        return Ok(target);
    }
    for part in reference.0.get(2..).unwrap_or(&[]).split(|v| *v == 47) {
        let key = pointer_key(part);
        target = match target {
            IvyJson::Array(array) => {
                let index = key
                    .to_utf8()
                    .ok()
                    .and_then(|v| v.parse::<usize>().ok().filter(|i| i.to_string() == v));
                index.and_then(|i| array.get(i)).ok_or_else(fail)?
            }
            _ => target.get_key(&key).ok_or_else(fail)?,
        };
    }
    Ok(target)
}
fn dependencies(
    value: &IvyJson,
    source: &IvyJson,
    result: &mut IvyJson,
    outside: &mut bool,
) -> IvyResult<()> {
    visit(value, &mut |node| {
        let Some(reference) = node
            .get("$ref")
            .and_then(IvyJson::as_js_string)
            .filter(|v| v.0.starts_with(&[35]))
        else {
            return Ok(());
        };
        if !reference
            .0
            .starts_with(&"#/$defs/".encode_utf16().collect::<Vec<_>>())
        {
            *outside = true;
        }
        if result.get_key(reference).is_some() {
            return Ok(());
        }
        let target = local_target(source, reference)?;
        result.insert(reference.clone(), target.clone());
        dependencies(target, source, result, outside)
    })
}
fn rewrite(
    value: &mut IvyJson,
    path: &JsString,
    references: &HashMap<JsString, JsString>,
) -> IvyResult<()> {
    if value.as_object().is_none() {
        return Ok(());
    }
    value.remove("$id");
    value.remove("$schema");
    if let Some(reference) = value
        .get("$ref")
        .and_then(IvyJson::as_js_string)
        .filter(|v| v.0.starts_with(&[35]))
        .cloned()
    {
        let prefix: Vec<u16> = "#/$defs/".encode_utf16().collect();
        let selected = reference
            .0
            .strip_prefix(prefix.as_slice())
            .filter(|v| !v.is_empty())
            .and_then(|tail| {
                let end = tail.iter().position(|v| *v == 47).unwrap_or(tail.len());
                if end == 0 {
                    return None;
                }
                references
                    .get(&JsString(tail[..end].to_vec()))
                    .map(|name| cat(&[&"#/$defs/".into(), name, &JsString(tail[end..].to_vec())]))
            });
        value.insert(
            "$ref",
            IvyJson::String(selected.unwrap_or_else(|| {
                cat(&[&"#".into(), path, &JsString(reference.0[1..].to_vec())])
            })),
        );
    }
    for key in [
        "properties",
        "$defs",
        "patternProperties",
        "dependentSchemas",
    ] {
        if let Some(child) = child_mut(value, key) {
            match child {
                IvyJson::Object(v) => {
                    for (_, v) in v {
                        rewrite(v, path, references)?;
                    }
                }
                IvyJson::Array(v) => {
                    for v in v {
                        rewrite(v, path, references)?;
                    }
                }
                _ => {}
            }
        }
    }
    for key in ["allOf", "anyOf", "oneOf", "prefixItems"] {
        if let Some(IvyJson::Array(v)) = child_mut(value, key) {
            for v in v {
                rewrite(v, path, references)?;
            }
        }
    }
    for key in [
        "items",
        "additionalProperties",
        "unevaluatedProperties",
        "not",
        "if",
        "then",
        "else",
        "contains",
        "propertyNames",
    ] {
        if let Some(child) = child_mut(value, key) {
            rewrite(child, path, references)?;
        }
    }
    Ok(())
}
pub struct McpSchemaBundle {
    definitions: IvyJson,
    names: HashMap<String, JsString>,
}
impl Default for McpSchemaBundle {
    fn default() -> Self {
        Self {
            definitions: empty(),
            names: HashMap::new(),
        }
    }
}
impl McpSchemaBundle {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn embed(&mut self, value: &IvyJson, path: &JsString) -> IvyResult<IvyJson> {
        if value.as_object().is_none() {
            return Ok(value.clone());
        }
        let source = value;
        let mut references = HashMap::new();
        let mut added = vec![];
        if let Some(definitions) = source.get("$defs").and_then(IvyJson::as_object) {
            for (key, definition) in definitions {
                let mut deps = empty();
                let mut outside = false;
                dependencies(definition, source, &mut deps, &mut outside)?;
                let mut identity =
                    IvyJson::object([("definition", definition.clone()), ("dependencies", deps)]);
                if outside {
                    identity.insert("path", IvyJson::String(path.clone()));
                }
                let identity = hash_json(&identity)?;
                let name = if let Some(name) = self.names.get(&identity) {
                    name.clone()
                } else {
                    let mut name = key.clone();
                    let mut suffix = 2;
                    while self.definitions.get_key(&name).is_some() {
                        name = cat(&[key, &format!("_{suffix}").as_str().into()]);
                        suffix += 1;
                    }
                    self.names.insert(identity, name.clone());
                    self.definitions.insert(name.clone(), true.into());
                    added.push((name.clone(), definition.clone()));
                    name
                };
                references.insert(pointer_token(key), pointer_token(&name));
            }
        }
        for (name, mut definition) in added {
            rewrite(&mut definition, path, &references)?;
            self.definitions.insert(name, definition);
        }
        let mut root = source.clone();
        root.remove("$defs");
        rewrite(&mut root, path, &references)?;
        Ok(root)
    }
    pub fn finish(self, mut schema: IvyJson) -> IvyJson {
        if self.definitions.as_object().is_some_and(|v| !v.is_empty()) {
            schema.insert("$defs", self.definitions);
        }
        schema
    }
}

#[cfg(test)]
#[path = "../../../../tests/rust/hive_mcp_schema_ownership.rs"]
mod ownership_tests;
