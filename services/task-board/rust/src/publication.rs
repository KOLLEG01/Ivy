//! Deterministic publication steps over the retained prepare/apply protocol.
//! Callback failure does not undo an already published effect or its receipt.
use crate::operations::{Document, Operations, Store, TaskChange, decoded, local};
use ivy_contracts::{IvyJson as Json, IvyResult, JsString};
use std::{future::Future, pin::Pin};

pub type TaskWritten<'a> =
    dyn Fn(Json, Json) -> Pin<Box<dyn Future<Output = IvyResult<()>> + 'a>> + 'a;

pub struct Publication<'a, S, C> {
    pub operations: &'a Operations<S, C>,
    pub operation: &'a Document,
    pub task_written: Option<&'a TaskWritten<'a>>,
    pub index: f64,
}

impl<S: Store, C: Fn() -> String> Publication<'_, S, C> {
    fn id(&self) -> &JsString {
        &self.operation.pin.object_id
    }
    pub fn pin(&self) -> IvyResult<Json> {
        Ok(Json::object([
            ("objectId", Json::String(self.id().clone())),
            ("revision", Json::Number(1.0)),
        ]))
    }
    pub async fn write(&mut self, key: &str, value: &Json, destination: &Json) -> IvyResult<Json> {
        let current = self.index;
        self.index += 1.0;
        let id = &Json::String(self.operation.pin.object_id.clone());
        let operations = self.operations.decoded();
        decoded::json_result(
            local::stage_graph_with(&operations, id, current, key, destination, || {
                std::future::ready(Ok(value.clone()))
            })
            .await,
        )?;
        // Stage validates the index before this exact array-index use.
        let pin =
            decoded::json_result(local::apply_graph(&operations, id, current as usize).await)?;
        if key == "task-board/task"
            && let Some(callback) = self.task_written
        {
            callback(pin.clone(), value.clone()).await?;
        }
        Ok(pin)
    }
    pub async fn write_task_change(
        &mut self,
        value: &Json,
        destination: &Json,
        change: TaskChange,
    ) -> IvyResult<Json> {
        let current = self.index;
        self.index += 1.0;
        let id = self.id();
        self.operations
            .stage_task_change(id, current, destination, value, change)
            .await?;
        let pin = self.operations.apply(id, current as usize).await?;
        if let Some(callback) = self.task_written {
            callback(pin.clone(), value.clone()).await?;
        }
        Ok(pin)
    }
    pub async fn create(&mut self, key: &str, value: &Json, label: &JsString) -> IvyResult<Json> {
        let parent = self
            .operations
            .store
            .root_object_id()
            .map(Json::String)
            .unwrap_or(Json::Null);
        let mut name = label.clone();
        name.0.push(u16::from(b' '));
        name.0.extend_from_slice(&self.id().0);
        self.write(
            key,
            value,
            &Json::object([(
                "create",
                Json::object([("parentId", parent), ("name", Json::String(name))]),
            )]),
        )
        .await
    }
}
