//! Hydrate an inline task or its retained append-only comment prefix.
use ivy_contracts::{IvyError, IvyJson as Json};
use std::future::Future;

fn unavailable() -> IvyError {
    IvyError::new(
        "task_board_evidence_mismatch",
        "The ticket conversation prefix is unavailable.",
    )
}
pub async fn read_legacy_with<E, F, Fut>(
    value: &Json,
    task_id: Option<&Json>,
    mut read: F,
) -> Result<Option<Json>, E>
where
    E: From<IvyError>,
    F: FnMut(Option<Json>) -> Fut,
    Fut: Future<Output = Result<Json, E>>,
{
    let reference = match value.get("commentStore") {
        None | Some(Json::Null) => return Ok(Some(value.clone())),
        Some(reference) => reference,
    };
    if reference.get("kind").is_some() {
        return Ok(None);
    }
    let saved = read(Some(Json::object([(
        "objectId",
        reference.required("objectId")?.clone(),
    )])))
    .await?;
    let object = saved.required("object")?;
    let content = saved.required("content")?;
    if object.get("contractKey") != Some(&Json::from("task-board/comments"))
        || object.get("ownerObjectId") != Some(&Json::Null)
        || content.get("encoding") != Some(&Json::from("json"))
    {
        return Err(IvyError::new(
            "task_board_contract_mismatch",
            "The ticket conversation is not a retained comment record.",
        )
        .into());
    }
    let log = content.required("value")?;
    if task_id.is_none() || log.get("taskId") != task_id || object.get("parentId") != task_id {
        return Err(unavailable().into());
    }
    let comments = log
        .get("comments")
        .and_then(Json::as_array)
        .ok_or_else(unavailable)?;
    let length = reference
        .get("length")
        .and_then(Json::as_f64)
        .ok_or_else(unavailable)?;
    if !length.is_finite()
        || length.fract() != 0.0
        || length < 0.0
        || length > comments.len() as f64
    {
        return Err(unavailable().into());
    }
    let states = reference
        .required("nativeInputs")?
        .as_array()
        .ok_or_else(unavailable)?;
    let mut hydrated = Vec::with_capacity(length as usize);
    for comment in &comments[..length as usize] {
        let mut body = comment.clone();
        for key in ["nativeInput", "handoff", "delivery"] {
            body.remove(key);
        }
        if let Some(state) = states
            .iter()
            .rev()
            .find(|state| state.get("commentId") == comment.get("commentId"))
        {
            for (key, value) in state.as_object().ok_or_else(unavailable)? {
                body.insert(key.clone(), value.clone());
            }
        }
        hydrated.push(body);
    }
    let mut full = value.clone();
    full.remove("commentStore");
    full.insert("comments", Json::Array(hydrated));
    Ok(Some(full))
}
