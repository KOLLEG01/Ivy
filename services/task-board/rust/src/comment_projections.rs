//! Bounded projections of immutable conversation trees.
use super::task_comments::ProjectionCache;
use crate::engine_read::conversation::cross_agent_comment;
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString, hash::hash_json, json::stringify,
};
use std::{collections::BTreeSet, sync::Mutex};

pub(crate) fn summary(task: &Json, owner: &JsString, open: &[JsString]) -> IvyResult<Json> {
    let comments = task
        .required("comments")?
        .as_array()
        .ok_or_else(IvyError::internal)?;
    let open: BTreeSet<_> = open.iter().collect();
    let questions = comments
        .iter()
        .filter(|comment| {
            comment.get("purpose") == Some(&Json::from("question"))
                && comment.get("sourceTaskId").is_none_or(Json::is_null)
                && comment
                    .get("commentId")
                    .and_then(Json::as_js_string)
                    .is_some_and(|id| open.contains(id))
        })
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
    let ids = ivy_sdk::open_task_questions(task, None)?
        .iter()
        .map(|q| {
            q.required("commentId")?
                .as_js_string()
                .cloned()
                .ok_or_else(IvyError::internal)
        })
        .collect::<IvyResult<Vec<_>>>()?;
    summary(task, owner, &ids)
}
fn summary_key(tree: &Json, owner: &JsString) -> IvyResult<String> {
    hash_json(&Json::object([
        ("taskId", Json::String(owner.clone())),
        ("root", tree.required("root")?.clone()),
        ("model", Json::Number(3.0)),
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
pub(super) fn prepare(value: &Json) -> Option<(Json, Json)> {
    let tree = value
        .get("commentStore")
        .filter(|v| v.get("kind").is_some())?;
    let mut header = value.clone();
    header.remove("commentStore");
    Some((tree.clone(), header))
}
pub(super) fn finish(
    summaries: &Mutex<ProjectionCache>,
    mut header: Json,
    tree: &Json,
    owner: &JsString,
    entries: &[Json],
    open: &[JsString],
) -> IvyResult<Json> {
    header.insert(
        "comments",
        Json::Array(
            entries
                .iter()
                .map(|entry| entry.required("comment").cloned())
                .collect::<IvyResult<_>>()?,
        ),
    );
    header.insert(
        "commentDeliveries",
        Json::Array(
            entries
                .iter()
                .filter_map(|entry| entry.get("receipt").filter(|v| !v.is_null()).cloned())
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
