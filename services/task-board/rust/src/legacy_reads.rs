use super::{
    map_key_equal, spread, truthy,
    values::{field, has, insert, same, type_error},
};
use ivy_contracts::{IvyError, IvyJson as Json};
use ivy_sdk::task_comments::CommentException;
use std::future::Future;

/// The same inline/prefix materialization used by the typed compatibility API.
/// Missing RPC arguments and source failures are the supplied reader's concern.
pub async fn read_legacy_with<E, F, Fut>(
    value: &Json,
    task_id: Option<&Json>,
    mut read: F,
) -> Result<Option<Json>, E>
where
    E: From<IvyError> + From<CommentException>,
    F: FnMut(Option<Json>) -> Fut,
    Fut: Future<Output = Result<Json, E>>,
{
    let reference = field::<E>(Some(value), "commentStore")?;
    let Some(reference) = reference.filter(|v| truthy(v)) else {
        return Ok(Some(value.clone()));
    };
    if has::<E>(Some(reference), "kind")? {
        return Ok(None);
    }
    let mut params = Json::Object(vec![]);
    insert(
        &mut params,
        "objectId",
        field::<E>(Some(reference), "objectId")?,
    );
    let saved = read(Some(params)).await?;
    let object = field::<E>(Some(&saved), "object")?;
    if field::<E>(object, "contractKey")? != Some(&Json::from("task-board/comments"))
        || field::<E>(object, "ownerObjectId")? != Some(&Json::Null)
        || field::<E>(field::<E>(Some(&saved), "content")?, "encoding")?
            != Some(&Json::from("json"))
    {
        return Err(IvyError::new(
            "task_board_contract_mismatch",
            "The ticket conversation is not a retained comment record.",
        )
        .into());
    }
    let log = field::<E>(field::<E>(Some(&saved), "content")?, "value")?;
    if !same(field::<E>(log, "taskId")?, task_id) || !same(field::<E>(object, "parentId")?, task_id)
    {
        return Err(IvyError::new(
            "task_board_evidence_mismatch",
            "The ticket conversation prefix is unavailable.",
        )
        .into());
    }
    let comments = field::<E>(log, "comments")?.and_then(Json::as_array);
    let length = if let Json::Array(items) = reference {
        Some(items.len() as f64)
    } else {
        field::<E>(Some(reference), "length")?.and_then(Json::as_f64)
    };
    if comments.is_none()
        || !length.is_some_and(|n| {
            n.is_finite()
                && n.fract() == 0.0
                && (0.0..=9007199254740991.0).contains(&n)
                && n <= comments.unwrap().len() as f64
        })
    {
        return Err(IvyError::new(
            "task_board_evidence_mismatch",
            "The ticket conversation prefix is unavailable.",
        )
        .into());
    }
    let states = field::<E>(Some(reference), "nativeInputs")?;
    field::<E>(states, "map")?;
    let states = states
        .and_then(Json::as_array)
        .ok_or_else(|| type_error::<E>("reference.nativeInputs.map is not a function"))?;
    let mut keyed = Vec::new();
    for state in states {
        keyed.push((field::<E>(Some(state), "commentId")?, state));
    }
    let mut hydrated = Vec::new();
    for comment in &comments.unwrap()[..length.unwrap() as usize] {
        // Object destructuring throws before comment.commentId on null.
        if comment.is_null() {
            return Err(type_error(
                "Cannot destructure property 'nativeInput' of 'comment' as it is null.",
            ));
        }
        let mut body = spread(comment);
        for key in ["nativeInput", "handoff", "delivery"] {
            body.remove(key);
        }
        let id = field::<E>(Some(comment), "commentId")?;
        if let Some((_, state)) = keyed.iter().rev().find(|(key, _)| map_key_equal(*key, id)) {
            for (key, value) in spread(state).as_object().unwrap() {
                body.insert(key.clone(), value.clone());
            }
        }
        hydrated.push(body);
    }
    let mut task = spread(value);
    task.remove("commentStore");
    task.insert("comments", Json::Array(hydrated));
    Ok(Some(task))
}
