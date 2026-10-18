//! Durable task-key reservations. The store owns validation, CAS and mutation
//! receipts; an uncertain write is returned to its caller without blind replay.
use crate::operations::{Store, identity_hash, mutation};
use ivy_contracts::{IvyError, IvyJson as Json, IvyResult, JsString, json::try_js_string};

const ALLOCATION: &str = "task-board/task-key-allocation";
const SEQUENCE: &str = "task-board/task-key-sequence";
const SEQUENCE_NAME: &str = "Task key sequence";

fn conflict(error: &IvyError) -> bool {
    matches!(
        error.code.as_str(),
        "revision_conflict" | "mutation_conflict"
    )
}

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
    let name = format!(
        "Task key allocation {}",
        identity_hash(&"task-board".into(), operation_id)?
    );
    if let Some(retained) = store.named(ALLOCATION, &name, None).await? {
        return retained_key(
            &retained.value,
            operation_id,
            "The retained Task key allocation belongs to another operation.",
        );
    }
    loop {
        let sequence = match store.named(SEQUENCE, SEQUENCE_NAME, None).await? {
            Some(sequence) => sequence,
            None => {
                let initialized = async {
                    let pin = store
                        .write(
                            SEQUENCE,
                            &Json::object([
                                ("schemaVersion", Json::Number(1.0)),
                                ("nextValue", Json::Number(0.0)),
                                ("updatedAt", now().into()),
                            ]),
                            &mutation(&"task-key-sequence".into(), "initialize")?,
                            &create(store, SEQUENCE_NAME),
                        )
                        .await?;
                    store.read(SEQUENCE, &pin, true).await
                }
                .await;
                match initialized {
                    Ok(sequence) => sequence,
                    Err(error) if conflict(&error) => continue,
                    Err(error) => return Err(error),
                }
            }
        };
        let next = sequence
            .value
            .required("nextValue")?
            .as_f64()
            .ok_or_else(IvyError::internal)?
            + 1.0;
        // JavaScript Number addition and String rendering are part of the key
        // identity. The validated store enforces the admitted sequence range.
        let number = try_js_string(&Json::Number(next))?.to_utf8()?;
        let advanced = store
            .write(
                SEQUENCE,
                &Json::object([
                    ("schemaVersion", Json::Number(1.0)),
                    ("nextValue", Json::Number(next)),
                    ("updatedAt", now().into()),
                ]),
                &mutation(operation_id, &format!("allocate-task-key:{number}"))?,
                &Json::object([
                    ("objectId", sequence.pin.required("objectId")?.clone()),
                    (
                        "expectedRevision",
                        sequence.pin.required("revision")?.clone(),
                    ),
                ]),
            )
            .await;
        match advanced {
            Ok(_) => {}
            Err(error) if conflict(&error) => continue,
            Err(error) => return Err(error),
        }
        let task_key = format!("TASK-{number:0>4}");
        let value = Json::object([
            ("schemaVersion", Json::Number(1.0)),
            ("operationId", Json::String(operation_id.clone())),
            ("taskKey", task_key.clone().into()),
            ("createdAt", now().into()),
        ]);
        match store
            .write(
                ALLOCATION,
                &value,
                &mutation(operation_id, "retain-task-key")?,
                &create(store, &name),
            )
            .await
        {
            Ok(_) => return Ok(task_key.into()),
            Err(error) if conflict(&error) => {}
            Err(error) => return Err(error),
        }
        let message =
            "The competing Task key allocation is unavailable or belongs to another operation.";
        let winner = store
            .named(ALLOCATION, &name, None)
            .await?
            .ok_or_else(|| IvyError::new("task_board_identity_conflict", message))?;
        return retained_key(&winner.value, operation_id, message);
    }
}

fn create<S: Store>(store: &S, name: &str) -> Json {
    Json::object([(
        "create",
        Json::object([
            (
                "parentId",
                store
                    .root_object_id()
                    .map(Json::String)
                    .unwrap_or(Json::Null),
            ),
            ("name", name.into()),
        ]),
    )])
}
