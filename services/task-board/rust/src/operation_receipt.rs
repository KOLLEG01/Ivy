//! Compact evidence for an existing operation identity, never replay authority.
use super::values::{field, has, insert};
use ivy_contracts::{IvyError, IvyJson as Json};

pub fn operation_receipt<E: From<IvyError>>(operation: &Json) -> Result<Json, E> {
    let mut receipt = Json::object([("receipt", true.into())]);
    for key in ["operationId", "callerPrincipalId", "requestHash"] {
        insert(&mut receipt, key, field::<E>(Some(operation), key)?);
    }
    let request = field::<E>(Some(operation), "request")?;
    let action = field::<E>(request, "action")?;
    insert(
        &mut receipt,
        "submittedHash",
        if action == Some(&Json::from("savePlan")) {
            field::<E>(request, "draftHash")?
        } else {
            field::<E>(Some(operation), "requestHash")?
        },
    );
    insert(&mut receipt, "phase", field::<E>(Some(operation), "phase")?);
    insert(&mut receipt, "action", action);
    if has::<E>(request, "taskId")? {
        insert(&mut receipt, "taskId", field::<E>(request, "taskId")?);
    } else {
        let task = field::<E>(Some(operation), "outcome")?
            .filter(|value| !value.is_null())
            .map(|outcome| field::<E>(Some(outcome), "task"))
            .transpose()?
            .flatten();
        let id = task
            .filter(|value| !value.is_null())
            .map(|task| field::<E>(Some(task), "objectId"))
            .transpose()?
            .flatten();
        receipt.insert("taskId", id.cloned().unwrap_or(Json::Null));
    }
    for key in ["updatedAt", "outcome", "error"] {
        insert(&mut receipt, key, field::<E>(Some(operation), key)?);
    }
    if let Some(run) = field::<E>(Some(operation), "handoffRun")?.filter(|value| !value.is_null()) {
        receipt.insert("handoffRun", run.clone());
    }
    Ok(receipt)
}
