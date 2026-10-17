//! Evaluation of trusted repository schema documents against the lossless wire domain.
//! Dynamic registry schemas require their separate RE2 admission path.
use crate::{IvyError, IvyJson, IvyResult, JsString, json::parse, limits::MANAGEMENT_FRAME_BYTES};
use std::collections::HashMap;

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
        self.evaluate(document, schema, value, &mut Vec::new())
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
    fn evaluate(
        &self,
        document: usize,
        schema: &IvyJson,
        value: &IvyJson,
        trail: &mut Vec<(usize, usize)>,
    ) -> IvyResult<bool> {
        if let IvyJson::Bool(accept) = schema {
            return Ok(*accept);
        }
        if schema.as_object().is_none() {
            return Err(IvyError::new("internal_error", "Invalid internal schema."));
        }
        let pair = (
            schema as *const IvyJson as usize,
            value as *const IvyJson as usize,
        );
        if trail.contains(&pair) {
            return Err(IvyError::new(
                "internal_error",
                "Non-consuming internal schema reference.",
            ));
        }
        trail.push(pair);
        let result = self.constraints(document, schema, value, trail);
        trail.pop();
        result
    }
    fn constraints(
        &self,
        document: usize,
        schema: &IvyJson,
        value: &IvyJson,
        trail: &mut Vec<(usize, usize)>,
    ) -> IvyResult<bool> {
        if let Some(reference) = schema.get("$ref") {
            let (target_document, target) = self.resolve(document, &reference.as_str()?)?;
            if !self.evaluate(target_document, target, value, trail)? {
                return Ok(false);
            }
        }
        if let Some(types) = schema.get("type") {
            let matches = if let Some(types) = types.as_array() {
                types.iter().any(|kind| type_matches(kind, value))
            } else {
                type_matches(types, value)
            };
            if !matches {
                return Ok(false);
            }
        }
        if schema
            .get("const")
            .is_some_and(|constant| !json_equal(constant, value))
        {
            return Ok(false);
        }
        if let Some(choices) = schema.get("enum").and_then(IvyJson::as_array) {
            if !choices.iter().any(|choice| json_equal(choice, value)) {
                return Ok(false);
            }
        }
        for keyword in ["allOf", "anyOf", "oneOf"] {
            if let Some(children) = schema.get(keyword).and_then(IvyJson::as_array) {
                let mut accepted = 0;
                for child in children {
                    let matches = self.evaluate(document, child, value, trail)?;
                    if matches {
                        accepted += 1;
                    }
                    if keyword == "allOf" && !matches {
                        return Ok(false);
                    }
                    if keyword == "anyOf" && matches {
                        break;
                    }
                    if keyword == "oneOf" && accepted > 1 {
                        return Ok(false);
                    }
                }
                if keyword != "allOf" && accepted == 0 {
                    return Ok(false);
                }
            }
        }
        if let Some(condition) = schema.get("not") {
            if self.evaluate(document, condition, value, trail)? {
                return Ok(false);
            }
        }
        if let Some(condition) = schema.get("if") {
            let branch = if self.evaluate(document, condition, value, trail)? {
                "then"
            } else {
                "else"
            };
            if let Some(branch) = schema.get(branch) {
                if !self.evaluate(document, branch, value, trail)? {
                    return Ok(false);
                }
            }
        }
        match value {
            IvyJson::Number(number) => {
                for (key, accept) in [
                    ("minimum", 0),
                    ("maximum", 1),
                    ("exclusiveMinimum", 2),
                    ("exclusiveMaximum", 3),
                ] {
                    if let Some(bound) = schema.get(key).and_then(IvyJson::as_f64) {
                        if !match accept {
                            0 => *number >= bound,
                            1 => *number <= bound,
                            2 => *number > bound,
                            _ => *number < bound,
                        } {
                            return Ok(false);
                        }
                    }
                }
                if let Some(divisor) = schema.get("multipleOf").and_then(IvyJson::as_f64) {
                    let quotient = number / divisor;
                    if !quotient.is_finite() || quotient.fract() != 0.0 {
                        return Ok(false);
                    }
                }
            }
            IvyJson::String(string) => {
                if !length_matches(schema, "minLength", "maxLength", string.code_point_len()) {
                    return Ok(false);
                }
                if let Some(pattern) = schema.get("pattern").and_then(IvyJson::as_js_string) {
                    let expression = self.patterns.get(pattern).ok_or_else(IvyError::internal)?;
                    if expression.find_from_utf16(&string.0, 0).next().is_none() {
                        return Ok(false);
                    }
                }
            }
            IvyJson::Array(values) => {
                if !length_matches(schema, "minItems", "maxItems", values.len()) {
                    return Ok(false);
                }
                if schema.get("uniqueItems").and_then(IvyJson::as_bool) == Some(true) {
                    for (index, value) in values.iter().enumerate() {
                        if values[..index].iter().any(|prior| json_equal(prior, value)) {
                            return Ok(false);
                        }
                    }
                }
                if let Some(items) = schema.get("items") {
                    for value in values {
                        if !self.evaluate(document, items, value, trail)? {
                            return Ok(false);
                        }
                    }
                }
            }
            IvyJson::Object(properties) => {
                if !length_matches(schema, "minProperties", "maxProperties", properties.len()) {
                    return Ok(false);
                }
                if let Some(required) = schema.get("required").and_then(IvyJson::as_array) {
                    for key in required {
                        if value
                            .get_key(key.as_js_string().ok_or_else(IvyError::internal)?)
                            .is_none()
                        {
                            return Ok(false);
                        }
                    }
                }
                if let Some(dependencies) =
                    schema.get("dependentRequired").and_then(IvyJson::as_object)
                {
                    for (key, required) in dependencies {
                        if value.get_key(key).is_some() {
                            for dependency in required.as_array().ok_or_else(IvyError::internal)? {
                                if value
                                    .get_key(
                                        dependency.as_js_string().ok_or_else(IvyError::internal)?,
                                    )
                                    .is_none()
                                {
                                    return Ok(false);
                                }
                            }
                        }
                    }
                }
                for (key, child) in properties {
                    if let Some(names) = schema.get("propertyNames") {
                        if !self.evaluate(document, names, &IvyJson::String(key.clone()), trail)? {
                            return Ok(false);
                        }
                    }
                    let mut declared = false;
                    if let Some(property) = schema
                        .get("properties")
                        .and_then(|properties| properties.get_key(key))
                    {
                        declared = true;
                        if !self.evaluate(document, property, child, trail)? {
                            return Ok(false);
                        }
                    }
                    if let Some(patterns) =
                        schema.get("patternProperties").and_then(IvyJson::as_object)
                    {
                        for (pattern, property) in patterns {
                            if self
                                .patterns
                                .get(pattern)
                                .ok_or_else(IvyError::internal)?
                                .find_from_utf16(&key.0, 0)
                                .next()
                                .is_some()
                            {
                                declared = true;
                                if !self.evaluate(document, property, child, trail)? {
                                    return Ok(false);
                                }
                            }
                        }
                    }
                    if !declared {
                        if let Some(additional) = schema.get("additionalProperties") {
                            if !self.evaluate(document, additional, child, trail)? {
                                return Ok(false);
                            }
                        }
                    }
                }
            }
            _ => {}
        }
        Ok(true)
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
    match (left, right) {
        (IvyJson::Object(a), IvyJson::Object(b)) => {
            a.len() == b.len()
                && a.iter().all(|(key, value)| {
                    right
                        .get_key(key)
                        .is_some_and(|other| json_equal(value, other))
                })
        }
        (IvyJson::Array(a), IvyJson::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(a, b)| json_equal(a, b))
        }
        _ => left == right,
    }
}
