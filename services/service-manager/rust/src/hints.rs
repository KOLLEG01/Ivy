use ivy_contracts::{IvyError, IvyJson, IvyResult};

/// Construct the existing v1 advisory hint. The event consumer must atomically
/// persist this value before acknowledging its batch; None requires no write.
pub fn hint(
    batch: &IvyJson,
    configuration_id: &IvyJson,
    catalog_id: &IvyJson,
    observed_at: &str,
) -> IvyResult<Option<IvyJson>> {
    let Some(IvyJson::Array(items)) = batch.get("items") else {
        return Err(IvyError::new(
            "invalid_response",
            "Event batch has no items.",
        ));
    };
    let gap = !matches!(
        batch.get("gap"),
        None | Some(IvyJson::Null) | Some(IvyJson::Bool(false))
    );
    if !gap && items.is_empty() {
        return Ok(None);
    }
    let sequence = batch
        .get("throughSequence")
        .ok_or_else(|| IvyError::new("invalid_response", "Event batch has no sequence."))?;
    let contains = |id: &IvyJson| {
        items.iter().any(|item| {
            item.get("payload")
                .and_then(|payload| payload.get("objectId"))
                == Some(id)
        })
    };
    Ok(Some(IvyJson::object([
        ("schemaVersion", IvyJson::Number(1.0)),
        ("throughSequence", sequence.clone()),
        ("observedAt", observed_at.into()),
        ("configuration", (gap || contains(configuration_id)).into()),
        ("packages", (gap || contains(catalog_id)).into()),
    ])))
}
