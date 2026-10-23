//! Task-key allocation retries only conclusive write conflicts.
use crate::{
    journal_store::{
        mutation_ports::{LocalMutationStore, WriteFailure, typed_write_cause},
        persistent_reads::json_values::{self as v, Value},
    },
    operations::local::{identity_hash, mutation},
};
use ivy_contracts::IvyError;

const ALLOCATION: &str = "task-board/task-key-allocation";
const SEQUENCE: &str = "task-board/task-key-sequence";
const SEQUENCE_NAME: &str = "Task key sequence";
pub(crate) struct Calls<'a, S, C, I, R> {
    pub(crate) store: &'a S,
    pub(crate) now: C,
    pub(crate) increment: I,
    pub(crate) retained: R,
}
fn f(value: &Value, key: &str) -> Result<Value, IvyError> {
    v::field(value, key)
}
fn text(value: &str) -> Value {
    Value::String(value.into())
}
fn object(fields: impl IntoIterator<Item = (&'static str, Value)>) -> Value {
    v::object(fields.into_iter().map(|(key, value)| (key.into(), value)))
}
fn conflict<E>(error: &WriteFailure<E>) -> bool {
    typed_write_cause(error).is_some_and(|error| {
        matches!(
            error.code.as_str(),
            "revision_conflict" | "mutation_conflict"
        )
    })
}
pub(crate) fn increment(value: &Value) -> Result<Value, IvyError> {
    let next = v::number(value.required("nextValue")?)?;
    if next < 0.0 || next.fract() != 0.0 || next >= 9007199254740991.0 {
        return Err(v::invalid(
            "Task key sequence is outside its integer range.",
        ));
    }
    Ok(Value::Number(next + 1.0))
}
pub(crate) fn retained<E>(
    value: Value,
    id: Value,
    message: &'static str,
) -> Result<Value, WriteFailure<E>> {
    if !f(&value, "operationId")?.eq(&id) {
        return Err(IvyError::new("task_board_identity_conflict", message).into());
    }
    Ok(f(&value, "taskKey")?)
}
fn create<E, S: LocalMutationStore<E>>(store: &S, name: Value) -> Value {
    object([(
        "create",
        object([("parentId", store.root_object_id_graph()), ("name", name)]),
    )])
}
pub(crate) async fn allocate_graph<E, S, C, I, R>(
    calls: &Calls<'_, S, C, I, R>,
    id: &Value,
) -> Result<Value, WriteFailure<E>>
where
    S: LocalMutationStore<E>,
    C: Fn() -> Value,
    I: Fn(Value) -> Result<Value, WriteFailure<E>>,
    R: Fn(Value, Value, &'static str) -> Result<Value, WriteFailure<E>>,
{
    let allocation_name = text(&format!(
        "Task key allocation {}",
        identity_hash(&text("task-board"), id)?
    ));
    if let Some(saved) = calls
        .store
        .named_graph(ALLOCATION, &allocation_name, None)
        .await?
    {
        return (calls.retained)(
            f(&saved, "value")?,
            id.clone(),
            "The retained Task key allocation belongs to another operation.",
        );
    }
    loop {
        let sequence = match calls
            .store
            .named_graph(SEQUENCE, &text(SEQUENCE_NAME), None)
            .await?
        {
            Some(sequence) => sequence,
            None => {
                let initialized = async {
                    let value = object([
                        ("schemaVersion", Value::Number(1.0)),
                        ("nextValue", Value::Number(0.0)),
                        ("updatedAt", (calls.now)()),
                    ]);
                    let pin = calls
                        .store
                        .write_graph(
                            SEQUENCE,
                            &value,
                            &mutation(&text("task-key-sequence"), text("initialize"))?,
                            &create::<E, _>(calls.store, text(SEQUENCE_NAME)),
                        )
                        .await?;
                    calls
                        .store
                        .read_graph(SEQUENCE, &pin, true, false, false)
                        .await
                }
                .await;
                match initialized {
                    Ok(sequence) => sequence,
                    Err(error) if conflict(&error) => continue,
                    Err(error) => return Err(error),
                }
            }
        };
        let next = (calls.increment)(f(&sequence, "value")?)?;
        let value = object([
            ("schemaVersion", Value::Number(1.0)),
            ("nextValue", next.clone()),
            ("updatedAt", (calls.now)()),
        ]);
        let mut step = ivy_contracts::JsString::from("allocate-task-key:");
        step.0
            .extend(ivy_contracts::JsString::from(format!("{:.0}", v::number(&next)?)).0);
        let advanced = calls
            .store
            .write_graph(
                SEQUENCE,
                &value,
                &mutation(id, Value::String(step))?,
                &object([
                    ("objectId", f(&f(&sequence, "pin")?, "objectId")?),
                    ("expectedRevision", f(&f(&sequence, "pin")?, "revision")?),
                ]),
            )
            .await;
        match advanced {
            Ok(_) => {}
            Err(error) if conflict(&error) => continue,
            Err(error) => return Err(error),
        }
        let number = ivy_contracts::JsString::from(format!("{:.0}", v::number(&next)?));
        let mut task_key = ivy_contracts::JsString::from("TASK-");
        task_key.0.extend(std::iter::repeat_n(
            u16::from(b'0'),
            4usize.saturating_sub(number.0.len()),
        ));
        task_key.0.extend(number.0);
        let value = object([
            ("schemaVersion", Value::Number(1.0)),
            ("operationId", id.clone()),
            ("taskKey", Value::String(task_key)),
            ("createdAt", (calls.now)()),
        ]);
        match calls
            .store
            .write_graph(
                ALLOCATION,
                &value,
                &mutation(id, text("retain-task-key"))?,
                &create::<E, _>(calls.store, allocation_name.clone()),
            )
            .await
        {
            Ok(_) => return Ok(f(&value, "taskKey")?),
            Err(error) if conflict(&error) => {}
            Err(error) => return Err(error),
        }
        let message =
            "The competing Task key allocation is unavailable or belongs to another operation.";
        let winner = calls
            .store
            .named_graph(ALLOCATION, &allocation_name, None)
            .await?
            .ok_or_else(|| IvyError::new("task_board_identity_conflict", message))?;
        return (calls.retained)(f(&winner, "value")?, id.clone(), message);
    }
}
