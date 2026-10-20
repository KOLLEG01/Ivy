//! Ordered question projection over JSON-backed TaskBoard comment records.
use crate::{IvyError, IvyJson as Json, IvyResult, JsString, json};

/// Return complete open question records in their original comment order.
/// Only native questions are filtered by a nonempty run ID. Access failures use
/// the existing IvyError::from(non-Ivy error) boundary rather than an empty view.
pub fn open_task_questions(task: &Json, run_id: Option<&JsString>) -> IvyResult<Vec<Json>> {
    let comments = field(Some(task), "comments")?
        .and_then(Json::as_array)
        .ok_or_else(IvyError::internal)?;
    let mut open = Vec::new();
    for (index, comment) in comments.iter().enumerate() {
        if !text(field(Some(comment), "purpose")?, "question")
            || truthy(field(Some(comment), "sourceTaskId")?)
        {
            continue;
        }
        let later = &comments[index + 1..];
        let closed = if truthy(field(Some(comment), "nativeInput")?) {
            if let Some(run_id) = run_id.filter(|id| !id.0.is_empty()) {
                if optional_field(field(Some(comment), "run")?, "objectId")
                    .and_then(Json::as_js_string)
                    != Some(run_id)
                {
                    continue;
                }
            }
            native_closed(comment, later)?
        } else {
            ordinary_closed(comment, later)?
        };
        if !closed {
            open.push(comment.clone());
        }
    }
    Ok(open)
}

fn native_closed(question: &Json, later: &[Json]) -> IvyResult<bool> {
    for reply in later {
        if equal(
            field(Some(reply), "replyTo")?,
            field(Some(question), "commentId")?,
        ) && truthy(field(Some(reply), "nativeInput")?)
        {
            let state = field(field(Some(reply), "nativeInput")?, "state")?;
            if text(state, "answered") || text(state, "expired") {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

fn ordinary_closed(question: &Json, later: &[Json]) -> IvyResult<bool> {
    for reply in later {
        if let Some(resolutions) =
            field(Some(reply), "resolvesQuestions")?.filter(|value| !value.is_null())
        {
            if includes(resolutions, field(Some(question), "commentId")?)? {
                return Ok(true);
            }
        }
        if equal(
            field(Some(reply), "replyTo")?,
            field(Some(question), "commentId")?,
        ) {
            let recipient = field(Some(question), "recipient")?;
            let answered = if text(optional_field(recipient, "kind"), "task") {
                equal(
                    field(Some(reply), "sourceTaskId")?,
                    field(recipient, "taskId")?,
                )
            } else {
                text(field(Some(reply), "authorKind")?, "user")
            };
            if answered {
                return Ok(true);
            }
        }
        // Keep the original short circuit: no later reply means requests are
        // never accessed, and a resolution/addressed answer precedes coverage.
        if requests_covered(question, later)? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn requests_covered(question: &Json, later: &[Json]) -> IvyResult<bool> {
    let requests = field(Some(question), "requests")?;
    let nonempty = match requests {
        Some(Json::Array(items)) => !items.is_empty(),
        Some(Json::String(value)) => !value.0.is_empty(),
        _ => match field(requests, "length")? {
            None | Some(Json::Null) => false,
            Some(Json::Number(length)) => *length > 0.0,
            Some(Json::Bool(length)) => *length,
            Some(length) => json::number_from_string(&json::try_js_string(length)?) > 0.0,
        },
    };
    if !nonempty {
        return Ok(false);
    }
    let requests = requests
        .and_then(Json::as_array)
        .ok_or_else(IvyError::internal)?;
    for request in requests {
        let mut covered = false;
        'answers: for answer in later {
            let responses = field(Some(answer), "responses")?
                .and_then(Json::as_array)
                .ok_or_else(IvyError::internal)?;
            for response in responses {
                if equal(
                    field(Some(response), "requestId")?,
                    field(Some(request), "requestId")?,
                ) {
                    covered = true;
                    break 'answers;
                }
            }
        }
        if !covered {
            return Ok(false);
        }
    }
    Ok(true)
}

fn field<'a>(value: Option<&'a Json>, key: &str) -> IvyResult<Option<&'a Json>> {
    match value {
        None | Some(Json::Null) => Err(IvyError::internal()),
        Some(value) => Ok(value.get(key)),
    }
}

fn optional_field<'a>(value: Option<&'a Json>, key: &str) -> Option<&'a Json> {
    value.and_then(|value| value.get(key))
}

fn text(value: Option<&Json>, expected: &str) -> bool {
    value.and_then(Json::as_js_string) == Some(&JsString::from(expected))
}

fn truthy(value: Option<&Json>) -> bool {
    match value {
        None | Some(Json::Null | Json::Bool(false)) => false,
        Some(Json::Number(value)) => *value != 0.0 && !value.is_nan(),
        Some(Json::String(value)) => !value.0.is_empty(),
        _ => true,
    }
}

// JS strict identity, not structural JSON equality. Missing properties model
// undefined; object IDs cannot become equal merely by having identical fields.
fn equal(left: Option<&Json>, right: Option<&Json>) -> bool {
    match (left, right) {
        (None, None) | (Some(Json::Null), Some(Json::Null)) => true,
        (Some(Json::Bool(a)), Some(Json::Bool(b))) => a == b,
        (Some(Json::Number(a)), Some(Json::Number(b))) => a == b,
        (Some(Json::String(a)), Some(Json::String(b))) => a == b,
        (Some(a @ (Json::Array(_) | Json::Object(_))), Some(b)) => std::ptr::eq(a, b),
        _ => false,
    }
}

fn includes(values: &Json, needle: Option<&Json>) -> IvyResult<bool> {
    match values {
        Json::Array(values) => Ok(values.iter().any(|value| {
            equal(Some(value), needle)
                || matches!((value, needle), (Json::Number(a), Some(Json::Number(b))) if a.is_nan() && b.is_nan())
        })),
        Json::String(value) => {
            let needle = match needle {
                Some(needle) => json::try_js_string(needle)?,
                None => "undefined".into(),
            };
            Ok(needle.0.is_empty()
                || value.0.windows(needle.0.len()).any(|part| part == needle.0))
        }
        _ => Err(IvyError::internal()),
    }
}
