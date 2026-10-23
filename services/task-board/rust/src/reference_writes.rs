//! Exact attachment/reference-page retention over the existing raw source port.
#![cfg_attr(not(test), allow(dead_code))]
use super::{
    comment_writes::{Result, array, at, f, hash, number, obj, require, text},
    mutation_ports::LocalMutationSource,
    persistent_reads::json_values::{self as v, Value},
};
use std::{
    collections::{HashMap, HashSet},
    future::Future,
};

use ivy_contracts::IvyError;
fn objects(value: &Value) -> std::result::Result<Vec<Value>, IvyError> {
    v::items(value)?
        .iter()
        .map(|v| v.required("object").cloned())
        .collect()
}
pub(crate) fn task_attachment_pins<E>(task: &Value) -> Result<Value, E> {
    let mut pins = objects(task.required("attachments")?)?;
    for comment in v::items(task.required("comments")?)? {
        pins.extend(objects(comment.required("attachments")?)?);
        if let Some(deliveries) = comment
            .get("handoff")
            .and_then(|h| h.get("deliveries"))
            .filter(|v| !v.is_null())
        {
            for delivery in v::items(deliveries)? {
                pins.extend(objects(delivery.required("artifacts")?)?);
            }
        }
    }
    if let Some(deliveries) = task.get("deliveries").filter(|v| !v.is_null()) {
        for receipt in v::items(deliveries)? {
            pins.push(receipt.required("result")?.clone());
            pins.extend(objects(
                receipt.required("delivery")?.required("artifacts")?,
            )?);
        }
    }
    let mut unique = Vec::new();
    let mut positions = HashMap::new();
    for pin in pins {
        let identity = hash(&pin)?;
        if let Some(index) = positions.get(&identity) {
            unique[*index] = pin;
        } else {
            positions.insert(identity, unique.len());
            unique.push(pin);
        }
    }
    Ok(array(unique))
}
fn pin(result: &Value) -> std::result::Result<Value, IvyError> {
    Ok(obj([
        ("objectId", f(&f(result, "object")?, "id")?),
        ("revision", f(&f(result, "revision")?, "revision")?),
    ]))
}
/// Sequential page publications preserve exact prior revisions. There is no
/// catch or private retry around query/read/identity/write or postwrite pins.
pub(crate) async fn task_references<E, S: LocalMutationSource<E>, I, Fut>(
    source: &S,
    identity: &I,
    task_id: &Value,
    named: &Value,
    attachments: &Value,
) -> Result<Value, E>
where
    I: Fn(Value) -> Fut,
    Fut: Future<Output = Result<Value, E>>,
{
    let mut references = v::fields(named);
    let mut seen = HashSet::new();
    for (_, value) in v::fields(named) {
        seen.insert(hash(&value)?);
    }
    let mut pins = Vec::new();
    for pin in v::items(attachments)? {
        let h = hash(&pin)?;
        if seen.insert(h) {
            pins.push(pin);
        }
    }
    if references.len() + pins.len() <= 256 {
        references.extend(
            pins.into_iter()
                .enumerate()
                .map(|(i, pin)| (format!("attachment-{i}").into(), pin)),
        );
        return Ok(v::object(references));
    }
    require(
        references.len() + pins.len().div_ceil(256) <= 256,
        "limit_exceeded",
        "Task evidence exceeds its reference page capacity.",
    )?;
    for (index, selected) in pins.chunks(256).enumerate() {
        let name = text(format!("Task references {}", index + 1));
        let page = source
            .request_owned(
                "objects.query",
                Some(obj([
                    ("contractKey", text("task-board/reference-page")),
                    ("includeArchived", Value::Bool(true)),
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
                                        ("value", name.clone()),
                                    ]),
                                ]),
                            ),
                        ]),
                    ),
                    ("limit", Value::Number(2.0)),
                ])),
            )
            .await?;
        require(
            number(&v::length(&f(&page, "items")?)?)? <= 1.0
                && !v::present(&(f(&page, "nextCursor")?)),
            "task_board_identity_conflict",
            "A task reference page must have one identity.",
        )?;
        let first = at(&f(&page, "items")?, 0)?;
        let previous = if v::present(&(first)) {
            source
                .request_owned(
                    "objects.read",
                    Some(obj([("objectId", f(&first, "objectId")?)])),
                )
                .await?
        } else {
            Value::Null
        };
        require(
            !v::present(&(previous))
                || (f(&f(&previous, "object")?, "ownerObjectId")?.eq(task_id)
                    && f(&f(&previous, "object")?, "parentId")?.eq(task_id)),
            "task_board_scope_mismatch",
            "The reference page belongs to another ticket.",
        )?;
        let content = obj([
            ("encoding", text("json")),
            (
                "value",
                obj([
                    ("schemaVersion", Value::Number(1.0)),
                    ("taskId", task_id.clone()),
                    ("pins", array(selected.iter().cloned())),
                ]),
            ),
        ]);
        let exact = v::object(
            selected
                .iter()
                .enumerate()
                .map(|(i, pin)| (format!("pin-{i}").into(), pin.clone())),
        );
        if v::present(&(previous))
            && hash(&f(&previous, "content")?)? == hash(&content)?
            && hash(&f(&f(&previous, "revision")?, "references")?)? == hash(&exact)?
        {
            references.push((format!("reference-page-{index}").into(), pin(&previous)?));
            continue;
        }
        let prior = if v::nullish(&previous) {
            Value::Number(0.0)
        } else {
            let revision = f(&f(&previous, "revision")?, "revision")?;
            if v::nullish(&revision) {
                Value::Number(0.0)
            } else {
                revision
            }
        };
        let scope = text(format!(
            "reference-page:{}",
            hash(&obj([
                ("taskId", task_id.clone()),
                ("index", Value::Number(index as f64)),
                ("content", content.clone()),
                ("previous", prior)
            ]))?
        ));
        let mutation = identity(scope).await?;
        let mut request = vec![
            ("contractVersion".into(), text("1.0.0")),
            ("content".into(), content),
            ("references".into(), exact),
            ("mutationId".into(), mutation),
        ];
        if v::present(&(previous)) {
            request.extend([
                ("objectId".into(), f(&f(&previous, "object")?, "id")?),
                (
                    "expectedRevision".into(),
                    f(&f(&previous, "revision")?, "revision")?,
                ),
                (
                    "expectedArchived".into(),
                    f(&f(&previous, "object")?, "effectivelyArchived")?,
                ),
            ]);
        } else {
            request.push((
                "create".into(),
                obj([
                    ("contractKey", text("task-board/reference-page")),
                    ("parentId", task_id.clone()),
                    ("ownerObjectId", task_id.clone()),
                    ("name", name),
                ]),
            ));
        }
        let saved = source
            .request_owned("objects.write", Some(v::object(request)))
            .await?;
        references.push((format!("reference-page-{index}").into(), pin(&saved)?));
    }
    Ok(v::object(references))
}
