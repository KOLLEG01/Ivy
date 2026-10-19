//! Task publication observations, browser notices and reserved-gate release.
//! The owner explicitly selects its accepted notice policy; none is a default.
use crate::{
    category_projection::{CategoryLocale, CategoryProjection},
    operations::Document,
    publication_coordination::{CoordinationStore, PublicationCoordination},
};
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString, hash::hash_json, json::encode_uri_component,
};
use ivy_sdk::{browser_notice::publish_browser_notice, rpc::RpcClient};
use std::sync::Arc;

pub trait TaskWriteProjection {
    fn observe(&self, id: &JsString, revision: f64, task: &Json) -> IvyResult<()>;
}
impl<S, L: CategoryLocale, X> TaskWriteProjection for CategoryProjection<'_, '_, S, L, X> {
    fn observe(&self, id: &JsString, revision: f64, task: &Json) -> IvyResult<()> {
        CategoryProjection::observe(self, id, revision, task);
        Ok(())
    }
}
#[allow(async_fn_in_trait)]
pub trait TaskWriteGates {
    async fn release_reserved(&self, operation: &JsString) -> IvyResult<()>;
}
impl<S: CoordinationStore, C: Fn() -> String> TaskWriteGates for PublicationCoordination<'_, S, C> {
    async fn release_reserved(&self, operation: &JsString) -> IvyResult<()> {
        self.release(operation, true).await
    }
}
pub trait TaskNoticePolicy {
    fn comment(&self, task: &Json, operation_id: &JsString) -> IvyResult<Option<Json>>;
}
/// Preserve the exact publication's Operation context for every Task step.
/// Implementations must not infer it from a latest record or mutable owner slot.
#[allow(async_fn_in_trait)]
pub trait TaskWriteSink {
    async fn written(&self, operation: &Document, pin: &Json, task: &Json) -> IvyResult<()>;
}
/// Source-compatible question/handoff selection for explicit composition. The
/// owner must qualify this against its finalized current Task/comment overlay.
pub struct TaskCommentNotices;
impl TaskNoticePolicy for TaskCommentNotices {
    fn comment(&self, task: &Json, operation_id: &JsString) -> IvyResult<Option<Json>> {
        let comment = task
            .required("comments")?
            .as_array()
            .ok_or_else(IvyError::internal)?
            .iter()
            .rev()
            .find(|comment| {
                f(comment, "operationId").as_js_string() == Some(operation_id)
                    && eq(comment, "authorKind", "agent")
                    && (eq(comment, "purpose", "question") || eq(comment, "purpose", "handoff"))
            });
        Ok(comment
            .filter(|comment| {
                !eq(f(task, "fields"), "userContact", "ticket")
                    && (!eq(comment, "purpose", "handoff") || !truthy(f(task, "parentTaskId")))
            })
            .cloned())
    }
}
pub struct TaskWriteObserver<'a, P, G, N> {
    pub projection: &'a P,
    pub gates: &'a G,
    pub notices: &'a N,
    pub client: Arc<dyn RpcClient>,
    pub service_node_id: &'a JsString,
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
    value.get(key) == Some(&Json::from(expected))
}
fn truthy(value: &Json) -> bool {
    match value {
        Json::Null => false,
        Json::Bool(v) => *v,
        Json::Number(v) => *v != 0.0 && !v.is_nan(),
        Json::String(v) => !v.0.is_empty(),
        _ => true,
    }
}
// URLSearchParams applies USVString conversion and the form encode set. Reuse
// shared encodeURIComponent, then adjust its few extra unescaped characters.
fn route_component(value: &JsString) -> IvyResult<String> {
    let value = JsString::from(String::from_utf16_lossy(&value.0));
    let mut result = encode_uri_component(&value)?.replace("%20", "+");
    for (character, escaped) in [
        ("!", "%21"),
        ("~", "%7E"),
        ("'", "%27"),
        ("(", "%28"),
        (")", "%29"),
    ] {
        result = result.replace(character, escaped);
    }
    Ok(result)
}
impl<P: TaskWriteProjection, G: TaskWriteGates, N: TaskNoticePolicy>
    TaskWriteObserver<'_, P, G, N>
{
    /// Called after every applied Task step, including a saved-receipt replay.
    /// Notice failure preserves the category observation and stops gate release.
    pub async fn written(&self, operation: &Document, pin: &Json, task: &Json) -> IvyResult<()> {
        let id = s(pin, "objectId")?;
        self.projection.observe(
            id,
            pin.required("revision")?
                .as_f64()
                .ok_or_else(IvyError::internal)?,
            task,
        )?;
        if let Some(comment) = self
            .notices
            .comment(task, s(f(&operation.value, "request"), "operationId")?)?
        {
            let mut title = JsString::from("Ivy · ");
            title.0.extend_from_slice(&s(f(task, "fields"), "title")?.0);
            publish_browser_notice(
                self.client.clone(),
                &"task-board".into(),
                &Json::object([
                    ("title", Json::String(title)),
                    ("body", comment.required("body")?.clone()),
                    (
                        "tag",
                        hash_json(&Json::Array(vec![
                            Json::String(id.clone()),
                            comment.required("commentId")?.clone(),
                        ]))?
                        .into(),
                    ),
                    (
                        "target",
                        Json::object([
                            ("uiId", "task-board-ui".into()),
                            (
                                "fragment",
                                format!(
                                    "#/tasks?node={}&id={}",
                                    route_component(self.service_node_id)?,
                                    route_component(id)?
                                )
                                .into(),
                            ),
                        ]),
                    ),
                ]),
            )
            .await?;
        }
        if f(f(task, "publication"), "objectId") == f(&operation.pin, "objectId") {
            self.gates
                .release_reserved(s(&operation.pin, "objectId")?)
                .await?;
        }
        Ok(())
    }
}
impl<P: TaskWriteProjection, G: TaskWriteGates, N: TaskNoticePolicy> TaskWriteSink
    for TaskWriteObserver<'_, P, G, N>
{
    async fn written(&self, operation: &Document, pin: &Json, task: &Json) -> IvyResult<()> {
        TaskWriteObserver::written(self, operation, pin, task).await
    }
}
