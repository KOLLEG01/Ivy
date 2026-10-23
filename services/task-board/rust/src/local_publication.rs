//! Publication's common stage/apply/callback ordering on actual owner values.
use crate::{
    journal_store::{
        mutation_ports::{LocalMutationStore, WriteFailure},
        persistent_reads::json_values::{self as v, Value},
    },
    operations::{
        Operations,
        local::{apply_graph, stage_graph_with},
    },
};
use std::future::Future;

pub(crate) struct Steps<'a, S, C, W> {
    pub(crate) operations: &'a Operations<S, C>,
    pub(crate) operation: &'a Value,
    pub(crate) task_written: &'a W,
    pub(crate) index: &'a mut f64,
}
impl<S, C, W> Steps<'_, S, C, W> {
    pub(crate) fn pin<E>(&self) -> Result<Value, WriteFailure<E>> {
        Ok(v::object([
            (
                "objectId".into(),
                v::field(&v::field(self.operation, "pin")?, "objectId")?,
            ),
            ("revision".into(), Value::Number(1.0)),
        ]))
    }
    pub(crate) async fn write<E, F>(
        &mut self,
        key: &str,
        value: &Value,
        destination: &Value,
    ) -> Result<Value, WriteFailure<E>>
    where
        S: LocalMutationStore<E>,
        C: Fn() -> String,
        W: Fn(Value, Value) -> F,
        F: Future<Output = Result<(), WriteFailure<E>>>,
    {
        write_graph_with(
            self.operations,
            self.operation,
            self.index,
            key,
            value,
            destination,
            || std::future::ready(Ok(value.clone())),
            |pin, value| (self.task_written)(pin, value),
        )
        .await
    }
    pub(crate) async fn create<E, F>(
        &mut self,
        key: &str,
        value: &Value,
        label: &ivy_contracts::JsString,
    ) -> Result<Value, WriteFailure<E>>
    where
        S: LocalMutationStore<E>,
        C: Fn() -> String,
        W: Fn(Value, Value) -> F,
        F: Future<Output = Result<(), WriteFailure<E>>>,
    {
        let parent = self.operations.store.root_object_id_graph();
        let mut name = label.clone();
        name.0.push(u16::from(b' '));
        name.0
            .extend(v::string(&(v::field(&v::field(self.operation, "pin")?, "objectId")?))?.0);
        self.write(
            key,
            value,
            &v::object([(
                "create".into(),
                v::object([
                    ("parentId".into(), parent),
                    ("name".into(), Value::String(name)),
                ]),
            )]),
        )
        .await
    }
}

pub(crate) async fn write_graph_with<
    E,
    S: LocalMutationStore<E>,
    C: Fn() -> String,
    Prepare: Future<Output = Result<Value, WriteFailure<E>>>,
    Callback: Future<Output = Result<(), WriteFailure<E>>>,
>(
    operations: &Operations<S, C>,
    operation: &Value,
    index: &mut f64,
    key: &str,
    value: &Value,
    destination: &Value,
    prepare: impl FnOnce() -> Prepare,
    callback: impl FnOnce(Value, Value) -> Callback,
) -> Result<Value, WriteFailure<E>> {
    let current = *index;
    *index += 1.0;
    let id = v::field(&v::field(operation, "pin")?, "objectId")?;
    stage_graph_with(operations, &id, current, key, destination, prepare).await?;
    // stage checked the index before this exact array-index use.
    let pin = apply_graph(operations, &id, current as usize).await?;
    if key == "task-board/task" {
        callback(pin.clone(), value.clone()).await?;
    }
    Ok(pin)
}
