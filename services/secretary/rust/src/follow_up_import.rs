//! Explicit one-time conversion of previously admitted pending context starts.
//! Invoke only on the stopped journal import copy, before starting its workers.
use crate::{
    follow_up::{
        CONTEXT_KEY, FOLLOW_UP_KEY, SecretaryFollowUp, call_json, context_start_params,
        prepare_call,
    },
    journal::Document,
};
use ivy_contracts::{IvyError, IvyJson, IvyResult, operation::derive_operation_id_js};

pub struct ImportedFollowUpContext {
    pub before: Document,
    pub after: Document,
}
/// The original operation ID and exact legacy start arguments are retained.
/// This performs binding/ownership reads and a local conversion, never dispatch.
pub async fn import_pending_context(
    worker: &SecretaryFollowUp<'_, '_, '_>,
    name: &str,
) -> IvyResult<Option<ImportedFollowUpContext>> {
    let journal = worker.engine.evidence.store.journal;
    let Some(before) = journal.named(CONTEXT_KEY, name, true)? else {
        return Ok(None);
    };
    if before.value.get("threadId") != Some(&IvyJson::from(""))
        || before
            .value
            .get("start")
            .is_some_and(crate::inbox_evidence::truthy)
    {
        return Ok(None);
    }
    let original = journal.named(FOLLOW_UP_KEY, name, true)?.ok_or_else(|| {
        IvyError::new(
            "secretary_follow_up_context_missing",
            "The original question for the pending context is unavailable.",
        )
    })?;
    let request = original.value.required("request")?;
    if request.get("itemId") != before.value.get("itemId") {
        return Err(IvyError::new(
            "secretary_follow_up_context_missing",
            "The selected follow-up context belongs to another item.",
        ));
    }
    let operation = request
        .required("operationId")?
        .as_js_string()
        .ok_or_else(IvyError::internal)?;
    let owner = worker.owner(before.value.required("target")?)?;
    let start = prepare_call(
        &owner,
        derive_operation_id_js(operation, &"start-secretary-follow-up-context".into())?,
        "thread/start",
        context_start_params(&before.value),
    )
    .await?;
    worker.engine.evidence.verify_owner().await?;
    let mut value = before.value.clone();
    value.insert("start", call_json(&start));
    value.insert("createdFor", name.into());
    let after = journal.import_technical_revision(CONTEXT_KEY, &before.pin, &value)?;
    Ok(Some(ImportedFollowUpContext { before, after }))
}
