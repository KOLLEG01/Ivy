//! Explicit continuations keep recursive schema references off the native stack.
use super::*;
use std::collections::HashSet;

#[derive(Clone, Copy)]
enum Input<'a> {
    Value(&'a IvyJson),
    Key(&'a JsString),
}
impl Input<'_> {
    fn identity(self) -> (u8, usize) {
        match self {
            Self::Value(v) => (0, v as *const _ as usize),
            Self::Key(k) => (1, k as *const _ as usize),
        }
    }
    fn with_value<T>(self, call: impl FnOnce(&IvyJson) -> T) -> T {
        match self {
            Self::Value(v) => call(v),
            Self::Key(k) => call(&IvyJson::String(k.clone())),
        }
    }
}
enum Check<'a> {
    Evaluate(usize, &'a IvyJson, Input<'a>),
    Simple(&'a IvyJson, Input<'a>),
    Specific(usize, &'a IvyJson, Input<'a>),
    Group(Group, Vec<Check<'a>>),
    Not(usize, &'a IvyJson, Input<'a>),
    If(usize, &'a IvyJson, Input<'a>),
}
#[derive(Clone, Copy)]
enum Group {
    All,
    Any,
    One,
}
type Identity = (usize, u8, usize);
enum Step<'a> {
    Check(Check<'a>),
    Exit(Identity),
    Group(Group, std::vec::IntoIter<Check<'a>>, usize),
    Not,
    If(usize, &'a IvyJson, Input<'a>),
}
impl SchemaBundle {
    pub(super) fn evaluate(
        &self,
        document: usize,
        schema: &IvyJson,
        value: &IvyJson,
    ) -> IvyResult<bool> {
        let entry = (document, schema);
        let mut pending = vec![Step::Check(Check::Evaluate(
            document,
            schema,
            Input::Value(value),
        ))];
        let mut active = HashSet::new();
        // AJV's dynamic-anchor environment is shared by the complete validation:
        // the first encountered definition wins, including across sibling refs.
        let mut anchors = HashMap::<JsString, (usize, &IvyJson)>::new();
        let mut result = true;
        while let Some(step) = pending.pop() {
            let check = match step {
                Step::Exit(pair) => {
                    active.remove(&pair);
                    continue;
                }
                Step::Not => {
                    result = !result;
                    continue;
                }
                Step::If(doc, schema, value) => {
                    if let Some(branch) = schema.get(if result { "then" } else { "else" }) {
                        pending.push(Step::Check(Check::Evaluate(doc, branch, value)));
                    } else {
                        result = true;
                    }
                    continue;
                }
                Step::Group(kind, mut remaining, accepted) => {
                    let accepted = accepted + usize::from(result);
                    if matches!(kind, Group::All) && !result {
                        continue;
                    }
                    if matches!(kind, Group::Any) && result {
                        continue;
                    }
                    if matches!(kind, Group::One) && accepted > 1 {
                        result = false;
                        continue;
                    }
                    if let Some(next) = remaining.next() {
                        pending.push(Step::Group(kind, remaining, accepted));
                        pending.push(Step::Check(next));
                    } else {
                        result = match kind {
                            Group::All => true,
                            Group::Any => accepted > 0,
                            Group::One => accepted == 1,
                        };
                    }
                    continue;
                }
                Step::Check(check) => check,
            };
            match check {
                Check::Group(kind, children) => {
                    let mut remaining = children.into_iter();
                    if let Some(first) = remaining.next() {
                        pending.push(Step::Group(kind, remaining, 0));
                        pending.push(Step::Check(first));
                    } else {
                        result = matches!(kind, Group::All);
                    }
                }
                Check::Not(doc, schema, value) => {
                    pending.push(Step::Not);
                    pending.push(Step::Check(Check::Evaluate(doc, schema, value)));
                }
                Check::If(doc, schema, value) => {
                    pending.push(Step::If(doc, schema, value));
                    pending.push(Step::Check(Check::Evaluate(
                        doc,
                        schema.get("if").unwrap(),
                        value,
                    )));
                }
                Check::Simple(schema, value) => {
                    result = value.with_value(|value| {
                        if let Some(types) = schema.get("type") {
                            let matches = types.as_array().map_or_else(
                                || type_matches(types, value),
                                |types| types.iter().any(|kind| type_matches(kind, value)),
                            );
                            if !matches {
                                return false;
                            }
                        }
                        if schema
                            .get("const")
                            .is_some_and(|constant| !json_equal(constant, value))
                        {
                            return false;
                        }
                        schema
                            .get("enum")
                            .and_then(IvyJson::as_array)
                            .is_none_or(|choices| {
                                choices.iter().any(|choice| json_equal(choice, value))
                            })
                    })
                }
                Check::Specific(doc, schema, input) => {
                    result = input.with_value(|value| scalar_constraints(self, schema, value))?;
                    if !result {
                        continue;
                    }
                    let Input::Value(value) = input else {
                        continue;
                    };
                    match child_constraints(self, doc, schema, value)? {
                        Some(children) => {
                            pending.push(Step::Check(Check::Group(Group::All, children)))
                        }
                        None => result = false,
                    }
                }
                Check::Evaluate(doc, schema, value) => {
                    if let IvyJson::Bool(accept) = schema {
                        result = *accept;
                        continue;
                    }
                    if schema.as_object().is_none() {
                        return Err(IvyError::new("internal_error", "Invalid internal schema."));
                    }
                    let (kind, address) = value.identity();
                    let identity = (schema as *const _ as usize, kind, address);
                    if !active.insert(identity) {
                        return Err(IvyError::new(
                            "internal_error",
                            "Non-consuming internal schema reference.",
                        ));
                    }
                    pending.push(Step::Exit(identity));
                    let mut checks = Vec::new();
                    if let Some(anchor) =
                        schema.get("$dynamicAnchor").and_then(IvyJson::as_js_string)
                    {
                        anchors.entry(anchor.clone()).or_insert((doc, schema));
                    }
                    if let Some(reference) =
                        schema.get("$dynamicRef").and_then(IvyJson::as_js_string)
                    {
                        if reference.0.first() != Some(&35) {
                            return Err(IvyError::internal());
                        }
                        let anchor = JsString(reference.0[1..].to_vec());
                        let (target_doc, target) = anchors.get(&anchor).copied().unwrap_or(entry);
                        checks.push(Check::Evaluate(target_doc, target, value));
                    }
                    if let Some(reference) = schema.get("$ref") {
                        let (target_doc, target) = self.resolve(doc, &reference.as_str()?)?;
                        checks.push(Check::Evaluate(target_doc, target, value));
                    }
                    checks.push(Check::Simple(schema, value));
                    for (keyword, kind) in [
                        ("allOf", Group::All),
                        ("anyOf", Group::Any),
                        ("oneOf", Group::One),
                    ] {
                        if let Some(children) = schema.get(keyword).and_then(IvyJson::as_array) {
                            checks.push(Check::Group(
                                kind,
                                children
                                    .iter()
                                    .map(|child| Check::Evaluate(doc, child, value))
                                    .collect(),
                            ));
                        }
                    }
                    if let Some(condition) = schema.get("not") {
                        checks.push(Check::Not(doc, condition, value));
                    }
                    if schema.get("if").is_some() {
                        checks.push(Check::If(doc, schema, value));
                    }
                    checks.push(Check::Specific(doc, schema, value));
                    pending.push(Step::Check(Check::Group(Group::All, checks)));
                }
            }
        }
        Ok(result)
    }
}

fn scalar_constraints(bundle: &SchemaBundle, schema: &IvyJson, value: &IvyJson) -> IvyResult<bool> {
    match value {
        IvyJson::Number(number) => {
            for (key, comparison) in [
                ("minimum", 0),
                ("maximum", 1),
                ("exclusiveMinimum", 2),
                ("exclusiveMaximum", 3),
            ] {
                if let Some(bound) = schema.get(key).and_then(IvyJson::as_f64) {
                    if !match comparison {
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
                if bundle
                    .patterns
                    .get(pattern)
                    .ok_or_else(IvyError::internal)?
                    .find_from_utf16(&string.0, 0)
                    .next()
                    .is_none()
                {
                    return Ok(false);
                }
            }
        }
        _ => {}
    }
    Ok(true)
}

fn child_constraints<'a>(
    bundle: &SchemaBundle,
    doc: usize,
    schema: &'a IvyJson,
    value: &'a IvyJson,
) -> IvyResult<Option<Vec<Check<'a>>>> {
    let mut checks = Vec::new();
    match value {
        IvyJson::Array(values) => {
            if !length_matches(schema, "minItems", "maxItems", values.len()) {
                return Ok(None);
            }
            if schema.get("uniqueItems").and_then(IvyJson::as_bool) == Some(true) {
                for (index, value) in values.iter().enumerate() {
                    if values[..index].iter().any(|prior| json_equal(prior, value)) {
                        return Ok(None);
                    }
                }
            }
            if let Some(items) = schema.get("items") {
                checks.extend(
                    values
                        .iter()
                        .map(|value| Check::Evaluate(doc, items, Input::Value(value))),
                );
            }
        }
        IvyJson::Object(properties) => {
            if !length_matches(schema, "minProperties", "maxProperties", properties.len()) {
                return Ok(None);
            }
            if let Some(required) = schema.get("required").and_then(IvyJson::as_array) {
                for key in required {
                    if value
                        .get_key(key.as_js_string().ok_or_else(IvyError::internal)?)
                        .is_none()
                    {
                        return Ok(None);
                    }
                }
            }
            if let Some(dependencies) = schema.get("dependentRequired").and_then(IvyJson::as_object)
            {
                for (key, required) in dependencies {
                    if value.get_key(key).is_some() {
                        for dependency in required.as_array().ok_or_else(IvyError::internal)? {
                            if value
                                .get_key(dependency.as_js_string().ok_or_else(IvyError::internal)?)
                                .is_none()
                            {
                                return Ok(None);
                            }
                        }
                    }
                }
            }
            for (key, child) in properties {
                if let Some(names) = schema.get("propertyNames") {
                    checks.push(Check::Evaluate(doc, names, Input::Key(key)));
                }
                let mut declared = false;
                if let Some(property) = schema
                    .get("properties")
                    .and_then(|properties| properties.get_key(key))
                {
                    declared = true;
                    checks.push(Check::Evaluate(doc, property, Input::Value(child)));
                }
                if let Some(patterns) = schema.get("patternProperties").and_then(IvyJson::as_object)
                {
                    for (pattern, property) in patterns {
                        if bundle
                            .patterns
                            .get(pattern)
                            .ok_or_else(IvyError::internal)?
                            .find_from_utf16(&key.0, 0)
                            .next()
                            .is_some()
                        {
                            declared = true;
                            checks.push(Check::Evaluate(doc, property, Input::Value(child)));
                        }
                    }
                }
                if !declared {
                    if let Some(additional) = schema.get("additionalProperties") {
                        checks.push(Check::Evaluate(doc, additional, Input::Value(child)));
                    }
                }
            }
        }
        _ => {}
    }
    Ok(Some(checks))
}
