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
    /// Build from owned, trusted repository documents, such as packaged schemas.
    /// Dynamic registry schemas require their separate admission path.
    pub fn from_documents(documents: Vec<IvyJson>) -> IvyResult<Self> {
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
    /// Named definition lookup into a trusted repository document.
    pub(crate) fn matches_named(
        &self,
        document: &str,
        name: &str,
        value: &IvyJson,
    ) -> IvyResult<Option<bool>> {
        let document = *self
            .identities
            .get(document)
            .ok_or_else(IvyError::internal)?;
        let mut selected = &self.documents[document];
        for part in std::iter::once("$defs").chain(name.split('/')) {
            let key = crate::schema_lookup::pointer_key(part)?;
            let Some(child) = crate::schema_lookup::property(selected, &key) else {
                return Ok(None);
            };
            selected = child;
        }
        let schema = crate::schema_lookup::schema(selected)?;
        self.evaluate(document, schema, value)
            .map(Some)
            .map_err(|_| IvyError::internal())
    }
    fn resolve(&self, current: usize, reference: &str) -> IvyResult<(usize, &IvyJson)> {
        self.resolve_js(current, &reference.into(), false)
    }
    fn resolve_js(
        &self,
        current: usize,
        reference: &JsString,
        strict_percent: bool,
    ) -> IvyResult<(usize, &IvyJson)> {
        // Compiled refs normalize their ID before resolution. Public getSchema
        // lookup instead parses the original fragment: a later '#' is key data.
        let units = if strict_percent {
            reference
                .0
                .strip_suffix(&[35, 47])
                .or_else(|| reference.0.strip_suffix(&[35]))
                .unwrap_or(&reference.0)
        } else {
            let hash = reference.0.iter().position(|c| *c == 35);
            match hash {
                Some(hash)
                    if reference.0[hash + 1..].is_empty() || reference.0[hash + 1..] == [47] =>
                {
                    &reference.0[..hash]
                }
                _ => &reference.0,
            }
        };
        let split = units.iter().position(|unit| *unit == u16::from(b'#'));
        let end = split.unwrap_or(units.len());
        let identity = String::from_utf16(&units[..end]).map_err(|_| IvyError::internal())?;
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
                .get(&identity)
                .or_else(|| relative.as_ref().and_then(|name| self.identities.get(name)))
                .ok_or_else(|| {
                    IvyError::new("internal_error", "Unknown internal schema resource.")
                })?
        };
        let schema = self
            .documents
            .get(document)
            .ok_or_else(IvyError::internal)?;
        let mut selected = schema;
        if let Some(start) = split.filter(|start| start + 1 < units.len()) {
            let fragment = JsString(units[start + 1..].to_vec());
            for key in crate::schema_lookup::compiler_pointer_parts(&fragment, strict_percent)? {
                selected = crate::schema_lookup::property(selected, &key).ok_or_else(|| {
                    IvyError::new("internal_error", "Unknown internal schema pointer.")
                })?;
            }
        }
        Ok((document, crate::schema_lookup::schema(selected)?))
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
