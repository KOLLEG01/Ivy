//! Deterministic publication steps over the retained prepare/apply protocol.
//! Callback failure does not undo an already published effect or its receipt.
use crate::operations::{Document, Operations, Store};
use ivy_contracts::{IvyError, IvyJson as Json, IvyResult, JsString};
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
    fn id(&self) -> IvyResult<&JsString> {
        self.operation
            .pin
            .required("objectId")?
            .as_js_string()
            .ok_or_else(IvyError::internal)
    }
    pub fn pin(&self) -> IvyResult<Json> {
        Ok(Json::object([
            ("objectId", Json::String(self.id()?.clone())),
            ("revision", Json::Number(1.0)),
        ]))
    }
    pub async fn write(&mut self, key: &str, value: &Json, destination: &Json) -> IvyResult<Json> {
        let index = self.index;
        self.index += 1.0;
        self.operations
            .stage(self.id()?, index, key, destination, value)
            .await?;
        let pin = self.operations.apply(self.id()?, index as usize).await?;
        if key == "task-board/task" {
            if let Some(callback) = self.task_written {
                callback(pin.clone(), value.clone()).await?;
            }
        }
        Ok(pin)
    }
    pub async fn create(&mut self, key: &str, value: &Json, label: &JsString) -> IvyResult<Json> {
        let mut name = label.clone();
        name.0.push(u16::from(b' '));
        name.0.extend_from_slice(&self.id()?.0);
        self.write(
            key,
            value,
            &Json::object([(
                "create",
                Json::object([
                    (
                        "parentId",
                        self.operations
                            .store
                            .root_object_id()
                            .map(Json::String)
                            .unwrap_or(Json::Null),
                    ),
                    ("name", Json::String(name)),
                ]),
            )]),
        )
        .await
    }
}
