//! The parameter boundary for Hive's registered MCP methods. Outer JSON-RPC
//! admission and era/header routing precede this post-lift schema projection.
// Adapted from the TypeScript MCP SDK 2.0.0; translated to Rust with IvyJson.
// See NOTICE and licenses/mcp-sdk.txt in this crate.
use crate::{mcp_inbound, mcp_protocol};
use ivy_contracts::{IvyJson, IvyResult, JsString, json::stringify};
use std::sync::OnceLock;

#[derive(Debug, Clone, PartialEq)]
pub struct Issue {
    pub path: Vec<IvyJson>,
    pub message: JsString,
    details: IvyJson,
}
impl Issue {
    pub fn to_json(&self) -> IvyJson {
        self.details.clone()
    }
    pub(crate) fn key(&self) -> JsString {
        let mut result = Vec::new();
        for (index, value) in self.path.iter().enumerate() {
            if index > 0 {
                result.push(46);
            }
            if let Some(value) = value.as_js_string() {
                result.extend(&value.0);
            } else if let Some(value) = value.as_f64() {
                result.extend(value.to_string().encode_utf16());
            }
        }
        JsString(result)
    }
}
#[derive(Clone)]
enum Schema {
    Any,
    Json,
    String,
    Number,
    Boolean,
    Null,
    Enum(&'static [&'static str]),
    Array(Box<Schema>),
    Record(Box<Schema>),
    Object(Vec<(&'static str, Schema, bool)>, bool),
    Intersection(Box<Schema>, Box<Schema>),
    Elicitation(Box<Schema>),
}
fn object(fields: Vec<(&'static str, Schema, bool)>, loose: bool) -> Schema {
    Schema::Object(fields, loose)
}
fn record(value: Schema) -> Schema {
    Schema::Record(Box::new(value))
}
fn array(value: Schema) -> Schema {
    Schema::Array(Box::new(value))
}
fn intersection(a: Schema, b: Schema) -> Schema {
    Schema::Intersection(Box::new(a), Box::new(b))
}
fn record_json() -> Schema {
    record(Schema::Json)
}
fn received(value: Option<&IvyJson>) -> &'static str {
    match value {
        None => "undefined",
        Some(IvyJson::Null) => "null",
        Some(IvyJson::Bool(_)) => "boolean",
        Some(IvyJson::Number(_)) => "number",
        Some(IvyJson::String(_)) => "string",
        Some(IvyJson::Array(_)) => "array",
        Some(IvyJson::Object(_)) => "object",
    }
}
fn typed_issue(
    expected: &'static str,
    value: Option<&IvyJson>,
    path: &[IvyJson],
    issues: &mut Vec<Issue>,
) {
    let message: JsString = format!(
        "Invalid input: expected {expected}, received {}",
        received(value)
    )
    .into();
    let mut details = IvyJson::object([
        ("expected", expected.into()),
        ("code", "invalid_type".into()),
    ]);
    if expected == "number"
        && value
            .and_then(IvyJson::as_f64)
            .is_some_and(|n| !n.is_finite())
    {
        details.insert("received", received(value).into());
    }
    details.insert("path", IvyJson::Array(path.to_vec()));
    details.insert("message", IvyJson::String(message.clone()));
    issues.push(Issue {
        path: path.to_vec(),
        message,
        details,
    });
}
fn child(path: &[IvyJson], key: IvyJson) -> Vec<IvyJson> {
    let mut path = path.to_vec();
    path.push(key);
    path
}
fn merge(a: &mut IvyJson, b: IvyJson) {
    for (key, value) in b.as_object().unwrap_or(&[]) {
        match (a.get_key(key), value) {
            (Some(IvyJson::Object(_)), IvyJson::Object(_)) => {
                let mut merged = a.get_key(key).unwrap().clone();
                merge(&mut merged, value.clone());
                a.insert(key.clone(), merged);
            }
            _ => {
                a.insert(key.clone(), value.clone());
            }
        }
    }
}
fn walk(
    schema: &Schema,
    value: Option<&IvyJson>,
    path: &[IvyJson],
    issues: &mut Vec<Issue>,
) -> IvyJson {
    let fail = |expected, issues: &mut Vec<Issue>| {
        typed_issue(expected, value, path, issues);
        IvyJson::Null
    };
    match schema {
        Schema::Any => value.cloned().unwrap_or(IvyJson::Null),
        Schema::String if !value.is_some_and(|v| v.as_js_string().is_some()) => {
            fail("string", issues)
        }
        Schema::Boolean if !value.is_some_and(|v| v.as_bool().is_some()) => fail("boolean", issues),
        Schema::Number if !value.and_then(IvyJson::as_f64).is_some_and(f64::is_finite) => {
            fail("number", issues)
        }
        Schema::Null if value != Some(&IvyJson::Null) => fail("null", issues),
        Schema::String | Schema::Boolean | Schema::Number | Schema::Null => value.unwrap().clone(),
        Schema::Enum(options) => {
            if options
                .iter()
                .any(|option| value.and_then(IvyJson::as_js_string) == Some(&(*option).into()))
            {
                return value.unwrap().clone();
            }
            let message: JsString = format!(
                "Invalid option: expected one of {}",
                options
                    .iter()
                    .map(|v| format!("\"{v}\""))
                    .collect::<Vec<_>>()
                    .join("|")
            )
            .into();
            issues.push(Issue {
                path: path.to_vec(),
                message: message.clone(),
                details: IvyJson::object([
                    ("code", "invalid_value".into()),
                    (
                        "values",
                        IvyJson::Array(options.iter().map(|v| (*v).into()).collect()),
                    ),
                    ("path", IvyJson::Array(path.to_vec())),
                    ("message", IvyJson::String(message)),
                ]),
            });
            IvyJson::Null
        }
        Schema::Array(item) => {
            let Some(values) = value.and_then(IvyJson::as_array) else {
                return fail("array", issues);
            };
            IvyJson::Array(
                values
                    .iter()
                    .enumerate()
                    .map(|(index, value)| {
                        walk(
                            item,
                            Some(value),
                            &child(path, IvyJson::Number(index as f64)),
                            issues,
                        )
                    })
                    .collect(),
            )
        }
        Schema::Record(item) => {
            let Some(IvyJson::Object(values)) = value else {
                return fail("record", issues);
            };
            IvyJson::object(values.iter().map(|(key, value)| {
                (
                    key.clone(),
                    walk(
                        item,
                        Some(value),
                        &child(path, IvyJson::String(key.clone())),
                        issues,
                    ),
                )
            }))
        }
        Schema::Object(fields, loose) => {
            let Some(value @ IvyJson::Object(_)) = value else {
                return fail("object", issues);
            };
            let mut result = IvyJson::object([] as [(&str, IvyJson); 0]);
            for (key, schema, required) in fields {
                let raw = value.get(key);
                if raw.is_none() && !required {
                    continue;
                }
                result.insert(*key, walk(schema, raw, &child(path, (*key).into()), issues));
            }
            if *loose {
                for (key, value) in value.as_object().unwrap() {
                    if !fields
                        .iter()
                        .any(|(name, _, _)| key == &JsString::from(*name))
                    {
                        result.insert(key.clone(), value.clone());
                    }
                }
            }
            result
        }
        Schema::Intersection(left, right) => {
            let mut left = walk(left, value, path, issues);
            let right = walk(right, value, path, issues);
            merge(&mut left, right);
            left
        }
        Schema::Elicitation(inner) => {
            let empty = IvyJson::object([("form", IvyJson::object([] as [(&str, IvyJson); 0]))]);
            walk(
                inner,
                if value.is_some_and(|v| matches!(v, IvyJson::Object(o) if o.is_empty())) {
                    Some(&empty)
                } else {
                    value
                },
                path,
                issues,
            )
        }
        Schema::Json => {
            // Validate nested JSON without interpreting object property names.
            let valid = match value {
                Some(IvyJson::String(_) | IvyJson::Bool(_) | IvyJson::Null) => true,
                Some(IvyJson::Number(n)) => n.is_finite(),
                Some(IvyJson::Object(_) | IvyJson::Array(_)) => {
                    let before = issues.len();
                    let schema = if matches!(value, Some(IvyJson::Object(_))) {
                        record(Schema::Json)
                    } else {
                        array(Schema::Json)
                    };
                    let projected = walk(&schema, value, path, issues);
                    if issues.len() == before {
                        return projected;
                    }
                    issues.truncate(before);
                    false
                }
                None => false,
            };
            if valid {
                return value.unwrap().clone();
            }
            let mut errors = Vec::new();
            for branch in [
                Schema::String,
                Schema::Number,
                Schema::Boolean,
                Schema::Null,
                record(Schema::Json),
                array(Schema::Json),
            ] {
                let mut local = Vec::new();
                walk(&branch, value, &[], &mut local);
                errors.push(IvyJson::Array(local.iter().map(Issue::to_json).collect()));
            }
            let message = JsString::from("Invalid input");
            issues.push(Issue {
                path: path.to_vec(),
                message: message.clone(),
                details: IvyJson::object([
                    ("code", "invalid_union".into()),
                    ("errors", IvyJson::Array(errors)),
                    ("path", IvyJson::Array(path.to_vec())),
                    ("message", IvyJson::String(message)),
                ]),
            });
            IvyJson::Null
        }
    }
}
fn parse(schema: &Schema, value: &IvyJson) -> Result<IvyJson, Vec<Issue>> {
    let mut issues = Vec::new();
    let value = walk(schema, Some(value), &[], &mut issues);
    if issues.is_empty() {
        Ok(value)
    } else {
        Err(issues)
    }
}
fn implementation() -> Schema {
    object(
        vec![
            ("name", Schema::String, true),
            ("title", Schema::String, false),
            (
                "icons",
                array(object(
                    vec![
                        ("src", Schema::String, true),
                        ("mimeType", Schema::String, false),
                        ("sizes", array(Schema::String), false),
                        ("theme", Schema::Enum(&["light", "dark"]), false),
                    ],
                    false,
                )),
                false,
            ),
            ("version", Schema::String, true),
            ("websiteUrl", Schema::String, false),
            ("description", Schema::String, false),
        ],
        false,
    )
}
fn client_capabilities(legacy: bool) -> Schema {
    let mut fields = vec![
        ("experimental", record(record_json()), false),
        (
            "sampling",
            object(
                vec![
                    ("context", record_json(), false),
                    ("tools", record_json(), false),
                ],
                false,
            ),
            false,
        ),
        (
            "elicitation",
            Schema::Elicitation(Box::new(intersection(
                object(
                    vec![
                        (
                            "form",
                            intersection(
                                object(vec![("applyDefaults", Schema::Boolean, false)], false),
                                record_json(),
                            ),
                            false,
                        ),
                        ("url", record_json(), false),
                    ],
                    false,
                ),
                record_json(),
            ))),
            false,
        ),
        (
            "roots",
            object(vec![("listChanged", Schema::Boolean, false)], false),
            false,
        ),
    ];
    if legacy {
        fields.push((
            "tasks",
            object(
                vec![
                    ("list", record_json(), false),
                    ("cancel", record_json(), false),
                    (
                        "requests",
                        object(
                            vec![
                                (
                                    "sampling",
                                    object(vec![("createMessage", record_json(), false)], true),
                                    false,
                                ),
                                (
                                    "elicitation",
                                    object(vec![("create", record_json(), false)], true),
                                    false,
                                ),
                            ],
                            true,
                        ),
                        false,
                    ),
                ],
                true,
            ),
            false,
        ));
    }
    fields.push(("extensions", record(record_json()), false));
    object(fields, false)
}
fn metadata(modern: bool) -> Schema {
    let mut fields = vec![("progressToken", Schema::Any, false)];
    if !modern {
        fields.push((
            "io.modelcontextprotocol/related-task",
            object(vec![("taskId", Schema::String, true)], false),
            false,
        ));
    }
    object(fields, true)
}
struct Schemas {
    envelope: Schema,
    initialize: Schema,
    ping: Schema,
    legacy_list: Schema,
    legacy_call: Schema,
    modern_list: Schema,
    modern_call: Schema,
    discover: Schema,
}
fn schemas() -> &'static Schemas {
    static SCHEMAS: OnceLock<Schemas> = OnceLock::new();
    SCHEMAS.get_or_init(|| {
        let params = |params, required| {
            object(
                vec![
                    ("method", Schema::String, true),
                    ("params", params, required),
                ],
                false,
            )
        };
        let list = |modern| {
            object(
                vec![
                    ("_meta", metadata(modern), false),
                    ("cursor", Schema::String, false),
                ],
                false,
            )
        };
        let call = |modern| {
            let mut fields = vec![("_meta", metadata(modern), false)];
            if !modern {
                fields.push((
                    "task",
                    object(vec![("ttl", Schema::Number, false)], false),
                    false,
                ));
            }
            fields.extend([
                ("name", Schema::String, true),
                ("arguments", record(Schema::Any), false),
            ]);
            object(fields, false)
        };
        Schemas {
            envelope: object(
                vec![
                    (mcp_inbound::VERSION_META, Schema::String, true),
                    (mcp_inbound::INFO_META, implementation(), false),
                    (
                        mcp_inbound::CAPABILITIES_META,
                        client_capabilities(false),
                        true,
                    ),
                    (
                        mcp_inbound::LOG_META,
                        Schema::Enum(&[
                            "debug",
                            "info",
                            "notice",
                            "warning",
                            "error",
                            "critical",
                            "alert",
                            "emergency",
                        ]),
                        false,
                    ),
                ],
                true,
            ),
            initialize: params(
                object(
                    vec![
                        ("_meta", metadata(false), false),
                        ("protocolVersion", Schema::String, true),
                        ("capabilities", client_capabilities(true), true),
                        ("clientInfo", implementation(), true),
                    ],
                    false,
                ),
                true,
            ),
            ping: params(
                object(vec![("_meta", metadata(false), false)], false),
                false,
            ),
            legacy_list: params(list(false), false),
            legacy_call: params(call(false), true),
            modern_list: params(list(true), false),
            modern_call: params(call(true), false),
            discover: params(object(vec![("_meta", metadata(true), false)], false), false),
        }
    })
}
pub(crate) fn envelope_issues(value: &IvyJson) -> Vec<Issue> {
    parse(&schemas().envelope, value).err().unwrap_or_default()
}
pub(crate) fn legacy_initialize(body: &IvyJson) -> bool {
    body.get("method") == Some(&"initialize".into())
        && mcp_protocol::message_kind(body) == Some(mcp_protocol::MessageKind::Request)
        && parse(&schemas().initialize, body).is_ok()
}
/// None means this is not one of Hive's registered methods in the chosen era.
/// The body must already pass strict JSON-RPC admission. Wire-only envelope and
/// retry material is removed before returning the handler-visible projection.
pub fn request(body: &IvyJson, modern: bool) -> Option<Result<IvyJson, Vec<Issue>>> {
    if mcp_protocol::message_kind(body) != Some(mcp_protocol::MessageKind::Request) {
        return None;
    }
    let schema = match body.get("method")?.as_js_string()? {
        method if method == &JsString::from("initialize") && !modern => &schemas().initialize,
        method if method == &JsString::from("ping") && !modern => &schemas().ping,
        method if method == &JsString::from("server/discover") && modern => &schemas().discover,
        method if method == &JsString::from("tools/list") => {
            if modern {
                &schemas().modern_list
            } else {
                &schemas().legacy_list
            }
        }
        method if method == &JsString::from("tools/call") => {
            if modern {
                &schemas().modern_call
            } else {
                &schemas().legacy_call
            }
        }
        _ => return None,
    };
    let mut lifted = body.clone();
    if let Some(params) = body.get("params") {
        let mut params = params.clone();
        if let Some(meta) = params.get("_meta") {
            let mut meta = meta.clone();
            let mut found = false;
            for key in [
                mcp_inbound::VERSION_META,
                mcp_inbound::INFO_META,
                mcp_inbound::CAPABILITIES_META,
                mcp_inbound::LOG_META,
            ] {
                found |= meta.remove(key).is_some();
            }
            if found {
                if meta.as_object().is_some_and(|v| v.is_empty()) {
                    params.remove("_meta");
                } else {
                    params.insert("_meta", meta);
                }
            }
        }
        params.remove("inputResponses");
        params.remove("requestState");
        lifted.insert("params", params);
    }
    Some(parse(schema, &lifted))
}
/// The SDK serializes its issue list with JSON.stringify(..., null, 2).
pub fn diagnostic(issues: &[Issue]) -> IvyResult<JsString> {
    fn pretty(value: &IvyJson, level: usize) -> IvyResult<String> {
        let (open, close, items) = match value {
            IvyJson::Array(values) if !values.is_empty() => (
                "[",
                "]",
                values
                    .iter()
                    .map(|v| pretty(v, level + 1))
                    .collect::<IvyResult<Vec<_>>>()?,
            ),
            IvyJson::Object(values) if !values.is_empty() => (
                "{",
                "}",
                values
                    .iter()
                    .map(|(key, value)| {
                        Ok(format!(
                            "{}: {}",
                            stringify(&IvyJson::String(key.clone()), usize::MAX)?,
                            pretty(value, level + 1)?
                        ))
                    })
                    .collect::<IvyResult<Vec<_>>>()?,
            ),
            _ => return stringify(value, usize::MAX),
        };
        let indent = "  ".repeat(level + 1);
        Ok(format!(
            "{open}\n{indent}{}\n{}{close}",
            items.join(&format!(",\n{indent}")),
            "  ".repeat(level)
        ))
    }
    Ok(pretty(
        &IvyJson::Array(issues.iter().map(Issue::to_json).collect()),
        0,
    )?
    .into())
}
