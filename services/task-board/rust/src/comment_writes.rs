//! Retained conversation publication over native JSON mutation requests.
#![cfg_attr(not(test), allow(dead_code))]
use crate::journal_store::{
    mutation_ports::{LocalMutationSource, WriteFailure},
    persistent_reads::json_values::{self as v, Value},
};
use ivy_contracts::{IvyError, JsString, hash::hash_json};
use std::future::Future;
#[path = "tree_comment_writes.rs"]
pub(crate) mod tree;
pub(super) type Result<T, E> = std::result::Result<T, WriteFailure<E>>;
const CONTRACT: &str = "task-board/comments";
pub(super) fn text(value: impl Into<JsString>) -> Value {
    Value::String(value.into())
}
pub(super) fn obj(fields: impl IntoIterator<Item = (&'static str, Value)>) -> Value {
    Value::object(fields)
}
pub(super) fn f(value: &Value, key: &str) -> std::result::Result<Value, IvyError> {
    v::field(value, key)
}
pub(super) fn at(value: &Value, index: usize) -> std::result::Result<Value, IvyError> {
    v::at(value, index)
}
pub(super) fn array(values: impl IntoIterator<Item = Value>) -> Value {
    v::array(values)
}
pub(super) fn spread(
    value: &Value,
    fields: impl IntoIterator<Item = (&'static str, Value)>,
) -> Value {
    let mut value = value.clone();
    for (k, v) in fields {
        value.insert(k, v);
    }
    value
}
fn immutable(value: &Value) -> Value {
    let mut value = value.clone();
    for key in ["nativeInput", "handoff", "delivery", "recipient"] {
        value.remove(key);
    }
    value
}
pub(super) fn hash(value: &Value) -> std::result::Result<String, IvyError> {
    hash_json(value)
}
pub(super) fn number(value: &Value) -> std::result::Result<f64, IvyError> {
    v::number(value)
}
pub(super) fn add(left: Value, right: Value) -> std::result::Result<Value, IvyError> {
    Ok(Value::Number(number(&left)? + number(&right)?))
}
pub(super) fn require(ok: bool, code: &str, message: &str) -> std::result::Result<(), IvyError> {
    if ok {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
pub(super) fn iterated(
    value: &Value,
    _expression: &str,
) -> std::result::Result<Vec<Value>, IvyError> {
    v::items(value)
}
fn header(task: &Value, object_id: Value) -> std::result::Result<Value, IvyError> {
    let comments = v::items(task.required("comments")?)?;
    let inputs = comments
        .iter()
        .map(|comment| {
            let mut input = obj([
                ("commentId", comment.required("commentId")?.clone()),
                ("delivery", comment.required("delivery")?.clone()),
            ]);
            for key in ["nativeInput", "handoff"] {
                if let Some(value) = comment.get(key).filter(|v| !v.is_null()) {
                    input.insert(key, value.clone());
                }
            }
            Ok(input)
        })
        .collect::<std::result::Result<Vec<_>, IvyError>>()?;
    Ok(spread(
        task,
        [
            ("comments", array([])),
            (
                "commentStore",
                obj([
                    ("objectId", object_id),
                    ("length", Value::Number(comments.len() as f64)),
                    ("nativeInputs", array(inputs)),
                ]),
            ),
        ],
    ))
}
pub(crate) async fn write_retained_graph<E, S: LocalMutationSource<E>, F, Fut>(
    source: &S,
    identity: &F,
    task: &Value,
    task_id: &Value,
    scope: &Value,
    version: &Value,
) -> Result<Value, E>
where
    F: Fn(Value) -> Fut,
    Fut: Future<Output = Result<Value, E>>,
{
    let page = source
        .request_owned(
            "objects.query",
            Some(obj([
                ("contractKey", text(CONTRACT)),
                (
                    "where",
                    obj([
                        ("op", text("and")),
                        (
                            "args",
                            array([
                                obj([
                                    ("field", text("object.parentId")),
                                    ("op", text("eq")),
                                    ("value", task_id.clone()),
                                ]),
                                obj([
                                    ("field", text("object.name")),
                                    ("op", text("eq")),
                                    ("value", text("Comments")),
                                ]),
                            ]),
                        ),
                    ]),
                ),
                ("limit", Value::Number(2.0)),
                ("includeArchived", Value::Bool(true)),
            ])),
        )
        .await?;
    let items = v::items(page.required("items")?)?;
    require(
        items.len() <= 1 && page.get("nextCursor").is_none_or(Value::is_null),
        "task_board_identity_conflict",
        "A ticket must have one conversation.",
    )?;
    let existing = if let Some(first) = items.first() {
        Some(
            source
                .request_owned(
                    "objects.read",
                    Some(obj([("objectId", first.required("objectId")?.clone())])),
                )
                .await?,
        )
    } else {
        None
    };
    let current = existing
        .as_ref()
        .and_then(|saved| saved.get("content"))
        .filter(|content| content.get("encoding") == Some(&text("json")))
        .and_then(|content| content.get("value"));
    if let Some(saved) = &existing {
        require(
            saved.required("object")?.get("ownerObjectId") == Some(&Value::Null)
                && saved.required("object")?.get("parentId") == Some(task_id)
                && current.is_some_and(|current| {
                    current.get("taskId") == Some(task_id)
                        && current.get("comments").and_then(Value::as_array).is_some()
                }),
            "task_board_scope_mismatch",
            "The ticket conversation belongs to another owner.",
        )?;
    }
    let proposed = v::items(task.required("comments")?)?;
    let previous = current
        .map(|current| v::items(current.required("comments")?))
        .transpose()?
        .unwrap_or_default();
    for (before, after) in previous.iter().zip(&proposed) {
        require(
            hash(&immutable(before))? == hash(&immutable(after))?,
            "task_board_comment_mismatch",
            "Published comment content is immutable; only conversation metadata can change.",
        )?;
    }
    let mut comments = proposed.clone();
    comments.extend(previous.into_iter().skip(proposed.len()));
    let value = obj([
        ("schemaVersion", Value::Number(1.0)),
        ("taskId", task_id.clone()),
        ("comments", array(comments)),
    ]);
    let mut object_id = existing
        .as_ref()
        .map(|saved| saved.required("object")?.required("id").cloned())
        .transpose()?;
    if current.map(hash).transpose()?.as_ref() != Some(&hash(&value)?) {
        let revision = existing
            .as_ref()
            .map(|saved| saved.required("revision")?.required("revision").cloned())
            .transpose()?
            .unwrap_or(Value::Number(0.0));
        let mutation = identity(text(hash(&obj([
            ("scope", scope.clone()),
            ("comments", value.clone()),
            ("previous", revision),
        ]))?))
        .await?;
        let mut params = obj([
            ("mutationId", mutation),
            ("contractVersion", version.clone()),
            (
                "content",
                obj([("encoding", text("json")), ("value", value)]),
            ),
            ("references", obj([])),
        ]);
        if let Some(saved) = &existing {
            params.insert(
                "objectId",
                saved.required("object")?.required("id")?.clone(),
            );
            params.insert(
                "expectedRevision",
                saved.required("revision")?.required("revision")?.clone(),
            );
        } else {
            params.insert(
                "create",
                obj([
                    ("name", text("Comments")),
                    ("parentId", task_id.clone()),
                    ("ownerObjectId", Value::Null),
                    ("contractKey", text(CONTRACT)),
                ]),
            );
        }
        let saved = source.request_owned("objects.write", Some(params)).await?;
        object_id = Some(saved.required("object")?.required("id")?.clone());
    }
    Ok(header(task, object_id.ok_or_else(IvyError::internal)?)?)
}

#[cfg(test)]
#[path = "../../../../tests/fixtures/task-board-native-comment-writes.rs"]
mod tests;
