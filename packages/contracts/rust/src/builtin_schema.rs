//! Evaluation of trusted repository schema documents against the lossless wire domain.
//! Dynamic registry schemas require their separate RE2 admission path.
use crate::{IvyError, IvyJson, IvyResult, JsString, json::parse, limits::MANAGEMENT_FRAME_BYTES};
use std::collections::HashMap;
#[path = "schema_evaluation.rs"]
mod evaluation;

pub struct SchemaBundle {
    documents: Vec<IvyJson>,
    identities: HashMap<String, usize>,
    patterns: HashMap<JsString, regress::Regex>,
}
impl SchemaBundle {
    /// Documents must be trusted, static repository schemas, never a submitted registry schema.
    pub fn new(documents: &'static [&'static str]) -> IvyResult<Self> {
        let documents = documents
            .iter()
            .map(|document| parse(document, MANAGEMENT_FRAME_BYTES))
            .collect::<IvyResult<Vec<_>>>()?;
        Self::from_documents(documents)
    }
    pub(crate) fn from_documents(documents: Vec<IvyJson>) -> IvyResult<Self> {
        let mut identities = HashMap::new();
        let mut patterns = HashMap::new();
        for (index, document) in documents.iter().enumerate() {
            if let Some(identity) = document.get("$id") {
                identities.insert(identity.as_str()?, index);
            }
            collect_patterns(document, &mut patterns)?;
        }
        Ok(Self {
            documents,
            identities,
            patterns,
        })
    }
    pub fn matches(&self, reference: &str, value: &IvyJson) -> IvyResult<bool> {
        let (document, schema) = self.resolve(0, reference)?;
        self.evaluate(document, schema, value)
    }
    fn resolve(&self, current: usize, reference: &str) -> IvyResult<(usize, &IvyJson)> {
        let (identity, fragment) = reference.split_once('#').unwrap_or((reference, ""));
        let document = if identity.is_empty() {
            current
        } else {
            // The repository's relative cross-document references are sibling
            // filenames, resolved against the current document's absolute $id.
            let relative = self.documents[current]
                .get("$id")
                .map(IvyJson::as_str)
                .transpose()?
                .and_then(|base| {
                    base.rsplit_once('/')
                        .map(|(directory, _)| format!("{directory}/{identity}"))
                });
            *self
                .identities
                .get(identity)
                .or_else(|| relative.as_ref().and_then(|name| self.identities.get(name)))
                .ok_or_else(|| {
                    IvyError::new("internal_error", "Unknown internal schema resource.")
                })?
        };
        let mut schema = self
            .documents
            .get(document)
            .ok_or_else(IvyError::internal)?;
        if !fragment.is_empty() {
            if !fragment.starts_with('/') {
                return Err(IvyError::new(
                    "internal_error",
                    "Invalid internal schema pointer.",
                ));
            }
            for key in fragment[1..].split('/') {
                let key = key.replace("~1", "/").replace("~0", "~");
                schema = match schema {
                    IvyJson::Array(values) => key
                        .parse::<usize>()
                        .ok()
                        .and_then(|index| values.get(index)),
                    _ => schema.get(&key),
                }
                .ok_or_else(|| {
                    IvyError::new("internal_error", "Unknown internal schema pointer.")
                })?;
            }
        }
        Ok((document, schema))
    }
}

fn collect_patterns(
    schema: &IvyJson,
    patterns: &mut HashMap<JsString, regress::Regex>,
) -> IvyResult<()> {
    fn compile(
        pattern: &JsString,
        patterns: &mut HashMap<JsString, regress::Regex>,
    ) -> IvyResult<()> {
        if !patterns.contains_key(pattern) {
            let codepoints = char::decode_utf16(pattern.0.iter().copied()).map(|unit| {
                unit.map_or_else(|error| u32::from(error.unpaired_surrogate()), u32::from)
            });
            let expression = regress::Regex::from_unicode(codepoints, "u")
                .map_err(|_| IvyError::new("internal_error", "Invalid internal schema pattern."))?;
            patterns.insert(pattern.clone(), expression);
        }
        Ok(())
    }
    if let Some(pattern) = schema.get("pattern").and_then(IvyJson::as_js_string) {
        compile(pattern, patterns)?;
    }
    for keyword in ["properties", "$defs", "definitions", "patternProperties"] {
        if let Some(map) = schema.get(keyword).and_then(IvyJson::as_object) {
            for (key, child) in map {
                if keyword == "patternProperties" {
                    compile(key, patterns)?;
                }
                collect_patterns(child, patterns)?;
            }
        }
    }
    for keyword in ["anyOf", "oneOf", "allOf"] {
        if let Some(children) = schema.get(keyword).and_then(IvyJson::as_array) {
            for child in children {
                collect_patterns(child, patterns)?;
            }
        }
    }
    for keyword in [
        "items",
        "additionalProperties",
        "propertyNames",
        "if",
        "then",
        "else",
        "not",
    ] {
        if let Some(child) = schema.get(keyword) {
            collect_patterns(child, patterns)?;
        }
    }
    Ok(())
}
fn type_matches(kind: &IvyJson, value: &IvyJson) -> bool {
    match kind.as_str().ok().as_deref() {
        Some("null") => value.is_null(),
        Some("boolean") => value.as_bool().is_some(),
        Some("number") => value.as_f64().is_some(),
        Some("integer") => value
            .as_f64()
            .is_some_and(|n| n.is_finite() && n.fract() == 0.0),
        Some("string") => value.as_js_string().is_some(),
        Some("array") => value.as_array().is_some(),
        Some("object") => value.as_object().is_some(),
        _ => false,
    }
}
fn length_matches(schema: &IvyJson, minimum: &str, maximum: &str, length: usize) -> bool {
    schema
        .get(minimum)
        .and_then(IvyJson::as_f64)
        .is_none_or(|minimum| length as f64 >= minimum)
        && schema
            .get(maximum)
            .and_then(IvyJson::as_f64)
            .is_none_or(|maximum| length as f64 <= maximum)
}
pub fn json_equal(left: &IvyJson, right: &IvyJson) -> bool {
    let mut pending = vec![(left, right)];
    while let Some((left, right)) = pending.pop() {
        match (left, right) {
            (IvyJson::Object(a), IvyJson::Object(b)) => {
                if a.len() != b.len() {
                    return false;
                }
                for (key, value) in a {
                    let Some(other) = right.get_key(key) else {
                        return false;
                    };
                    pending.push((value, other));
                }
            }
            (IvyJson::Array(a), IvyJson::Array(b)) => {
                if a.len() != b.len() {
                    return false;
                }
                pending.extend(a.iter().zip(b));
            }
            _ => {
                if left != right {
                    return false;
                }
            }
        }
    }
    true
}
