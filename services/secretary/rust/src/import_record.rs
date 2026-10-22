//! Import-only reads include archived records without widening runtime admission.
use crate::store::{Record, Store};
use ivy_contracts::{IvyError, IvyJson, IvyResult, hash::hash_json};
use ivy_sdk::{operation_ids::scoped_operation_id, rpc::RequestOptions};

pub(crate) async fn read(store: &Store<'_>, key: Record, selected: &IvyJson) -> IvyResult<IvyJson> {
    let params = if matches!(selected, IvyJson::String(_)) {
        IvyJson::object([("objectId", selected.clone())])
    } else {
        selected.clone()
    };
    let mut raw = store
        .client
        .request("objects.read", params, RequestOptions::default())
        .await?;
    let mut object = raw.required("object")?.clone();
    let archived = object
        .required("effectivelyArchived")?
        .as_bool()
        .ok_or_else(IvyError::internal)?;
    // Only this one-time import bypasses the active flag. The same Store decoder
    // still validates root, key/version, exact content hash/bytes/schema and refs.
    object.insert("effectivelyArchived", false.into());
    raw.insert("object", object);
    let _ = archived;
    store.decode(key, &raw)
}
pub(crate) async fn original(
    store: &Store<'_>,
    key: Record,
    record: &IvyJson,
) -> IvyResult<IvyJson> {
    if record.required("pin")?.get("revision") == Some(&IvyJson::Number(1.0)) {
        Ok(record.clone())
    } else {
        read(
            store,
            key,
            &IvyJson::object([
                (
                    "objectId",
                    record.required("pin")?.required("objectId")?.clone(),
                ),
                ("revision", IvyJson::Number(1.0)),
            ]),
        )
        .await
    }
}
pub(crate) async fn amend(
    store: &Store<'_>,
    current: &IvyJson,
    key: Record,
    value: &IvyJson,
) -> IvyResult<IvyJson> {
    if hash_json(current.required("value")?)? == hash_json(value)? {
        return Ok(current.clone());
    }
    let pin = current.required("pin")?;
    let operation = scoped_operation_id(
        store.client.clone(),
        &IvyJson::Array(vec![
            "secretary-amend".into(),
            IvyJson::String(store.root.clone()),
            key.key().into(),
            pin.clone(),
            hash_json(value)?.into(),
        ]),
    )
    .await?;
    let result = store
        .write(
            key,
            value,
            &IvyJson::object([("object", pin.clone())]),
            &operation,
        )
        .await;
    match result {
        Ok(next) => read(store, key, &next).await,
        Err(error)
            if matches!(
                error.code.as_str(),
                "revision_conflict" | "mutation_conflict"
            ) =>
        {
            let latest = read(store, key, pin.required("objectId")?).await?;
            if hash_json(latest.required("value")?)? == hash_json(value)? {
                Ok(latest)
            } else {
                Err(error)
            }
        }
        other => other,
    }
}
