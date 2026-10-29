//! Explicit continuations evaluate consuming references without native recursion.
use super::*;
use std::collections::HashSet;

#[derive(Clone, Copy)]
enum Input<'a> {
    Value(&'a IvyJson, usize),
    Key(&'a JsString, usize),
}
impl Input<'_> {
    fn with<T>(self, f: impl FnOnce(&IvyJson) -> T) -> T {
        match self {
            Self::Value(value, _) => f(value),
            Self::Key(key, _) => f(&IvyJson::String(key.clone())),
        }
    }
    fn identity(self) -> (u8, usize) {
        match self {
            Self::Value(v, _) => (0, v as *const _ as usize),
            Self::Key(v, _) => (1, v as *const _ as usize),
        }
    }
    fn root(self) -> usize {
        match self {
            Self::Value(_, root) | Self::Key(_, root) => root,
        }
    }
    fn with_root(self, root: usize) -> Self {
        match self {
            Self::Value(value, _) => Self::Value(value, root),
            Self::Key(key, _) => Self::Key(key, root),
        }
    }
}
#[derive(Clone, Default)]
struct Marks {
    properties: HashSet<JsString>,
    all_properties: bool,
    items: usize,
    all_items: bool,
}
impl Marks {
    fn merge(&mut self, other: &Self) {
        self.all_properties |= other.all_properties;
        self.all_items |= other.all_items;
        self.items = self.items.max(other.items);
        self.properties.extend(other.properties.iter().cloned());
    }
}
#[derive(Default)]
struct ResultValue {
    valid: bool,
    marks: Marks,
}
impl ResultValue {
    fn new(valid: bool) -> Self {
        Self {
            valid,
            marks: Marks::default(),
        }
    }
}
#[derive(Clone, Copy)]
enum Group {
    All,
    Any(bool),
    One,
}
enum Check<'a> {
    Eval(usize, Input<'a>),
    Type(usize, Input<'a>),
    Literal(usize, Input<'a>),
    Specific(usize, Input<'a>, Marks),
    Dependencies(usize, &'static str, Input<'a>),
    Unique(usize, &'a [IvyJson]),
    Not(usize, Input<'a>),
    If(usize, Input<'a>),
    Contains(usize, &'a [IvyJson], usize),
    Unevaluated(usize, Input<'a>, Marks),
    Child(usize, Input<'a>),
    Group(Group, Vec<Check<'a>>, Marks),
}
type Identity = (usize, u8, usize, usize);
enum Step<'a> {
    Check(Check<'a>),
    Exit(Identity),
    Not,
    Discard,
    If(usize, Input<'a>),
    Branch(Marks),
    Group(Group, std::vec::IntoIter<Check<'a>>, usize, Marks),
    Contains(usize, std::slice::Iter<'a, IvyJson>, usize, usize),
}
fn with_prior<'a>(check: Check<'a>, prior: &Marks) -> Check<'a> {
    match check {
        Check::Specific(id, value, _) => Check::Specific(id, value, prior.clone()),
        Check::Unevaluated(id, value, _) => Check::Unevaluated(id, value, prior.clone()),
        other => other,
    }
}
impl CompiledSchema {
    pub(super) fn evaluate(&self, value: &IvyJson) -> IvyResult<bool> {
        let mut pending = vec![Step::Check(Check::Eval(
            self.root,
            Input::Value(value, self.root),
        ))];
        let mut result = ResultValue::new(true);
        let mut active = HashSet::new();
        let mut anchors = HashMap::<JsString, usize>::new();
        while let Some(step) = pending.pop() {
            let check = match step {
                Step::Exit(key) => {
                    active.remove(&key);
                    continue;
                }
                Step::Not => {
                    result = ResultValue::new(!result.valid);
                    continue;
                }
                Step::Discard => {
                    result.marks = Marks::default();
                    continue;
                }
                Step::Branch(marks) => {
                    result.marks.merge(&marks);
                    continue;
                }
                Step::If(id, input) => {
                    let marks = if result.valid {
                        std::mem::take(&mut result.marks)
                    } else {
                        Marks::default()
                    };
                    let branch = self.nodes[id].child(if result.valid { "then" } else { "else" });
                    if let Some(branch) = branch {
                        pending.push(Step::Branch(marks));
                        pending.push(Step::Check(Check::Eval(branch, input)));
                    } else {
                        result = ResultValue { valid: true, marks };
                    }
                    continue;
                }
                Step::Group(kind, mut remaining, accepted, mut marks) => {
                    let accepted = accepted + usize::from(result.valid);
                    if result.valid || matches!(kind, Group::All) {
                        marks.merge(&result.marks);
                    }
                    if matches!(kind, Group::All) && !result.valid {
                        result.marks = marks;
                        continue;
                    }
                    if matches!(kind, Group::One) && accepted > 1 {
                        result = ResultValue {
                            valid: false,
                            marks,
                        };
                        continue;
                    }
                    // Only a prior unconditional reference covering all properties
                    // and items permits the original union to short-circuit.
                    if matches!(kind, Group::Any(true)) && accepted > 0 {
                        result = ResultValue { valid: true, marks };
                        continue;
                    }
                    if let Some(next) = remaining.next() {
                        let next = with_prior(next, &marks);
                        pending.push(Step::Group(kind, remaining, accepted, marks));
                        pending.push(Step::Check(next));
                    } else {
                        result = ResultValue {
                            valid: match kind {
                                Group::All => true,
                                Group::Any(_) => accepted > 0,
                                Group::One => accepted == 1,
                            },
                            marks,
                        };
                    }
                    continue;
                }
                Step::Contains(id, mut remaining, count, root) => {
                    let count = count + usize::from(result.valid);
                    let schema = &self.nodes[id].schema;
                    let min = schema
                        .get("minContains")
                        .and_then(IvyJson::as_f64)
                        .unwrap_or(1.0);
                    let max = schema.get("maxContains").and_then(IvyJson::as_f64);
                    if max.is_some_and(|max| count as f64 > max) {
                        result = ResultValue::new(false);
                        continue;
                    }
                    if max.is_none() && count as f64 >= min {
                        result = ResultValue::new(true);
                        result.marks.all_items = true;
                        continue;
                    }
                    if let Some(value) = remaining.next() {
                        pending.push(Step::Contains(id, remaining, count, root));
                        pending.push(Step::Check(Check::Eval(
                            self.nodes[id].child("contains").unwrap(),
                            Input::Value(value, root),
                        )));
                    } else {
                        result = ResultValue::new(count as f64 >= min);
                        result.marks.all_items = true;
                    }
                    continue;
                }
                Step::Check(check) => check,
            };
            match check {
                Check::Dependencies(id, keyword, input) => {
                    result = ResultValue::new(
                        input.with(|value| dependencies(&self.nodes[id].schema, keyword, value))?,
                    );
                }
                Check::Group(kind, children, marks) => {
                    let mut remaining = children.into_iter();
                    if let Some(next) = remaining.next() {
                        pending.push(Step::Group(kind, remaining, 0, marks.clone()));
                        pending.push(Step::Check(with_prior(next, &marks)));
                    } else {
                        result = ResultValue {
                            valid: matches!(kind, Group::All),
                            marks,
                        };
                    }
                }
                Check::Child(id, input) => {
                    pending.push(Step::Discard);
                    pending.push(Step::Check(Check::Eval(id, input)));
                }
                Check::Not(id, input) => {
                    pending.push(Step::Not);
                    pending.push(Step::Check(Check::Eval(id, input)));
                }
                Check::If(id, input) => {
                    pending.push(Step::If(id, input));
                    pending.push(Step::Check(Check::Eval(
                        self.nodes[id].child("if").unwrap(),
                        input,
                    )));
                }
                Check::Type(id, input) => {
                    result = ResultValue::new(
                        input.with(|value| type_matches(&self.nodes[id].schema, value)),
                    )
                }
                Check::Literal(id, input) => {
                    let schema = &self.nodes[id].schema;
                    result = ResultValue::new(input.with(|value| {
                        if let Some(constant) = schema.get("const") {
                            if !json_equal(value, constant) {
                                return false;
                            }
                        }
                        if let Some(choices) = schema.get("enum").and_then(IvyJson::as_array) {
                            for choice in choices {
                                if json_equal(value, choice) {
                                    return true;
                                }
                            }
                            return false;
                        }
                        true
                    }));
                }
                Check::Unique(id, values) => result = ResultValue::new(self.unique(id, values)?),
                Check::Contains(id, values, root) => {
                    let schema = &self.nodes[id].schema;
                    let min = schema
                        .get("minContains")
                        .and_then(IvyJson::as_f64)
                        .unwrap_or(1.0);
                    let max = schema.get("maxContains").and_then(IvyJson::as_f64);
                    if max.is_none() && min == 0.0 {
                        result = ResultValue::new(true);
                        continue;
                    }
                    if max.is_some_and(|max| min > max) {
                        result = ResultValue::new(false);
                        continue;
                    }
                    let child = self.nodes[id].child("contains").unwrap();
                    if child == 0 {
                        result = ResultValue::new(
                            values.len() as f64 >= min
                                && max.is_none_or(|max| values.len() as f64 <= max),
                        );
                        continue;
                    }
                    let mut remaining = values.iter();
                    if let Some(value) = remaining.next() {
                        pending.push(Step::Contains(id, remaining, 0, root));
                        pending.push(Step::Check(Check::Eval(child, Input::Value(value, root))));
                    } else {
                        result = ResultValue::new(min <= 0.0);
                        result.marks.all_items = true;
                    }
                }
                Check::Unevaluated(id, input, mut marks) => {
                    let node = &self.nodes[id];
                    let mut checks = Vec::new();
                    if let Input::Value(value, root) = input {
                        match value {
                            IvyJson::Object(_) if !marks.all_properties => {
                                if let Some(child) = node.child("unevaluatedProperties") {
                                    for (key, value) in entries(value) {
                                        if !marks.properties.contains(key) {
                                            checks.push(Check::Child(
                                                child,
                                                Input::Value(value, root),
                                            ));
                                        }
                                    }
                                    marks.all_properties = true;
                                }
                            }
                            IvyJson::Array(values) if !marks.all_items => {
                                if let Some(child) = node.child("unevaluatedItems") {
                                    checks.extend(values.iter().skip(marks.items).map(|value| {
                                        Check::Child(child, Input::Value(value, root))
                                    }));
                                    marks.all_items = true;
                                }
                            }
                            _ => {}
                        }
                    }
                    pending.push(Step::Check(Check::Group(Group::All, checks, marks)));
                }
                Check::Specific(id, input, prior) => {
                    let node = &self.nodes[id];
                    if !input.with(|value| self.scalar(&node.schema, value))? {
                        result = ResultValue::new(false);
                        continue;
                    }
                    let mut checks = Vec::new();
                    let mut marks = prior;
                    if let Input::Value(value, root) = input {
                        match value {
                            IvyJson::Array(values) => {
                                if !length(&node.schema, "minItems", "maxItems", values.len()) {
                                    result = ResultValue::new(false);
                                    continue;
                                }
                                let prefix = node.children("prefixItems");
                                marks.items = marks.items.max(prefix.len());
                                checks.extend(prefix.iter().zip(values).map(|((_, id), value)| {
                                    Check::Child(*id, Input::Value(value, root))
                                }));
                                if let Some(child) = node.child("items") {
                                    marks.all_items = true;
                                    checks.extend(values.iter().skip(prefix.len()).map(|value| {
                                        Check::Child(child, Input::Value(value, root))
                                    }));
                                }
                                if node.child("contains").is_some() {
                                    checks.push(Check::Contains(id, values, root));
                                }
                                if node.schema.get("uniqueItems").and_then(IvyJson::as_bool)
                                    == Some(true)
                                {
                                    checks.push(Check::Unique(id, values));
                                }
                            }
                            IvyJson::Object(properties) => {
                                if !length(
                                    &node.schema,
                                    "minProperties",
                                    "maxProperties",
                                    properties.len(),
                                ) {
                                    result = ResultValue::new(false);
                                    continue;
                                }
                                if let Some(required) =
                                    node.schema.get("required").and_then(IvyJson::as_array)
                                {
                                    if !required_keys(value, required)? {
                                        result = ResultValue::new(false);
                                        continue;
                                    }
                                }
                                if let Some(child) = node.child("propertyNames") {
                                    checks.extend(entries(value).into_iter().map(|(key, _)| {
                                        Check::Child(child, Input::Key(key, root))
                                    }));
                                }
                                if let Some(child) = node.child("additionalProperties") {
                                    marks.all_properties = true;
                                    for (key, value) in
                                        entries(value).into_iter().filter(|_| child != 0)
                                    {
                                        if !node
                                            .children("properties")
                                            .iter()
                                            .any(|(name, _)| name == key)
                                            && !node.children("patternProperties").iter().any(
                                                |(pattern, _)| self.pattern(pattern).is_match(key),
                                            )
                                        {
                                            checks.push(Check::Child(
                                                child,
                                                Input::Value(value, root),
                                            ));
                                        }
                                    }
                                }
                                checks.push(Check::Dependencies(id, "dependencies", input));
                                checks.extend(
                                    node.children("dependencies")
                                        .iter()
                                        .filter(|(key, _)| value.get_key(key).is_some())
                                        .map(|(_, child)| Check::Eval(*child, input)),
                                );
                                for (key, child) in node.children("properties") {
                                    marks.properties.insert(key.clone());
                                    if let Some(value) = value.get_key(key) {
                                        checks
                                            .push(Check::Child(*child, Input::Value(value, root)));
                                    }
                                }
                                for (pattern, child) in node.children("patternProperties") {
                                    for (key, value) in entries(value) {
                                        if self.pattern(pattern).is_match(key) {
                                            marks.properties.insert(key.clone());
                                            checks.push(Check::Child(
                                                *child,
                                                Input::Value(value, root),
                                            ));
                                        }
                                    }
                                }
                                // These errors occur after properties/patternProperties.
                                checks.push(Check::Dependencies(id, "dependentRequired", input));
                                checks.extend(
                                    node.children("dependentSchemas")
                                        .iter()
                                        .filter(|(key, _)| value.get_key(key).is_some())
                                        .map(|(_, child)| Check::Eval(*child, input)),
                                );
                            }
                            _ => {}
                        }
                    }
                    checks.push(Check::Unevaluated(id, input, Marks::default()));
                    pending.push(Step::Check(Check::Group(Group::All, checks, marks)));
                }
                Check::Eval(id, input) => {
                    let node = &self.nodes[id];
                    if let IvyJson::Bool(accept) = node.schema {
                        result = ResultValue::new(accept);
                        continue;
                    }
                    let (kind, address) = input.identity();
                    let key = (id, kind, address, input.root());
                    if !active.insert(key) {
                        return Err(IvyError::internal());
                    }
                    pending.push(Step::Exit(key));
                    for anchor in node
                        .schema
                        .get("$dynamicAnchor")
                        .and_then(IvyJson::as_js_string)
                        .into_iter()
                    {
                        anchors.entry(anchor.clone()).or_insert(id);
                    }
                    if node
                        .schema
                        .get("$recursiveAnchor")
                        .and_then(IvyJson::as_bool)
                        == Some(true)
                    {
                        anchors.entry(JsString::default()).or_insert(id);
                    }
                    let mut checks = Vec::new();
                    if early_type(node) {
                        checks.push(Check::Type(id, input));
                    }
                    if let Some(reference) = node
                        .schema
                        .get("$dynamicRef")
                        .or_else(|| node.schema.get("$recursiveRef"))
                        .and_then(IvyJson::as_js_string)
                    {
                        let anchor = JsString(reference.0[1..].to_vec());
                        let reference = if self
                            .dynamic_targets
                            .binary_search(&(id, input.root()))
                            .is_ok()
                        {
                            *anchors.get(&anchor).unwrap_or(&input.root())
                        } else {
                            input.root()
                        };
                        checks.push(Check::Eval(reference, input.with_root(reference)));
                    }
                    if let Some(reference) = node.reference {
                        checks.push(Check::Eval(reference, input.with_root(reference)));
                    }
                    checks.push(Check::Literal(id, input));
                    if let Some(child) = node.child("not") {
                        checks.push(Check::Not(child, input));
                    }
                    for (keyword, kind) in [
                        (
                            "anyOf",
                            Group::Any(
                                node.reference
                                    .is_some_and(|id| self.nodes[id].static_all == 3),
                            ),
                        ),
                        ("oneOf", Group::One),
                        ("allOf", Group::All),
                    ] {
                        if node.has_children(keyword) {
                            checks.push(Check::Group(
                                kind,
                                node.children(keyword)
                                    .iter()
                                    .map(|(_, child)| Check::Eval(*child, input))
                                    .collect(),
                                Marks::default(),
                            ));
                        }
                    }
                    if node.child("if").is_some()
                        && (node.child("then").is_some_and(|id| id != 0)
                            || node.child("else").is_some_and(|id| id != 0))
                    {
                        checks.push(Check::If(id, input));
                    }
                    if !early_type(node) {
                        checks.push(Check::Type(id, input));
                    }
                    checks.push(Check::Specific(id, input, Marks::default()));
                    pending.push(Step::Check(Check::Group(
                        Group::All,
                        checks,
                        Marks::default(),
                    )));
                }
            }
        }
        Ok(result.valid)
    }
    fn scalar(&self, schema: &IvyJson, value: &IvyJson) -> IvyResult<bool> {
        match value {
            IvyJson::Number(value) => {
                for (key, kind) in [
                    ("maximum", 0),
                    ("minimum", 1),
                    ("exclusiveMaximum", 2),
                    ("exclusiveMinimum", 3),
                ] {
                    if let Some(bound) = schema.get(key).and_then(IvyJson::as_f64) {
                        if !match kind {
                            0 => *value <= bound,
                            1 => *value >= bound,
                            2 => *value < bound,
                            _ => *value > bound,
                        } {
                            return Ok(false);
                        }
                    }
                }
                if let Some(divisor) = schema.get("multipleOf").and_then(IvyJson::as_f64) {
                    let quotient = value / divisor;
                    if !quotient.is_finite() || divisor == 0.0 {
                        return Ok(false);
                    }
                    let mut buffer = ryu_js::Buffer::new();
                    let text = buffer.format(quotient);
                    let end = text
                        .as_bytes()
                        .iter()
                        .enumerate()
                        .find(|(i, byte)| !(byte.is_ascii_digit() || (*i == 0 && **byte == b'-')))
                        .map_or(text.len(), |(i, _)| i);
                    if text[..end].parse::<f64>().ok() != Some(quotient) {
                        return Ok(false);
                    }
                }
            }
            IvyJson::String(value) => {
                if !length(schema, "minLength", "maxLength", value.code_point_len()) {
                    return Ok(false);
                }
                if let Some(pattern) = schema.get("pattern").and_then(IvyJson::as_js_string) {
                    if !self.pattern(pattern).is_match(value) {
                        return Ok(false);
                    }
                }
            }
            _ => {}
        }
        Ok(true)
    }
    fn unique(&self, _id: usize, values: &[IvyJson]) -> IvyResult<bool> {
        let mut seen = HashSet::with_capacity(values.len());
        for value in values {
            if !seen.insert(canonical(value, usize::MAX)?) {
                return Ok(false);
            }
        }
        Ok(true)
    }
}
fn required_keys(value: &IvyJson, keys: &[IvyJson]) -> IvyResult<bool> {
    for key in keys {
        let name = key.as_js_string().ok_or_else(invalid)?;
        if value.get_key(name).is_none() {
            return Ok(false);
        }
    }
    Ok(true)
}
fn dependencies(schema: &IvyJson, keyword: &str, value: &IvyJson) -> IvyResult<bool> {
    if let Some(map) = schema.get(keyword) {
        for (key, required) in entries(map) {
            if value.get_key(key).is_none() {
                continue;
            }
            if let Some(required) = required.as_array() {
                if !required_keys(value, required)? {
                    return Ok(false);
                }
            }
        }
    }
    Ok(true)
}
fn length(schema: &IvyJson, min: &str, max: &str, size: usize) -> bool {
    schema
        .get(min)
        .and_then(IvyJson::as_f64)
        .is_none_or(|min| size as f64 >= min)
        && schema
            .get(max)
            .and_then(IvyJson::as_f64)
            .is_none_or(|max| size as f64 <= max)
}
fn early_type(node: &Node) -> bool {
    let Some(types) = node.schema.get("type") else {
        return true;
    };
    let types = types
        .as_array()
        .map_or_else(|| vec![types], |values| values.iter().collect());
    if types.len() != 1 || node.schema.get("nullable").and_then(IvyJson::as_bool) == Some(true) {
        return true;
    }
    let keywords: &[&str] = match types[0].as_str().ok().as_deref() {
        Some("number") => &[
            "minimum",
            "maximum",
            "exclusiveMinimum",
            "exclusiveMaximum",
            "multipleOf",
            "format",
        ],
        Some("string") => &["minLength", "maxLength", "pattern", "format"],
        Some("array") => &[
            "minItems",
            "maxItems",
            "uniqueItems",
            "minContains",
            "maxContains",
            "prefixItems",
            "items",
            "contains",
            "unevaluatedItems",
        ],
        Some("object") => &[
            "minProperties",
            "maxProperties",
            "required",
            "propertyNames",
            "additionalProperties",
            "dependencies",
            "properties",
            "patternProperties",
            "dependentRequired",
            "dependentSchemas",
            "unevaluatedProperties",
        ],
        _ => return true,
    };
    !keywords
        .iter()
        .any(|keyword| node.schema.get(keyword).is_some() || node.has_children(keyword))
}
