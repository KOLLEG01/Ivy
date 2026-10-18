//! Explicit conversion of retained assignment delivery records. The caller runs
//! this against stopped import copies before admitting their runtime workers.
use crate::{inbox_evidence::InboxEvidence, journal::Document, store::Record};
use ivy_contracts::{IvyError, IvyJson, IvyResult, hash::hash_json};

pub struct DeliveryMapping {
    pub source_key: &'static str,
    pub target_field: &'static str,
    /// Includes the original pin/name/value. Its stored revisions remain intact.
    pub source: Document,
    pub applied: bool,
}
pub struct ImportedExecution {
    pub before: IvyJson,
    pub after: IvyJson,
    pub mappings: Vec<DeliveryMapping>,
}
fn mismatch(voice: bool) -> IvyError {
    if voice {
        IvyError::new(
            "secretary_voice_mismatch",
            "The original Voice request changed.",
        )
    } else {
        IvyError::new(
            "secretary_assignment_result_changed",
            "The retained decision changed after delivery began.",
        )
    }
}

/// Copy original Main/Voice admission into the current execution value. No
/// provider client is accepted: this performs owner/object reads and one CAS
/// amendment, never a notification, call, native task or operation replay.
/// Current execution fields win; shadowed source records and all their history
/// are retained. A repeated explicit import reads the current value and makes
/// no further amendment, including after an uncertain original write applied.
pub async fn import_execution_delivery(
    evidence: &InboxEvidence<'_, '_>,
    execution: &IvyJson,
) -> IvyResult<ImportedExecution> {
    evidence.verify_owner().await?;
    let store = evidence.store;
    let before = store
        .read(
            Record::Execution,
            execution.required("pin")?.required("objectId")?,
        )
        .await?;
    let value = before.required("value")?;
    if value.get("executionId") != execution.required("value")?.get("executionId") {
        return Err(IvyError::new(
            "secretary_assignment_identity_conflict",
            "The retained execution must keep its original identity.",
        ));
    }
    let name = value.required("executionId")?.as_str()?;
    let mut next = value.clone();
    let mut mappings = vec![];
    for (key, field) in [
        ("secretary/assignment-handoff", "delivery"),
        ("secretary/assignment-voice", "voice"),
    ] {
        let Some(source) = store.journal.named(key, &name, true)? else {
            continue;
        };
        let applied = next.get(field).is_none_or(IvyJson::is_null);
        if applied {
            let voice = field == "voice";
            if source.value.get("executionId") != value.get("executionId")
                || source.value.get("resultHash")
                    != Some(&hash_json(value.required("result")?)?.into())
            {
                return Err(mismatch(voice));
            }
            if voice {
                let pin = before.required("pin")?;
                let original = source.value.required("source")?;
                let revision = original.get("revision").and_then(IvyJson::as_f64);
                if original.get("objectId") != pin.get("objectId")
                    || revision.is_none_or(|revision| {
                        !revision.is_finite()
                            || revision.fract() != 0.0
                            || revision < 1.0
                            || revision
                                > pin.get("revision").and_then(IvyJson::as_f64).unwrap_or(0.0)
                    })
                {
                    return Err(mismatch(true));
                }
            }
            next.insert(field, source.value.clone());
        }
        mappings.push(DeliveryMapping {
            source_key: key,
            target_field: field,
            source,
            applied,
        });
    }
    // The Store validates the combined execution at its write boundary. An
    // invalid later mapping cannot leave an earlier mapping half imported.
    let after = store.amend(&before, Record::Execution, &next).await?;
    Ok(ImportedExecution {
        before,
        after,
        mappings,
    })
}
