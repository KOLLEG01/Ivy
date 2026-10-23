//! Retained relays leave canonical questions and delegated acceptance with their
//! owning tickets. Stable comment identities make later passes idempotent.
use crate::{
    checks::CheckStore,
    engine_checks::{CheckPolicy, EngineChecks},
    engine_invocations::InternalCalls,
    engine_validation::ActionValidationChecks,
    native_lifecycle::LifecycleDocument,
    operations::Document,
};
use ivy_contracts::{IvyError, IvyJson as Json, IvyResult, JsString, hash::hash_json};
use ivy_sdk::{operation_ids::scoped_operation_id, rpc::RpcClient};
use std::{cell::RefCell, ops::Range, sync::Arc};

pub type TaskRead<'a> = dyn Fn(JsString, bool) -> futures_util::future::LocalBoxFuture<'a, IvyResult<LifecycleDocument>>
    + 'a;
#[allow(async_fn_in_trait)]
pub trait RoutingEngine {
    async fn read_task(
        &self,
        id: &JsString,
        include_archived: bool,
    ) -> IvyResult<LifecycleDocument>;
    async fn aggregate(&self, task: &Json, id: &JsString) -> IvyResult<()>;
    async fn scoped_operation(&self, scope: &Json) -> IvyResult<JsString>;
    async fn native_comment(&self, request: &Json) -> IvyResult<()>;
}
/// These effects bind directly to the current hydrated reader, actual aggregate
/// checks and retained internal admission on the generation's owner executor.
pub struct RoutingRuntime<'a, S, P> {
    pub read: &'a TaskRead<'a>,
    pub checks: &'a EngineChecks<'a, S, P>,
    pub client: Arc<dyn RpcClient>,
    pub calls: InternalCalls<'a>,
}
impl<S: CheckStore, P: CheckPolicy> RoutingEngine for RoutingRuntime<'_, S, P> {
    async fn read_task(&self, id: &JsString, archived: bool) -> IvyResult<LifecycleDocument> {
        (self.read)(id.clone(), archived).await
    }
    async fn aggregate(&self, task: &Json, id: &JsString) -> IvyResult<()> {
        self.checks.aggregate(task, id).await
    }
    async fn scoped_operation(&self, scope: &Json) -> IvyResult<JsString> {
        scoped_operation_id(self.client.clone(), scope).await
    }
    async fn native_comment(&self, request: &Json) -> IvyResult<()> {
        self.calls.native_comment(request)?.await.map(|_| ())
    }
}
fn f<'a>(value: &'a Json, key: &str) -> &'a Json {
    static NULL: Json = Json::Null;
    value.get(key).unwrap_or(&NULL)
}
fn s<'a>(value: &'a Json, key: &str) -> IvyResult<&'a JsString> {
    value
        .required(key)?
        .as_js_string()
        .ok_or_else(IvyError::internal)
}
fn eq(value: &Json, key: &str, expected: &str) -> bool {
    f(value, key) == &Json::from(expected)
}
fn comments(value: &Json) -> IvyResult<&[Json]> {
    f(value, "comments")
        .as_array()
        .ok_or_else(IvyError::internal)
}
fn contains(value: &Json, id: &str) -> IvyResult<bool> {
    Ok(comments(value)?
        .iter()
        .any(|comment| f(comment, "commentId") == &Json::from(id)))
}
fn concat(parts: impl IntoIterator<Item = JsString>) -> JsString {
    JsString(parts.into_iter().flat_map(|v| v.0).collect())
}
fn comment(
    id: &str,
    operation: Json,
    source: &JsString,
    target: &Document,
    body: JsString,
    attachments: Json,
) -> Json {
    Json::object([
        ("action", "comment".into()),
        ("operationId", operation),
        ("taskId", f(&target.pin, "objectId").clone()),
        ("expectedRevision", f(&target.pin, "revision").clone()),
        ("commentId", id.into()),
        ("authorKind", "agent".into()),
        ("sourceTaskId", Json::String(source.clone())),
        ("agentDelivery", "queue".into()),
        ("purpose", "update".into()),
        ("body", Json::String(body)),
        ("requests", Json::Array(vec![])),
        ("responses", Json::Array(vec![])),
        ("attachments", attachments),
        ("replyTo", Json::Null),
    ])
}

/// Generation-local progress only: durable target comments remain the authority
/// for deduplication. Small/empty lists discard their cursor; least recently used
/// task entries are evicted at 1,024, including tasks no longer encountered.
#[derive(Default)]
pub struct QuestionRouting {
    cursors: RefCell<Vec<(JsString, usize, usize)>>,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn question_cursors_bound_retained_tasks_and_discard_idle_entries() {
        let routing = QuestionRouting::default();
        for n in 0..1_025 {
            assert_eq!(routing.batch(&format!("task{n}").into(), 25), 0..10);
        }
        assert_eq!(routing.cursors.borrow().len(), 1_024);
        assert_eq!(routing.batch(&"task1".into(), 25), 10..20);
        assert_eq!(routing.batch(&"task0".into(), 25), 0..10);
        assert_eq!(routing.cursors.borrow().len(), 1_024);
        assert_eq!(routing.batch(&"task1".into(), 0), 0..0);
        assert_eq!(routing.cursors.borrow().len(), 1_023);
        assert_eq!(routing.batch(&"task1".into(), 25), 0..10);
        assert_eq!(routing.batch(&"task1".into(), 12), 0..10);
        assert_eq!(routing.batch(&"task1".into(), 11), 0..10);
    }
}
impl QuestionRouting {
    fn batch(&self, id: &JsString, count: usize) -> Range<usize> {
        let mut cursors = self.cursors.borrow_mut();
        let previous = cursors
            .iter()
            .position(|(task, _, _)| task == id)
            .map(|index| cursors.remove(index));
        if count <= 10 {
            return 0..count;
        }
        let start = previous
            .filter(|(_, offset, previous_count)| *offset < count && count >= *previous_count)
            .map_or(0, |(_, offset, _)| offset);
        let end = (start + 10).min(count);
        cursors.push((id.clone(), if end == count { 0 } else { end }, count));
        if cursors.len() > 1_024 {
            cursors.remove(0);
        }
        start..end
    }
    /// Relaying never moves or answers a question. Only the original sender's
    /// current resolver decides whether it is still open.
    pub async fn route<E: RoutingEngine>(&self, engine: &E, id: &JsString) -> IvyResult<()> {
        let source = engine.read_task(id, false).await?;
        if !f(&source.document.value, "publication").is_null() {
            return Ok(());
        }
        let questions = ivy_sdk::open_task_questions(&source.document.value, None)?;
        let questions: Vec<_> = questions
            .iter()
            .filter(|q| eq(f(q, "recipient"), "kind", "task"))
            .collect();
        for question in &questions[self.batch(id, questions.len())] {
            let target_id = s(f(question, "recipient"), "taskId")?;
            let target = engine.read_task(target_id, false).await?;
            let hash = hash_json(&Json::object([
                ("taskId", Json::String(id.clone())),
                ("commentId", f(question, "commentId").clone()),
            ]))?;
            let comment_id = format!("question:{}", &hash[7..]);
            if contains(&target.document.value, &comment_id)? {
                continue;
            }
            let revision = f(&target.document.pin, "revision")
                .as_f64()
                .ok_or_else(IvyError::internal)?;
            let operation = format!("{comment_id}:{revision:.0}");
            let mut body = concat([
                "Question from ".into(),
                s(&source.document.value, "taskKey")?.clone(),
                " to this ticket:\n\n".into(),
                s(question, "body")?.clone(),
                "\n\nAnswer in the original ticket ".into(),
                id.clone(),
                " with sourceTaskId=".into(),
                s(&target.document.pin, "objectId")?.clone(),
                " and replyTo=".into(),
                s(question, "commentId")?.clone(),
                ". This question is addressed to this task; it is not a user question.".into(),
            ]);
            body.0.truncate(65_536);
            let mut request = comment(
                &comment_id,
                operation.into(),
                id,
                &target.document,
                body,
                f(question, "attachments").clone(),
            );
            request.insert(
                "questionSource",
                Json::object([
                    ("taskId", Json::String(id.clone())),
                    ("commentId", f(question, "commentId").clone()),
                ]),
            );
            engine.native_comment(&request).await?;
        }
        Ok(())
    }
}

/// Forward each genuinely ready saved result once. This sends no acceptance,
/// workflow transition or archive request on behalf of its parent.
pub async fn forward_delegated_review<E: RoutingEngine>(
    engine: &E,
    child: &Document,
) -> IvyResult<()> {
    let value = &child.value;
    let parent = f(value, "parentTaskId");
    let result = f(value, "latestResult");
    if parent.is_null()
        || !eq(value, "workflowState", "review")
        || result.is_null()
        || !f(value, "claim").is_null()
        || !f(value, "publication").is_null()
    {
        return Ok(());
    }
    let parent = engine
        .read_task(parent.as_js_string().ok_or_else(IvyError::internal)?, true)
        .await?;
    if f(&parent.metadata, "effectivelyArchived").as_bool() == Some(true)
        || !f(&parent.document.value, "publication").is_null()
        || ["done", "cancelled"]
            .iter()
            .any(|state| eq(&parent.document.value, "workflowState", state))
    {
        return Ok(());
    }
    let child_id = s(&child.pin, "objectId")?;
    let hash = hash_json(&Json::Array(vec![
        Json::String(child_id.clone()),
        result.clone(),
    ]))?;
    let comment_id = format!("delegated-review-{}", &hash[7..]);
    if contains(&parent.document.value, &comment_id)? {
        return Ok(());
    }
    engine.aggregate(value, child_id).await?;
    let operation = engine
        .scoped_operation(&Json::Array(vec![
            "delegated-review".into(),
            Json::String(child_id.clone()),
            result.clone(),
            parent.document.pin.clone(),
        ]))
        .await?;
    let body=concat(["Delegated task ".into(),s(value,"taskKey")?.clone(),
        " is ready for your review. Read its saved final result, verify the delivered scope and integration, then move the accepted task to Done and archive it. Request corrections when needed; its Review is your responsibility.".into()]);
    engine
        .native_comment(&comment(
            &comment_id,
            Json::String(operation),
            child_id,
            &parent.document,
            body,
            Json::Array(vec![]),
        ))
        .await
}
