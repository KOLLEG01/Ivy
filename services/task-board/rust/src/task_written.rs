//! Task publication observations, browser notices and reserved-gate release.
//! The owner explicitly selects its accepted notice policy; none is a default.
use crate::{
    category_projection::{CategoryLocale, CategoryProjection},
    operations::Document,
    publication_coordination::{CoordinationStore, PublicationCoordination},
};
use ivy_contracts::{IvyError, IvyJson as Json, IvyResult, JsString, hash::hash_json};
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
                    && (!eq(comment, "purpose", "handoff")
                        || f(task, "parentTaskId")
                            .as_js_string()
                            .is_none_or(|id| id.0.is_empty()))
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
fn eq(value: &Json, key: &str, expected: &str) -> bool {
    value.get(key) == Some(&Json::from(expected))
}

impl<P: TaskWriteProjection, G: TaskWriteGates, N: TaskNoticePolicy>
    TaskWriteObserver<'_, P, G, N>
{
    /// Called after every applied Task step, including a saved-receipt replay.
    /// Notice failure preserves the category observation and stops gate release.
    pub async fn written(&self, operation: &Document, pin: &Json, task: &Json) -> IvyResult<()> {
        let id = pin
            .required("objectId")?
            .as_js_string()
            .ok_or_else(IvyError::internal)?;
        let revision = pin
            .required("revision")?
            .as_f64()
            .ok_or_else(IvyError::internal)?;
        self.projection.observe(id, revision, task)?;
        let request_id = operation
            .value
            .required("request")?
            .required("operationId")?
            .as_js_string()
            .ok_or_else(IvyError::internal)?;
        if let Some(comment) = self.notices.comment(task, request_id)? {
            let mut title = JsString::from("Ivy · ");
            title.0.extend_from_slice(
                &task
                    .required("fields")?
                    .required("title")?
                    .as_js_string()
                    .ok_or_else(IvyError::internal)?
                    .0,
            );
            let tag = hash_json(&Json::Array(vec![
                Json::String(id.clone()),
                comment.required("commentId")?.clone(),
            ]))?;
            let notice = Json::object([
                ("title", Json::String(title)),
                ("body", comment.required("body")?.clone()),
                ("tag", tag.into()),
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
            ]);
            publish_browser_notice(self.client.clone(), &"task-board".into(), &notice).await?;
        }
        let operation_id = operation
            .pin
            .required("objectId")?
            .as_js_string()
            .ok_or_else(IvyError::internal)?;
        if task
            .get("publication")
            .and_then(|p| p.get("objectId"))
            .and_then(Json::as_js_string)
            == Some(operation_id)
        {
            self.gates.release_reserved(operation_id).await?;
        }
        Ok(())
    }
}

fn route_component(value: &JsString) -> IvyResult<String> {
    let mut encoded = String::new();
    for byte in String::from_utf16_lossy(&value.0).bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'*' | b'-' | b'.' | b'_' => {
                encoded.push(byte as char)
            }
            b' ' => encoded.push('+'),
            _ => {
                use std::fmt::Write;
                write!(encoded, "%{byte:02X}").unwrap();
            }
        }
    }
    Ok(encoded)
}
impl<P: TaskWriteProjection, G: TaskWriteGates, N: TaskNoticePolicy> TaskWriteSink
    for TaskWriteObserver<'_, P, G, N>
{
    async fn written(&self, operation: &Document, pin: &Json, task: &Json) -> IvyResult<()> {
        TaskWriteObserver::written(self, operation, pin, task).await
    }
}
