//! Durable task-key reservations. The store owns validation, CAS and mutation
//! receipts; an uncertain write is returned to its caller without blind replay.
use crate::{
    journal_store::{mutation_ports::WriteFailure, persistent_reads::json_values::Value},
    operations::{Store, decoded},
};
use ivy_contracts::{IvyError, IvyJson as Json, IvyResult, JsString};
#[path = "local_task_keys.rs"]
pub(crate) mod local;

fn retained_key(value: &Json, operation_id: &JsString, message: &str) -> IvyResult<JsString> {
    if value.get("operationId").and_then(Json::as_js_string) != Some(operation_id) {
        return Err(IvyError::new("task_board_identity_conflict", message));
    }
    value
        .required("taskKey")?
        .as_js_string()
        .cloned()
        .ok_or_else(IvyError::internal)
}

pub async fn allocate<S: Store, C: Fn() -> String>(
    store: &S,
    now: &C,
    operation_id: &JsString,
) -> IvyResult<JsString> {
    let store = decoded::DecodedStore(store);
    let calls = local::Calls {
        store: &store,
        now: || Value::String(now().into()),
        // Keep the public facade's original admitted number/string domain.
        // Actual raw callers bind the original expression/retained handles.
        increment: |value| {
            let value = decoded::json_result(Ok(value))?;
            let next = value
                .required("nextValue")?
                .as_f64()
                .ok_or_else(IvyError::internal)?
                + 1.0;
            Ok::<_, WriteFailure<()>>(Value::Number(next))
        },
        retained: |value, _id, message| {
            Ok::<_, WriteFailure<()>>(Value::String(retained_key(
                &decoded::json_result(Ok(value))?,
                operation_id,
                message,
            )?))
        },
    };
    decoded::json_result(local::allocate_graph(&calls, &Value::String(operation_id.clone())).await)?
        .as_js_string()
        .cloned()
        .ok_or_else(IvyError::internal)
}
