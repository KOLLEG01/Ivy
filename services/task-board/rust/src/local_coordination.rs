//! Serialize publication mutations through retained operation records.
use crate::{
    journal_store::{
        mutation_ports::{LocalMutationStore, WriteFailure, typed_write_cause},
        persistent_reads::json_values::{self as v, Value},
    },
    operations::{Document, local::mutation},
};
use ivy_contracts::{IvyError, JsString, limits::MANAGEMENT_FRAME_BYTES};
use std::{future::Future, ops::AsyncFn};

const GATE: &str = "task-board/coordination";
pub(crate) struct Calls<'a, S, N, G, C> {
    pub(crate) store: &'a S,
    pub(crate) principal: &'a Value,
    pub(crate) named: N,
    pub(crate) gates: G,
    pub(crate) now: C,
}
fn f<'a>(value: &'a Value, key: &str) -> &'a Value {
    v::field(value, key)
}
fn text(value: &str) -> Value {
    Value::String(value.into())
}
fn object(fields: impl IntoIterator<Item = (&'static str, Value)>) -> Value {
    v::object(fields.into_iter().map(|(key, value)| (key.into(), value)))
}
fn changed(value: &Value, fields: impl IntoIterator<Item = (&'static str, Value)>) -> Value {
    v::object(
        v::fields(value)
            .iter()
            .cloned()
            .chain(fields.into_iter().map(|(k, v)| (k.into(), v))),
    )
}
fn require(ok: bool, code: &str, message: &str) -> Result<(), IvyError> {
    if ok {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn finished(value: &Value) -> bool {
    let phase = f(value, "phase");
    phase.is_str("succeeded") || phase.is_str("failed")
}
fn concat(values: impl IntoIterator<Item = Value>) -> Result<Value, IvyError> {
    let mut result = JsString::from("");
    for value in values {
        result.0.extend(v::string(&(value))?.0);
    }
    Ok(Value::String(result))
}
fn mutation_step(prefix: &str, revision: f64, id: &Value) -> Result<Value, IvyError> {
    let mut step = JsString::from(format!("{prefix}:{revision:.0}:"));
    step.0.extend(v::string(id)?.0);
    Ok(Value::String(step))
}
fn active_id(value: &Value) -> &Value {
    f(f(value, "activeOperation"), "objectId")
}
fn positive(value: &Value) -> Result<bool, IvyError> {
    Ok(v::number(value)? > 0.0)
}
impl<S, N, G, C> Calls<'_, S, N, G, C> {
    pub(crate) async fn current<E, NF>(&self, name: &Value) -> Result<Document, WriteFailure<E>>
    where
        S: LocalMutationStore<E>,
        N: Fn(Value) -> NF,
        NF: Future<Output = Result<Option<Document>, WriteFailure<E>>>,
        C: Fn() -> Value,
    {
        let mut value = (self.named)(name.clone()).await?;
        if value.is_none() {
            let created = async {
                let initial = object([
                    ("schemaVersion", Value::Number(1.0)),
                    ("principalId", self.principal.clone()),
                    ("activeOperation", Value::Null),
                    ("updatedAt", (self.now)()),
                ]);
                let root = self.store.root_object_id_graph();
                let scope = if v::nullish(&root) {
                    text("root")
                } else {
                    root
                };
                let pin = self
                    .store
                    .write_graph(
                        GATE,
                        &initial,
                        &mutation(&scope, concat([text("coordination:"), name.clone()])?)?,
                        &object([(
                            "create",
                            object([
                                ("parentId", self.store.root_object_id_graph()),
                                ("name", name.clone()),
                            ]),
                        )]),
                    )
                    .await?;
                self.store.read_graph(GATE, &pin, true, false, false).await
            }
            .await;
            match created {
                Ok(document) => value = Some(document),
                Err(error)
                    if typed_write_cause(&error).is_some_and(|error| {
                        matches!(
                            error.code.as_str(),
                            "mutation_conflict" | "revision_conflict"
                        )
                    }) =>
                {
                    value = (self.named)(name.clone()).await?;
                }
                Err(error) => return Err(error),
            }
        }
        let valid = match &value {
            Some(value) => f(&value.value, "principalId").eq(self.principal),
            None => false,
        };
        require(
            valid,
            "task_board_principal_mismatch",
            "This workflow publication gate belongs to another logical principal.",
        )?;
        Ok(value.unwrap())
    }
    pub(crate) async fn acquire<E, NF>(&self, operation: &Document) -> Result<(), WriteFailure<E>>
    where
        S: LocalMutationStore<E>,
        N: Fn(Value) -> NF,
        NF: Future<Output = Result<Option<Document>, WriteFailure<E>>>,
        G: AsyncFn(&Document) -> Result<Vec<Value>, WriteFailure<E>>,
        C: Fn() -> Value,
    {
        let id = &Value::String(operation.pin.object_id.clone());
        let pin = object([("objectId", id.clone()), ("revision", Value::Number(1.0))]);
        for name in (self.gates)(operation).await? {
            let current = self.current(&name).await?;
            let value = &current.value;
            let active = f(&value, "activeOperation");
            if v::present(&(active)) {
                require(
                    ivy_contracts::json::canonical_equal(active, &pin, MANAGEMENT_FRAME_BYTES)?,
                    "task_board_publication_busy",
                    "Reconcile the recorded publication before starting another workflow action.",
                )?;
                continue;
            }
            let current_pin = &current.pin;
            let next = changed(
                &value,
                [
                    ("activeOperation", pin.clone()),
                    ("updatedAt", f(&operation.value, "createdAt").clone()),
                ],
            );
            self.store
                .write_graph(
                    GATE,
                    &next,
                    &mutation(
                        &Value::String(current_pin.object_id.clone()),
                        mutation_step("acquire", current_pin.revision, &id)?,
                    )?,
                    &object([
                        ("objectId", Value::String(current_pin.object_id.clone())),
                        ("expectedRevision", Value::Number(current_pin.revision)),
                    ]),
                )
                .await?;
        }
        Ok(())
    }
    pub(crate) async fn reserved<E>(&self, operation: &Document) -> Result<bool, WriteFailure<E>>
    where
        S: LocalMutationStore<E>,
    {
        let first = v::at(f(&operation.value, "writes"), 0)?;
        if !v::present(&(first))
            || !f(&first, "contractKey").eq(&text("task-board/task"))
            || !v::present(&(f(&first, "outcome")))
        {
            return Ok(false);
        }
        let payload = self
            .store
            .read_graph(
                "task-board/task",
                f(f(&first, "payload"), "object"),
                false,
                false,
                true,
            )
            .await?;
        let publication = f(&payload.value, "publication");
        let publication_id = f(publication, "objectId");
        require(
            publication_id.eq(&Value::String(operation.pin.object_id.clone()))
                && f(&publication, "revision").eq(&Value::Number(1.0)),
            "task_board_publication_pending",
            "The first Task write must durably retain its owning publication marker.",
        )?;
        let destination = f(&first, "destination");
        require(
            v::has(&destination, "create")
                || f(&destination, "objectId").eq(f(f(&first, "outcome"), "objectId")),
            "task_board_stage_conflict",
            "The retained Task reservation receipt names another destination.",
        )?;
        Ok(true)
    }
    pub(crate) async fn release<E, NF>(
        &self,
        id: &Value,
        reserved: bool,
    ) -> Result<(), WriteFailure<E>>
    where
        S: LocalMutationStore<E>,
        N: Fn(Value) -> NF,
        NF: Future<Output = Result<Option<Document>, WriteFailure<E>>>,
        G: AsyncFn(&Document) -> Result<Vec<Value>, WriteFailure<E>>,
        C: Fn() -> Value,
    {
        let operation = self
            .store
            .read_graph("task-board/operation", id, true, false, false)
            .await?;
        let task_reserved = reserved && self.reserved(&operation).await?;
        let value = &operation.value;
        for name in (self.gates)(&operation).await? {
            let current = self.current(&name).await?;
            if !active_id(&current.value).eq(id) {
                continue;
            }
            require(
                finished(&value) || task_reserved,
                "task_board_publication_pending",
                "An unfinished publication gate cannot be released before its first Task reservation is durable.",
            )?;
            self.clear(&current, id, f(&value, "updatedAt")).await?;
        }
        Ok(())
    }
    pub(crate) async fn release_unstarted<E, NF>(&self, id: &Value) -> Result<bool, WriteFailure<E>>
    where
        S: LocalMutationStore<E>,
        N: Fn(Value) -> NF,
        NF: Future<Output = Result<Option<Document>, WriteFailure<E>>>,
        G: AsyncFn(&Document) -> Result<Vec<Value>, WriteFailure<E>>,
        C: Fn() -> Value,
    {
        let operation = self
            .store
            .read_graph("task-board/operation", id, true, false, false)
            .await?;
        let value = &operation.value;
        if finished(&value) || positive(&v::length(f(&value, "writes"))?)? {
            return Ok(false);
        }
        let mut released = false;
        for name in (self.gates)(&operation).await? {
            let current = self.current(&name).await?;
            if !active_id(&current.value).eq(id) {
                continue;
            }
            self.clear(&current, id, f(&value, "updatedAt")).await?;
            released = true;
        }
        Ok(released)
    }
    async fn clear<E>(
        &self,
        current: &Document,
        id: &Value,
        at: &Value,
    ) -> Result<(), WriteFailure<E>>
    where
        S: LocalMutationStore<E>,
    {
        let pin = &current.pin;
        self.store
            .write_graph(
                GATE,
                &changed(
                    &current.value,
                    [("activeOperation", Value::Null), ("updatedAt", at.clone())],
                ),
                &mutation(
                    &Value::String(pin.object_id.clone()),
                    mutation_step("release", pin.revision, &id)?,
                )?,
                &object([
                    ("objectId", Value::String(pin.object_id.clone())),
                    ("expectedRevision", Value::Number(pin.revision)),
                ]),
            )
            .await?;
        Ok(())
    }
}
