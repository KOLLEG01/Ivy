//! The original ordered backfill before Shared record upgrades. Missing RPC
//! parameters remain distinct from null at the supplied read boundary.
use ivy_contracts::{
    IvyError, IvyJson as Json, JsString,
    error::{JavaScriptException, JavaScriptExceptionKind},
    json::js_string,
};
use std::future::Future;

#[derive(Clone)]
enum Part {
    Field(&'static str),
    Index(usize),
}
type Path = Vec<Part>;
fn child(path: &[Part], part: Part) -> Path {
    let mut next = path.to_vec();
    next.push(part);
    next
}
fn at<'a>(mut value: &'a Json, path: &[Part]) -> &'a Json {
    for part in path {
        value = match part {
            Part::Field(name) => value.get(name).unwrap(),
            Part::Index(index) => &value.as_array().unwrap()[*index],
        };
    }
    value
}
fn at_mut<'a>(mut value: &'a mut Json, path: &[Part]) -> &'a mut Json {
    for part in path {
        value = match (part, value) {
            (Part::Field(name), Json::Object(fields)) => {
                &mut fields
                    .iter_mut()
                    .find(|(key, _)| key == &JsString::from(*name))
                    .unwrap()
                    .1
            }
            (Part::Index(index), Json::Array(items)) => &mut items[*index],
            _ => unreachable!("Selected backfill path changed"),
        };
    }
    value
}
fn type_error<E: From<JavaScriptException>>(message: impl Into<JsString>) -> E {
    JavaScriptException {
        kind: JavaScriptExceptionKind::TypeError,
        message: message.into(),
    }
    .into()
}
fn field<'a, E: From<JavaScriptException>>(
    value: Option<&'a Json>,
    key: &str,
) -> Result<Option<&'a Json>, E> {
    match value {
        None | Some(Json::Null) => Err(type_error(format!(
            "Cannot read properties of {} (reading '{key}')",
            if value.is_none() { "undefined" } else { "null" }
        ))),
        Some(value) => Ok(value.get(key)),
    }
}
fn optional<'a, E: From<JavaScriptException>>(
    value: Option<&'a Json>,
    key: &str,
) -> Result<Option<&'a Json>, E> {
    match value {
        None | Some(Json::Null) => Ok(None),
        _ => field(value, key),
    }
}
fn spread_findings<E: From<JavaScriptException>>(
    value: Option<&Json>,
    path: Path,
    paths: &mut Vec<Path>,
    message: &str,
) -> Result<(), E> {
    match value {
        None | Some(Json::Null) => {}
        Some(Json::Array(items)) => {
            paths.extend((0..items.len()).map(|i| child(&path, Part::Index(i))))
        }
        // String spread yields primitive findings without an evidence field.
        // The original string itself remains in the cloned record unchanged.
        Some(Json::String(_)) => {}
        _ => return Err(type_error(message)),
    }
    Ok(())
}
fn same_value(a: Option<&Json>, b: Option<&Json>) -> bool {
    match (a, b) {
        (None, None) | (Some(Json::Null), Some(Json::Null)) => true,
        (Some(Json::String(a)), Some(Json::String(b))) => a == b,
        (Some(Json::Bool(a)), Some(Json::Bool(b))) => a == b,
        (Some(Json::Number(a)), Some(Json::Number(b))) => a == b,
        (Some(a @ (Json::Object(_) | Json::Array(_))), Some(b)) => std::ptr::eq(a, b),
        _ => false,
    }
}

pub async fn fill_verification_sizes_with<E, F, Fut>(
    key: &str,
    value: &Json,
    mut read: F,
) -> Result<Json, E>
where
    E: From<IvyError> + From<JavaScriptException>,
    F: FnMut(Option<Json>) -> Fut,
    Fut: Future<Output = Result<Json, E>>,
{
    let mut stored = value.clone();
    if key != "task-board/task" {
        return Ok(stored);
    }
    let mut findings = Vec::new();
    spread_findings::<E>(
        field::<E>(Some(&stored), "latestChecks")?,
        vec![Part::Field("latestChecks")],
        &mut findings,
        "(stored.latestChecks ?? []) is not iterable",
    )?;
    let content = field::<E>(Some(&stored), "content")?;
    spread_findings::<E>(
        optional::<E>(content, "checks")?,
        vec![Part::Field("content"), Part::Field("checks")],
        &mut findings,
        "((intermediate value) ?? []) is not iterable",
    )?;
    match field::<E>(Some(&stored), "comments")? {
        None | Some(Json::Null) => {}
        Some(Json::Array(comments)) => {
            for (index, comment) in comments.iter().enumerate() {
                let handoff = field::<E>(Some(comment), "handoff")?;
                let checks = optional::<E>(handoff, "checks")?;
                let path = vec![
                    Part::Field("comments"),
                    Part::Index(index),
                    Part::Field("handoff"),
                    Part::Field("checks"),
                ];
                match checks {
                    None | Some(Json::Null) => {}
                    Some(Json::Array(items)) => {
                        findings.extend((0..items.len()).map(|i| child(&path, Part::Index(i))))
                    }
                    Some(_) => findings.push(path),
                }
            }
        }
        _ => {
            return Err(type_error(
                "(stored.comments ?? []).flatMap is not a function or its return value is not iterable",
            ));
        }
    }
    // Both flatMaps complete before the first evidence read. Paths retain the
    // original aliases into the one cloned record without cloning findings.
    let mut artifacts = Vec::new();
    for path in findings {
        let evidence = field::<E>(Some(at(&stored, &path)), "evidence")?;
        let path = child(&path, Part::Field("evidence"));
        match evidence {
            None | Some(Json::Null) => {}
            Some(Json::Array(items)) => {
                artifacts.extend((0..items.len()).map(|i| child(&path, Part::Index(i))))
            }
            Some(_) => artifacts.push(path),
        }
    }
    for path in artifacts {
        let artifact = at(&stored, &path);
        if field::<E>(Some(artifact), "byteLength")?.is_some() {
            continue;
        }
        let saved = read(field::<E>(Some(artifact), "object")?.cloned()).await?;
        let revision = field::<E>(Some(&saved), "revision")?;
        if !same_value(
            field::<E>(revision, "contentHash")?,
            field::<E>(Some(artifact), "contentHash")?,
        ) || !same_value(
            field::<E>(revision, "mediaType")?,
            field::<E>(Some(artifact), "mediaType")?,
        ) {
            return Err(IvyError::new(
                "task_board_evidence_mismatch",
                "Retained verification evidence differs from its immutable pin.",
            )
            .into());
        }
        let size = field::<E>(revision, "byteLength")?;
        match artifact {
            Json::Object(_) => {
                if let Some(size) = size {
                    at_mut(&mut stored, &path).insert("byteLength", size.clone());
                }
            }
            // JavaScript permits this property, but JSON array serialization
            // excludes it and the subsequent Shared upgrade retains that array.
            Json::Array(_) => {}
            primitive => {
                let kind = match primitive {
                    Json::String(_) => "string",
                    Json::Bool(_) => "boolean",
                    _ => "number",
                };
                let mut message =
                    JsString::from(format!("Cannot create property 'byteLength' on {kind} '"));
                message.0.extend(js_string(primitive).0);
                message.0.push(u16::from(b'\''));
                return Err(type_error(message));
            }
        }
    }
    Ok(stored)
}
