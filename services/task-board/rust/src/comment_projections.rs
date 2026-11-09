//! Bounded projections of immutable conversation trees.
use super::task_comments::ProjectionCache;
use crate::engine_read::conversation::cross_agent_comment;
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString, hash::hash_json, json::stringify,
};
use std::sync::Mutex;

pub(crate) fn summary(task: &Json, owner: &JsString, open: &[Json]) -> IvyResult<Json> {
    let comments = task
        .required("comments")?
        .as_array()
        .ok_or_else(IvyError::internal)?;
    let questions = open
        .iter()
        .map(|comment| {
            let mut question = Json::object([
                ("commentId", comment.required("commentId")?.clone()),
                ("sequence", comment.required("sequence")?.clone()),
                ("open", Json::Bool(true)),
            ]);
            if let Some(recipient) = comment.get("recipient").filter(|v| !v.is_null()) {
                question.insert("recipient", recipient.clone());
            }
            Ok(question)
        })
        .collect::<IvyResult<Vec<_>>>()?;
    Ok(Json::object([
        (
            "directCount",
            Json::Number(
                comments
                    .iter()
                    .filter(|c| !cross_agent_comment(c, owner))
                    .count() as f64,
            ),
        ),
        ("questions", Json::Array(questions)),
    ]))
}
pub(crate) fn current_summary(task: &Json, owner: &JsString) -> IvyResult<Json> {
    summary(task, owner, &ivy_sdk::open_task_questions(task, None)?)
}
fn summary_key(tree: &Json, owner: &JsString) -> IvyResult<String> {
    hash_json(&Json::object([
        ("taskId", Json::String(owner.clone())),
        ("root", tree.required("root")?.clone()),
        ("model", Json::Number(4.0)),
    ]))
}
pub(super) fn summary_cached(
    summaries: &Mutex<ProjectionCache>,
    tree: &Json,
    owner: &JsString,
) -> IvyResult<Option<Json>> {
    if let Some(summary) = tree.get("summary").filter(|v| !v.is_null()) {
        return Ok(Some(summary.clone()));
    }
    let key = summary_key(tree, owner)?;
    Ok(summaries
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(&key)
        .cloned())
}
pub(super) fn tree(value: &Json) -> Option<&Json> {
    value
        .get("commentStore")
        .filter(|v| v.get("kind").is_some())
}
pub(super) fn prepare(mut value: Json) -> Option<(Json, Json)> {
    tree(&value)?;
    let tree = value.remove("commentStore").unwrap();
    Some((tree, value))
}
pub(super) fn with_comments(mut header: Json, entries: &mut [Json]) -> IvyResult<Json> {
    let mut comments = Vec::with_capacity(entries.len());
    for entry in entries {
        entry.required("comment")?;
        comments.push(entry.remove("comment").unwrap());
    }
    header.insert("comments", Json::Array(comments));
    Ok(header)
}
pub(super) fn finish(
    summaries: &Mutex<ProjectionCache>,
    mut header: Json,
    tree: &Json,
    owner: &JsString,
    entries: Vec<Json>,
    open: &[Json],
) -> IvyResult<Json> {
    header.insert(
        "commentDeliveries",
        Json::Array(
            entries
                .into_iter()
                .filter_map(|mut entry| entry.remove("receipt").filter(|v| !v.is_null()))
                .collect(),
        ),
    );
    if tree.get("summary").is_none_or(Json::is_null) {
        let summary = summary(&header, owner, open)?;
        if stringify(&summary, 64 * 1024).is_ok() {
            let bytes = summary.retained_bytes();
            summaries
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .remember(summary_key(tree, owner)?, summary, bytes);
        }
    }
    Ok(header)
}
