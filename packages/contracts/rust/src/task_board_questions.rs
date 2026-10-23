//! Open questions from ordered TaskBoard comment records.
use crate::{IvyError, IvyJson as Json, IvyResult, JsString};

/// Return complete question records in comment order. A run filter applies only
/// to native inputs; ordinary questions remain open until addressed or resolved.
pub fn open_task_questions(task: &Json, run_id: Option<&JsString>) -> IvyResult<Vec<Json>> {
    let comments = task
        .get("comments")
        .and_then(Json::as_array)
        .ok_or_else(IvyError::internal)?;
    let run_id = run_id.filter(|id| !id.0.is_empty());
    let mut open = Vec::new();
    for (index, comment) in comments.iter().enumerate() {
        if comment.as_object().is_none() {
            return Err(IvyError::internal());
        }
        if !text(comment.get("purpose"), "question")
            || optional_id(comment.get("sourceTaskId"))?.is_some()
        {
            continue;
        }
        let id = required_id(comment.get("commentId"))?;
        let native = match comment.get("nativeInput") {
            None | Some(Json::Null) => false,
            Some(value) if value.as_object().is_some() => true,
            _ => return Err(IvyError::internal()),
        };
        if native
            && run_id.is_some_and(|run_id| {
                comment
                    .get("run")
                    .and_then(|run| run.get("objectId"))
                    .and_then(Json::as_js_string)
                    != Some(run_id)
            })
        {
            continue;
        }
        let later = &comments[index + 1..];
        let closed = if native {
            later.iter().any(|reply| {
                reply.get("replyTo").and_then(Json::as_js_string) == Some(id)
                    && reply
                        .get("nativeInput")
                        .and_then(|input| input.get("state"))
                        .is_some_and(|state| {
                            text(Some(state), "answered") || text(Some(state), "expired")
                        })
            })
        } else {
            ordinary_closed(comment, id, later)?
        };
        if !closed {
            open.push(comment.clone());
        }
    }
    Ok(open)
}

fn ordinary_closed(question: &Json, id: &JsString, later: &[Json]) -> IvyResult<bool> {
    for reply in later {
        for resolution in list(reply.get("resolvesQuestions"))? {
            if required_id(Some(resolution))? == id {
                return Ok(true);
            }
        }
        if reply.get("replyTo").and_then(Json::as_js_string) == Some(id) {
            let recipient = question.get("recipient");
            let answered = if text(recipient.and_then(|value| value.get("kind")), "task") {
                let task_id = required_id(recipient.and_then(|value| value.get("taskId")))?;
                reply.get("sourceTaskId").and_then(Json::as_js_string) == Some(task_id)
            } else {
                text(reply.get("authorKind"), "user")
            };
            if answered {
                return Ok(true);
            }
        }
    }
    let requests = list(question.get("requests"))?;
    if requests.is_empty() || later.is_empty() {
        return Ok(false);
    }
    for request in requests {
        let request_id = required_id(request.get("requestId"))?;
        let mut covered = false;
        for reply in later {
            for response in list(reply.get("responses"))? {
                if required_id(response.get("requestId"))? == request_id {
                    covered = true;
                    break;
                }
            }
            if covered {
                break;
            }
        }
        if !covered {
            return Ok(false);
        }
    }
    Ok(true)
}

fn text(value: Option<&Json>, expected: &str) -> bool {
    value.and_then(Json::as_js_string) == Some(&JsString::from(expected))
}
fn optional_id(value: Option<&Json>) -> IvyResult<Option<&JsString>> {
    match value {
        None | Some(Json::Null) => Ok(None),
        Some(Json::String(id)) => Ok((!id.0.is_empty()).then_some(id)),
        _ => Err(IvyError::internal()),
    }
}
fn required_id(value: Option<&Json>) -> IvyResult<&JsString> {
    optional_id(value)?.ok_or_else(IvyError::internal)
}
fn list(value: Option<&Json>) -> IvyResult<&[Json]> {
    match value {
        None | Some(Json::Null) => Ok(&[]),
        Some(Json::Array(values)) => Ok(values),
        _ => Err(IvyError::internal()),
    }
}
