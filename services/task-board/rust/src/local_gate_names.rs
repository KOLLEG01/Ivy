//! Current gate selection over the same operation, Task and retained index.
use crate::journal_store::{
    mutation_ports::{LocalMutationStore, WriteFailure},
    persistent_reads::json_values::{self as v, Value},
};
use ivy_contracts::{IvyError, limits::MANAGEMENT_FRAME_BYTES};

fn f(value: &Value, key: &str) -> Result<Value, IvyError> {
    v::field(value, key)
}
fn text(value: &str) -> Value {
    Value::String(value.into())
}
fn same(left: &Value, right: &Value) -> Result<bool, ivy_contracts::IvyError> {
    Ok(
        ivy_contracts::json::canonical(&(left), MANAGEMENT_FRAME_BYTES)?
            == ivy_contracts::json::canonical(&(right), MANAGEMENT_FRAME_BYTES)?,
    )
}
fn native_shared(request: &Value) -> Result<bool, IvyError> {
    let kind = f(&f(request, "change")?, "kind")?;
    Ok(["completion", "transcript", "snapshot"]
        .into_iter()
        .any(|expected| kind.eq(&text(expected))))
}

// Bind existing operationGates/retainOperationGates owners without adding a
// source capability. The legacy public selector remains separately explicit.
pub(crate) async fn names_graph<E, S, G, R>(
    store: &S,
    operation: &Value,
    retained: G,
    retain: R,
) -> Result<Vec<Value>, WriteFailure<E>>
where
    S: LocalMutationStore<E>,
    G: FnOnce(Value) -> Result<Vec<Value>, WriteFailure<E>>,
    R: FnOnce(Value, Vec<Value>) -> Result<(), WriteFailure<E>>,
{
    let saved = retained(f(&f(operation, "pin")?, "objectId")?)?;
    if !saved.is_empty() {
        return Ok(saved);
    }
    let request = f(&f(operation, "value")?, "request")?;
    let root = text("TaskBoard publication gate");
    let names = if !v::has(&request, "taskId")? {
        vec![root]
    } else {
        let mut name = ivy_contracts::JsString::from("TaskBoard publication gate ");
        name.0.extend(v::string(&(f(&request, "taskId")?))?.0);
        let task = Value::String(name);
        let action_is = |expected: &str| -> Result<bool, IvyError> {
            Ok(f(&request, "action")?.eq(&text(expected)))
        };
        let action = f(&request, "action")?;
        if ![
            "edit",
            "start",
            "continue",
            "transition",
            "review",
            "cancel",
            "defer",
            "nativeUpdate",
        ]
        .into_iter()
        .any(|expected| action.eq(&text(expected)))
        {
            vec![task]
        } else {
            let previous = store
                .read_graph(
                    "task-board/task",
                    &v::object([
                        ("objectId".into(), f(&request, "taskId")?),
                        ("revision".into(), f(&request, "expectedRevision")?),
                    ]),
                    true,
                    false,
                    false,
                )
                .await?;
            let shared = action_is("edit")?
                && (request.get("parentTaskId").is_some()
                    || !f(&f(&request, "fields")?, "category")?
                        .eq(&f(&f(&f(&previous, "value")?, "fields")?, "category")?)
                    || (request.get("nextSteps").is_some()
                        && !same(
                            &f(&request, "nextSteps")?,
                            &f(&f(&previous, "value")?, "nextSteps")?,
                        )?))
                || action_is("defer")?
                    && !same(
                        &f(&request, "nextSteps")?,
                        &f(&f(&previous, "value")?, "nextSteps")?,
                    )?
                || action_is("transition")?
                || action_is("review")?
                || action_is("cancel")?
                || action_is("start")?
                || action_is("continue")?
                || action_is("nativeUpdate")? && native_shared(&request)?;
            if shared { vec![root, task] } else { vec![task] }
        }
    };
    // Read the original operation's pin again after the Task await, just as
    // the source does. Neither the pin nor captured request is snapshotted.
    retain(f(&f(operation, "pin")?, "objectId")?, names.clone())?;
    Ok(names)
}
