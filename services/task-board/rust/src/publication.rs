//! Deterministic publication steps over the retained prepare/apply protocol.
//! Callback failure does not undo an already published effect or its receipt.
use crate::operations::{Document, Operations, Store};
use ivy_contracts::{IvyError, IvyJson as Json, IvyResult, JsString};
use std::{future::Future, pin::Pin};

#[path = "local_publication.rs"]
pub(crate) mod local;

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
        use crate::operations::decoded;
        let operation = (&Json::object([("pin", self.operation.pin.clone())])).clone();
        let value = (value).clone();
        let callback = self.task_written;
        let written = |pin, value| async move {
            if let Some(callback) = callback {
                callback(
                    decoded::json_result(Ok(pin))?,
                    decoded::json_result(Ok(value))?,
                )
                .await?;
            }
            Ok(())
        };
        decoded::json_result(
            local::Steps {
                operations: &self.operations.decoded(),
                operation: &operation,
                task_written: &written,
                index: &mut self.index,
            }
            .write(key, &value, &(destination).clone())
            .await,
        )
    }
    pub async fn create(&mut self, key: &str, value: &Json, label: &JsString) -> IvyResult<Json> {
        use crate::operations::decoded;
        self.id()?;
        let operation = (&Json::object([("pin", self.operation.pin.clone())])).clone();
        let callback = self.task_written;
        let written = |pin, value| async move {
            if let Some(callback) = callback {
                callback(
                    decoded::json_result(Ok(pin))?,
                    decoded::json_result(Ok(value))?,
                )
                .await?;
            }
            Ok(())
        };
        decoded::json_result(
            local::Steps {
                operations: &self.operations.decoded(),
                operation: &operation,
                task_written: &written,
                index: &mut self.index,
            }
            .create(key, &(value).clone(), label)
            .await,
        )
    }
}
