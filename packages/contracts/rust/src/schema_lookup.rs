//! Canonical name lookup uses JavaScript properties, separately from schema evaluation.
use crate::{IvyError, IvyJson, IvyResult, JsString};
use serde::Deserialize;
use std::{collections::HashMap, sync::OnceLock};

#[derive(Deserialize)]
struct Builtins {
    roots: HashMap<String, usize>,
    nodes: Vec<Node>,
}
#[derive(Deserialize)]
struct Node {
    kind: String,
    own: HashMap<String, i32>,
    prototype: Option<usize>,
}
fn builtins() -> &'static Builtins {
    static DATA: OnceLock<Builtins> = OnceLock::new();
    DATA.get_or_init(|| {
        serde_json::from_str(include_str!("schema-lookup-builtins.json"))
            .expect("generated built-ins")
    })
}
pub(crate) enum Value<'a> {
    Json(&'a IvyJson),
    Builtin(usize),
    Owned(IvyJson),
}
fn builtin_property(receiver: usize, key: &str) -> IvyResult<Option<Value<'static>>> {
    let data = builtins();
    let mut current = receiver;
    loop {
        let node = &data.nodes[current];
        if node.kind == "null" || node.kind == "undefined" {
            return Err(IvyError::internal());
        }
        if node.kind == "boolean" {
            return Ok(None);
        }
        if let Some(index) = node.own.get(key) {
            return match *index {
                -2 => data.nodes[receiver]
                    .prototype
                    .map(|index| Some(Value::Builtin(index)))
                    .ok_or_else(IvyError::internal),
                -1 => Err(IvyError::internal()),
                index => Ok((data.nodes[index as usize].kind != "undefined")
                    .then_some(Value::Builtin(index as usize))),
            };
        }
        let Some(next) = node.prototype else {
            return Ok(None);
        };
        current = next;
    }
}
fn json_property<'a>(value: &'a IvyJson, key: &str) -> IvyResult<Option<Value<'a>>> {
    if let Some(child) = value.get(key) {
        return Ok(Some(Value::Json(child)));
    }
    let inherited = match value {
        IvyJson::Null => return Err(IvyError::internal()),
        IvyJson::Bool(_) => return Ok(None),
        IvyJson::Object(_) => "object",
        IvyJson::Array(items) => {
            if key == "length" {
                return Ok(Some(Value::Owned(IvyJson::Number(items.len() as f64))));
            }
            if let Ok(index) = key.parse::<usize>() {
                if index.to_string() == key {
                    if let Some(item) = items.get(index) {
                        return Ok(Some(Value::Json(item)));
                    }
                }
            }
            "array"
        }
        IvyJson::String(text) => {
            if key == "length" {
                return Ok(Some(Value::Owned(IvyJson::Number(text.0.len() as f64))));
            }
            if let Ok(index) = key.parse::<usize>() {
                if index.to_string() == key {
                    if let Some(unit) = text.0.get(index) {
                        return Ok(Some(Value::Owned(IvyJson::String(JsString(vec![*unit])))));
                    }
                }
            }
            "string"
        }
        IvyJson::Number(_) => "number",
    };
    if key == "__proto__" {
        return Ok(Some(Value::Builtin(builtins().roots[inherited])));
    }
    builtin_property(builtins().roots[inherited], key)
}
impl<'a> Value<'a> {
    pub(crate) fn is_boolean(&self) -> bool {
        match self {
            Self::Json(value) => matches!(value, IvyJson::Bool(_)),
            Self::Owned(value) => matches!(value, IvyJson::Bool(_)),
            Self::Builtin(index) => builtins().nodes[*index].kind == "boolean",
        }
    }
    pub(crate) fn property(self, key: &str) -> IvyResult<Option<Value<'a>>> {
        match self {
            Self::Json(value) => json_property(value, key),
            Self::Builtin(index) => builtin_property(index, key),
            Self::Owned(value) => Ok(match json_property(&value, key)? {
                Some(Value::Json(child)) => Some(Self::Owned(child.clone())),
                Some(Value::Owned(child)) => Some(Self::Owned(child)),
                Some(Value::Builtin(index)) => Some(Self::Builtin(index)),
                None => None,
            }),
        }
    }
    pub(crate) fn schema(self) -> IvyResult<Option<&'a IvyJson>> {
        match self {
            Self::Json(value) => match value {
                IvyJson::Null => Err(IvyError::internal()),
                IvyJson::Bool(_) | IvyJson::Object(_) => Ok(Some(value)),
                _ => Ok(None),
            },
            Self::Owned(_) => Ok(None),
            Self::Builtin(index) => match builtins().nodes[index].kind.as_str() {
                "null" | "undefined" => Err(IvyError::internal()),
                _ => Ok(None),
            },
        }
    }
}
/// fast-uri preserves invalid percent triplets as literal percent signs, while
/// decodeURIComponent still rejects escaped bytes that are not valid UTF-8.
pub(crate) fn pointer_key(text: &str) -> IvyResult<String> {
    let input = text.as_bytes();
    let mut bytes = Vec::with_capacity(input.len());
    let mut i = 0;
    while i < input.len() {
        if input[i] == b'%' && i + 2 < input.len() {
            if let (Some(a), Some(b)) = (
                (input[i + 1] as char).to_digit(16),
                (input[i + 2] as char).to_digit(16),
            ) {
                bytes.push((a * 16 + b) as u8);
                i += 3;
                continue;
            }
        }
        bytes.push(input[i]);
        i += 1;
    }
    Ok(String::from_utf8(bytes)
        .map_err(|_| IvyError::internal())?
        .replace("~1", "/")
        .replace("~0", "~"))
}

/// URI fragments are distinct from admission's literal JSON pointers. Both AJV
/// entry paths replace unpaired UTF-16 surrogates with U+FFFD. Resolving a $ref
/// rejects malformed percent escapes; direct getSchema lookup escapes them.
pub(crate) fn compiler_pointer_parts(
    fragment: &JsString,
    strict_percent: bool,
) -> IvyResult<Vec<String>> {
    if fragment.0.first() != Some(&47) {
        return Err(IvyError::internal());
    }
    fragment.0[1..]
        .split(|c| *c == 47)
        .map(|part| compiler_pointer_key(part, strict_percent))
        .collect()
}

pub(crate) fn compiler_pointer_key(part: &[u16], strict_percent: bool) -> IvyResult<String> {
    let text = String::from_utf16_lossy(part);
    let input = text.as_bytes();
    let mut index = 0;
    while strict_percent && index < input.len() {
        if input[index] == b'%' {
            let hex = |offset: usize| {
                input
                    .get(offset)
                    .and_then(|byte| (*byte as char).to_digit(16))
            };
            hex(index + 1).ok_or_else(IvyError::internal)?;
            hex(index + 2).ok_or_else(IvyError::internal)?;
            index += 3;
        } else {
            index += 1;
        }
    }
    pointer_key(&text)
}
