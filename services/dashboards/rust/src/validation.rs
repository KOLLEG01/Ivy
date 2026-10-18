//! Runtime parsing for the service's pinned Zod schemas. Registry JSON Schema
//! alone cannot express normalization, refinements or the public issue messages.
use ivy_contracts::{IvyError, IvyJson as J, IvyResult, JsString, json};

const SAFE: f64 = 9_007_199_254_740_991.0;
const METADATA: &str =
    "Metadata must fit in 8000 encoded bytes; refreshSeconds uses the refresh setting.";

#[derive(Clone, Copy)]
enum Rule {
    Text(usize, usize, bool),
    Integer(f64, f64, bool),
    Token,
    Color,
    Object(&'static str),
    Sources,
    Source,
    Metadata,
}
struct Parsed {
    value: J,
    issues: Vec<J>,
    aborted: bool,
}
impl Parsed {
    fn new(value: J) -> Self {
        Self {
            value,
            issues: vec![],
            aborted: false,
        }
    }
    fn issue(&mut self, mut issue: J, path: &[JsString], message: impl Into<JsString>) {
        issue.insert(
            "path",
            J::Array(path.iter().cloned().map(J::String).collect()),
        );
        issue.insert("message", J::String(message.into()));
        self.issues.push(issue);
    }
    fn custom(&mut self, path: &[JsString], message: &str) {
        self.issue(J::object([("code", "custom".into())]), path, message);
    }
    fn invalid(&mut self, expected: &str, input: Option<&J>, path: &[JsString], int: bool) {
        let received = match input {
            None => "undefined",
            Some(J::Null) => "null",
            Some(J::Bool(_)) => "boolean",
            Some(J::Number(_)) => "number",
            Some(J::String(_)) => "string",
            Some(J::Array(_)) => "array",
            Some(J::Object(_)) => "object",
        };
        let mut issue = J::object([("expected", expected.into())]);
        if int {
            issue.insert("format", "safeint".into());
        }
        issue.insert("code", "invalid_type".into());
        self.issue(
            issue,
            path,
            format!("Invalid input: expected {expected}, received {received}"),
        );
        self.aborted = true;
    }
    fn bound(
        &mut self,
        origin: &str,
        max: bool,
        limit: f64,
        inclusive: bool,
        path: &[JsString],
        safe: bool,
    ) {
        let code = if max { "too_big" } else { "too_small" };
        let key = if max { "maximum" } else { "minimum" };
        let mut issue = if safe {
            J::object([
                ("code", code.into()),
                (key, J::Number(limit)),
                (
                    "note",
                    "Integers must be within the safe integer range.".into(),
                ),
                ("origin", origin.into()),
                ("inclusive", J::Bool(inclusive)),
            ])
        } else {
            J::object([
                ("origin", origin.into()),
                ("code", code.into()),
                (key, J::Number(limit)),
                ("inclusive", J::Bool(inclusive)),
            ])
        };
        let adjective = if max { "big" } else { "small" };
        let comparison = match (max, inclusive) {
            (true, true) => "<=",
            (true, false) => "<",
            (false, true) => ">=",
            (false, false) => ">",
        };
        let message = if origin == "string" {
            format!("Too {adjective}: expected string to have {comparison}{limit} characters")
        } else if origin == "array" {
            format!("Too {adjective}: expected array to have {comparison}{limit} items")
        } else {
            format!("Too {adjective}: expected {origin} to be {comparison}{limit}")
        };
        // Preserve issue property insertion order: it is part of error.message.
        issue.insert(
            "path",
            J::Array(path.iter().cloned().map(J::String).collect()),
        );
        issue.insert("message", message.into());
        self.issues.push(issue);
    }
}

fn trim(unit: &u16) -> bool {
    matches!(*unit, 0x9..=0xd | 0x20 | 0xa0 | 0x1680 | 0x2000..=0x200a | 0x2028 | 0x2029 | 0x202f | 0x205f | 0x3000 | 0xfeff)
}

fn parse(rule: Rule, input: Option<&J>, path: &[JsString]) -> IvyResult<Parsed> {
    let mut result = Parsed::new(input.cloned().unwrap_or(J::Null));
    match rule {
        Rule::Text(min, max, trimming) => {
            let Some(text) = input.and_then(J::as_js_string) else {
                result.invalid("string", input, path, false);
                if let Some(values) = input.and_then(J::as_array) {
                    if values.len() < min {
                        result.bound("array", false, min as f64, true, path, false);
                    }
                    if values.len() > max {
                        result.bound("array", true, max as f64, true, path, false);
                    }
                }
                if let Some(length) = input.and_then(|v| v.get("length")) {
                    let n = numeric(length);
                    if min > 0 && !(n >= min as f64) {
                        result.bound("unknown", false, min as f64, true, path, false);
                    }
                    if max < usize::MAX && !(n <= max as f64) {
                        result.bound("unknown", true, max as f64, true, path, false);
                    }
                }
                return Ok(result);
            };
            let mut text = text.clone();
            if trimming {
                let start = text.0.iter().position(|v| !trim(v)).unwrap_or(text.0.len());
                let end = text
                    .0
                    .iter()
                    .rposition(|v| !trim(v))
                    .map_or(start, |n| n + 1);
                text.0 = text.0[start..end].to_vec();
            }
            let len = text.code_point_len();
            if len < min {
                result.bound("string", false, min as f64, true, path, false);
            }
            if len > max {
                result.bound("string", true, max as f64, true, path, false);
            }
            result.value = J::String(text);
        }
        Rule::Integer(min, max, inclusive) => {
            let Some(n) = input.and_then(J::as_f64) else {
                result.invalid("number", input, path, false);
                return Ok(result);
            };
            if n.fract() != 0.0 {
                result.invalid("int", input, path, true);
                return Ok(result);
            }
            if n.abs() > SAFE {
                result.bound(
                    "int",
                    n > 0.0,
                    if n > 0.0 { SAFE } else { -SAFE },
                    true,
                    path,
                    true,
                );
            }
            if n < min || (!inclusive && n == min) {
                result.bound("number", false, min, inclusive, path, false);
            }
            if n > max {
                result.bound("number", true, max, true, path, false);
            }
        }
        Rule::Token => {
            if input.is_some_and(J::is_null) {
                return Ok(result);
            }
            let Some(text) = input.and_then(J::as_js_string) else {
                result.invalid("string", input, path, false);
                return Ok(result);
            };
            if !(32..=128).contains(&text.0.len())
                || !text
                    .0
                    .iter()
                    .all(|v| matches!(v, 65..=90 | 97..=122 | 48..=57 | 95 | 45))
            {
                result.issue(
                    J::object([
                        ("origin", "string".into()),
                        ("code", "invalid_format".into()),
                        ("format", "regex".into()),
                        ("pattern", "/^[A-Za-z0-9_-]{32,128}$/".into()),
                    ]),
                    path,
                    "Invalid string: must match pattern /^[A-Za-z0-9_-]{32,128}$/",
                );
            }
        }
        Rule::Color => {
            let values = ["color", "grayscale", "monochrome"];
            if !input
                .and_then(J::as_js_string)
                .is_some_and(|s| values.iter().any(|v| s == &JsString::from(*v)))
            {
                result.issue(
                    J::object([
                        ("code", "invalid_value".into()),
                        (
                            "values",
                            J::Array(values.into_iter().map(J::from).collect()),
                        ),
                    ]),
                    path,
                    "Invalid option: expected one of \"color\"|\"grayscale\"|\"monochrome\"",
                );
                result.aborted = true;
            }
        }
        Rule::Object(kind) => return object(kind, input, path),
        Rule::Source => {
            // Union branches carry relative issue paths, as Zod's public errors do.
            let mut a = object("object-source", input, &[])?;
            if a.issues.is_empty() {
                return Ok(a);
            }
            let b = object("contract-source", input, &[])?;
            if b.issues.is_empty() {
                return Ok(b);
            }
            if a.aborted != b.aborted {
                if a.aborted {
                    a = b;
                }
                for issue in &mut a.issues {
                    let mut p: Vec<J> = path.iter().cloned().map(J::String).collect();
                    p.extend(issue.required("path")?.as_array().unwrap().iter().cloned());
                    issue.insert("path", J::Array(p));
                }
                return Ok(a);
            }
            result.issue(
                J::object([
                    ("code", "invalid_union".into()),
                    (
                        "errors",
                        J::Array(vec![J::Array(a.issues), J::Array(b.issues)]),
                    ),
                ]),
                path,
                "Invalid input",
            );
            result.aborted = true;
        }
        Rule::Sources | Rule::Metadata => {
            let Some(pairs) = input.and_then(J::as_object) else {
                result.invalid("object", input, path, false);
                return Ok(result);
            };
            result.value = J::Object(vec![]);
            for (name, value) in ordered(pairs) {
                if name == &JsString::from("__proto__") {
                    continue;
                }
                let child = if matches!(rule, Rule::Sources) {
                    let mut p = path.to_vec();
                    p.push(name.clone());
                    parse(Rule::Source, Some(value), &p)?
                } else {
                    Parsed::new(json_object_copy(value))
                };
                result.aborted |= child.aborted;
                result.issues.extend(child.issues);
                result.value.insert(name.clone(), child.value);
            }
            if !result.aborted {
                if matches!(rule, Rule::Sources) {
                    let pairs = result.value.as_object().unwrap();
                    if pairs.len() > 20 || pairs.iter().any(|(key, _)| !valid_source_name(key)) {
                        result.custom(path, "Invalid input");
                    }
                } else {
                    let encoded = json::encode_uri_component(
                        &json::stringify(&result.value, usize::MAX)?.into(),
                    )?;
                    if result.value.get("refreshSeconds").is_some() || encoded.len() > 8000 {
                        result.custom(path, METADATA);
                    }
                }
            }
        }
    }
    Ok(result)
}

// JavaScript relational checks coerce a malformed object's own length. This
// only affects the additional Zod issues after the string type has failed.
fn numeric(value: &J) -> f64 {
    match value {
        J::Null => 0.0,
        J::Bool(v) => {
            if *v {
                1.0
            } else {
                0.0
            }
        }
        J::Number(v) => *v,
        _ => {
            let s = json::js_string(value);
            let start = s.0.iter().position(|v| !trim(v)).unwrap_or(s.0.len());
            let end = s.0.iter().rposition(|v| !trim(v)).map_or(start, |n| n + 1);
            let Ok(s) = String::from_utf16(&s.0[start..end]) else {
                return f64::NAN;
            };
            if s.is_empty() {
                return 0.0;
            }
            for (prefix, radix) in [
                ("0x", 16),
                ("0X", 16),
                ("0b", 2),
                ("0B", 2),
                ("0o", 8),
                ("0O", 8),
            ] {
                if let Some(digits) = s.strip_prefix(prefix) {
                    return if digits.is_empty() {
                        f64::NAN
                    } else {
                        digits
                            .chars()
                            .try_fold(0.0, |n, c| {
                                c.to_digit(radix)
                                    .map(|v| n * f64::from(radix) + f64::from(v))
                            })
                            .unwrap_or(f64::NAN)
                    };
                }
            }
            if s == "Infinity" || s == "+Infinity" {
                return f64::INFINITY;
            }
            if s == "-Infinity" {
                return f64::NEG_INFINITY;
            }
            if s.chars()
                .any(|c| !matches!(c, '0'..='9' | 'e' | 'E' | '+' | '-' | '.'))
            {
                return f64::NAN;
            }
            s.parse().unwrap_or(f64::NAN)
        }
    }
}

fn valid_source_name(key: &JsString) -> bool {
    (1..=64).contains(&key.0.len())
        && matches!(key.0[0], 65..=90 | 97..=122)
        && key
            .0
            .iter()
            .all(|v| matches!(v, 65..=90 | 97..=122 | 48..=57 | 95))
}

// z.object({}).catchall(jsonValue) strips __proto__ recursively. Use an
// explicit stack so ordinary valid metadata does not inherit Rust stack limits.
fn json_object_copy(value: &J) -> J {
    let mut value = value.clone();
    let mut pending = vec![&mut value];
    while let Some(node) = pending.pop() {
        match node {
            J::Object(pairs) => {
                pairs.retain(|(k, _)| k != &JsString::from("__proto__"));
                pairs.sort_by_key(|(k, _)| index(k));
                pending.extend(pairs.iter_mut().map(|(_, v)| v));
            }
            J::Array(values) => pending.extend(values.iter_mut()),
            _ => {}
        }
    }
    value
}

fn index(key: &JsString) -> (u8, u32) {
    let digits = &key.0;
    if digits.is_empty() || digits.len() > 10 || (digits.len() > 1 && digits[0] == 48) {
        return (1, 0);
    }
    let n = digits.iter().try_fold(0u32, |n, c| {
        if (48..=57).contains(c) {
            n.checked_mul(10)?.checked_add(u32::from(*c - 48))
        } else {
            None
        }
    });
    n.filter(|n| *n < u32::MAX).map_or((1, 0), |n| (0, n))
}
fn ordered(pairs: &[(JsString, J)]) -> Vec<&(JsString, J)> {
    let mut pairs: Vec<_> = pairs.iter().collect();
    pairs.sort_by_key(|(k, _)| index(k));
    pairs
}

fn object(kind: &'static str, input: Option<&J>, path: &[JsString]) -> IvyResult<Parsed> {
    let mut result = Parsed::new(J::Object(vec![]));
    let Some(pairs) = input.and_then(J::as_object) else {
        result.invalid("object", input, path, false);
        return Ok(result);
    };
    let text = Rule::Text(1, usize::MAX, false);
    let revision = Rule::Integer(0.0, f64::INFINITY, false);
    // (field, rule, optional, default); all schemas are service-local and fixed.
    let mut fields: Vec<(&str, Rule, bool, Option<J>)> = match kind {
        "definition" => vec![
            ("title", Rule::Text(1, 200, true), false, None),
            ("html", Rule::Text(1, 500_000, false), false, None),
            ("sources", Rule::Sources, false, None),
            (
                "refreshSeconds",
                Rule::Integer(0.0, 86400.0, true),
                false,
                Some(J::Number(5.0)),
            ),
            ("metadata", Rule::Metadata, false, Some(J::Object(vec![]))),
            ("imageToken", Rule::Token, false, Some(J::Null)),
        ],
        "object-source" => vec![("objectId", text, false, None)],
        "contract-source" => vec![
            ("contractKey", text, false, None),
            (
                "limit",
                Rule::Integer(1.0, 100.0, true),
                false,
                Some(J::Number(50.0)),
            ),
        ],
        "list" => vec![
            ("cursor", Rule::Text(0, usize::MAX, false), true, None),
            (
                "limit",
                Rule::Integer(1.0, 100.0, true),
                false,
                Some(J::Number(50.0)),
            ),
        ],
        "save" => vec![
            ("id", text, true, None),
            ("expectedRevision", revision, true, None),
            ("value", Rule::Object("definition"), false, None),
        ],
        "delete" => vec![
            ("id", text, false, None),
            ("expectedRevision", revision, false, None),
        ],
        "read" | "data" => vec![("id", text, false, None)],
        "output" | "render" | "image" => vec![
            (
                "width",
                Rule::Integer(100.0, 4096.0, true),
                false,
                Some(J::Number(800.0)),
            ),
            (
                "height",
                Rule::Integer(100.0, 4096.0, true),
                false,
                Some(J::Number(600.0)),
            ),
            ("colorMode", Rule::Color, false, Some("color".into())),
        ],
        _ => return Err(IvyError::internal()),
    };
    if kind == "render" || kind == "image" {
        fields.push(("id", text, false, None));
    }
    if kind == "image" {
        fields.push(("token", Rule::Text(1, 128, false), false, None));
    }
    for (name, rule, optional, default) in &fields {
        let value = input.and_then(|v| v.get(name));
        if value.is_none() && *optional {
            continue;
        }
        let mut p = path.to_vec();
        p.push((*name).into());
        let mut child = parse(*rule, value.or(default.as_ref()), &p)?;
        if *name == "refreshSeconds"
            && !child.aborted
            && child.value.as_f64().is_some_and(|n| n != 0.0 && n < 5.0)
        {
            child.custom(&p, "Invalid input");
        }
        result.aborted |= child.aborted;
        result.issues.extend(child.issues);
        result.value.insert(*name, child.value);
    }
    let unknown: Vec<_> = ordered(pairs)
        .into_iter()
        .filter(|(key, _)| {
            !fields
                .iter()
                .any(|(name, _, _, _)| key == &JsString::from(*name))
        })
        .map(|(key, _)| key.clone())
        .collect();
    if !unknown.is_empty() {
        let mut message: JsString = format!(
            "Unrecognized key{}: ",
            if unknown.len() > 1 { "s" } else { "" }
        )
        .into();
        for (i, key) in unknown.iter().enumerate() {
            if i > 0 {
                message.0.extend(", ".encode_utf16());
            }
            message.0.push(34);
            message.0.extend(&key.0);
            message.0.push(34);
        }
        result.issue(
            J::object([
                ("code", "unrecognized_keys".into()),
                (
                    "keys",
                    J::Array(unknown.into_iter().map(J::String).collect()),
                ),
            ]),
            path,
            message,
        );
    }
    if !result.aborted {
        if kind == "save" {
            let id = result
                .value
                .get("id")
                .and_then(J::as_js_string)
                .is_some_and(|s| !s.0.is_empty());
            let revision = result
                .value
                .get("expectedRevision")
                .and_then(J::as_f64)
                .is_some_and(|n| n != 0.0);
            if id != revision {
                result.custom(path, "Invalid input");
            }
        }
        if matches!(kind, "output" | "render" | "image")
            && result.value.required("width")?.as_f64().unwrap()
                * result.value.required("height")?.as_f64().unwrap()
                > 8_000_000.0
        {
            result.custom(path, "At most 8 million pixels.");
        }
    }
    Ok(result)
}

/// Returns normalized values; validation failures match the existing handler's
/// invalid_arguments code and Zod JSON issue message, including field order.
pub fn parse_input(name: &str, input: &J) -> IvyResult<J> {
    let name = match name {
        "definition" => "definition",
        "output" => "output",
        "list" => "list",
        "read" => "read",
        "save" => "save",
        "delete" => "delete",
        "data" => "data",
        "render" => "render",
        "image" => "image",
        _ => return Err(IvyError::internal()),
    };
    let parsed = object(name, Some(input), &[])?;
    if parsed.issues.is_empty() {
        return Ok(parsed.value);
    }
    Err(IvyError::new(
        "invalid_arguments",
        pretty(&J::Array(parsed.issues))?,
    ))
}

fn pretty(value: &J) -> IvyResult<String> {
    let compact = json::stringify(value, usize::MAX)?;
    let mut out = String::new();
    let mut depth = 0usize;
    let mut quoted = false;
    let mut escaped = false;
    let mut chars = compact.chars().peekable();
    while let Some(c) = chars.next() {
        if quoted {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                quoted = false;
            }
            continue;
        }
        match c {
            '"' => {
                quoted = true;
                out.push(c);
            }
            '[' | '{' => {
                out.push(c);
                if chars.peek().is_some_and(|n| *n == ']' || *n == '}') {
                    out.push(chars.next().unwrap());
                } else {
                    depth += 1;
                    out.push('\n');
                    out.push_str(&"  ".repeat(depth));
                }
            }
            ']' | '}' => {
                depth -= 1;
                out.push('\n');
                out.push_str(&"  ".repeat(depth));
                out.push(c);
            }
            ',' => {
                out.push_str(",\n");
                out.push_str(&"  ".repeat(depth));
            }
            ':' => out.push_str(": "),
            _ => out.push(c),
        }
    }
    Ok(out)
}
