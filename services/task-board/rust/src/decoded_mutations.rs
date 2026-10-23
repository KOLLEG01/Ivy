//! Adapt the public store to the mutation policy’s native error type.
use super::{Document, Store};
use crate::{
    engine_read::read_failure::ReadCause,
    journal_store::{
        mutation_ports::{LocalMutationStore, WriteFailure},
        persistent_reads::json_values::{self as v, Value},
    },
};
use ivy_contracts::{IvyError, IvyJson as Json, IvyResult, JsString};

pub(crate) struct DecodedStore<'a, S>(pub(crate) &'a S);
pub(crate) fn failure(error: WriteFailure<()>) -> IvyError {
    match error.cause() {
        ReadCause::Ivy(error) => error.clone(),
        ReadCause::Source(()) => IvyError::internal(),
    }
}
fn json(value: &Value) -> Result<Json, WriteFailure<()>> {
    v::decoded_comment(value)
}
fn string(value: &Value) -> Result<JsString, WriteFailure<()>> {
    match value {
        Value::String(value) => Ok(value.clone()),
        _ => Err(IvyError::new("invalid_arguments", "Expected a string.").into()),
    }
}
pub(crate) fn json_result(result: Result<Value, WriteFailure<()>>) -> IvyResult<Json> {
    result.and_then(|value| json(&value)).map_err(failure)
}
pub(crate) fn record(document: Document) -> Value {
    v::object([
        ("pin".into(), (&document.pin).clone()),
        ("value".into(), (&document.value).clone()),
        (
            "metadata".into(),
            v::object([(
                "parentId".into(),
                document.parent_id.map(Value::String).unwrap_or(Value::Null),
            )]),
        ),
        (
            "revision".into(),
            v::object([(
                "contentHash".into(),
                Value::String(document.content_hash.into()),
            )]),
        ),
    ])
}
pub(crate) fn document(value: Value) -> IvyResult<Document> {
    let value = json_result(Ok(value))?;
    Ok(Document {
        pin: value.required("pin")?.clone(),
        value: value.required("value")?.clone(),
        parent_id: value
            .required("metadata")?
            .get("parentId")
            .and_then(Json::as_js_string)
            .cloned(),
        content_hash: value
            .required("revision")?
            .required("contentHash")?
            .as_str()?,
    })
}
pub(super) fn document_result(result: Result<Value, WriteFailure<()>>) -> IvyResult<Document> {
    document(result.map_err(failure)?)
}
pub(super) fn string_result(result: Result<Value, WriteFailure<()>>) -> IvyResult<JsString> {
    result.and_then(|value| string(&value)).map_err(failure)
}
impl<S: Store> LocalMutationStore<()> for DecodedStore<'_, S> {
    fn root_object_id_graph(&self) -> Value {
        self.0
            .root_object_id()
            .map(Value::String)
            .unwrap_or(Value::Null)
    }
    fn contract_version_graph(&self, key: &str) -> Result<Value, WriteFailure<()>> {
        Ok(Value::String(self.0.contract_version(key)?.into()))
    }
    fn retired_operation_graph(
        &self,
        caller: &Value,
        id: &Value,
    ) -> Result<bool, WriteFailure<()>> {
        Ok(self.0.retired_operation(&string(caller)?, &string(id)?)?)
    }
    fn local_key_graph(&self, id: &Value) -> Result<Option<JsString>, WriteFailure<()>> {
        Ok(self.0.local_key(&string(id)?)?.map(Into::into))
    }
    fn complete_local_workflow_graph(
        &self,
        id: &Value,
        at: &Value,
    ) -> Result<(), WriteFailure<()>> {
        Ok(self.0.complete_local_workflow(&string(id)?, &string(at)?)?)
    }
    async fn named_graph(
        &self,
        key: &str,
        name: &Value,
        parent: Option<&Value>,
    ) -> Result<Option<Value>, WriteFailure<()>> {
        let parent = parent
            .filter(|value| !matches!(value, Value::Null))
            .map(string)
            .transpose()?;
        Ok(self
            .0
            .named(key, &string(name)?.to_utf8()?, parent.as_ref())
            .await?
            .map(record))
    }
    async fn read_graph(
        &self,
        key: &str,
        address: &Value,
        primary: bool,
        _: bool,
        _: bool,
    ) -> Result<Value, WriteFailure<()>> {
        Ok(record(self.0.read(key, &json(address)?, primary).await?))
    }
    async fn write_graph(
        &self,
        key: &str,
        value: &Value,
        mutation: &Value,
        destination: &Value,
    ) -> Result<Value, WriteFailure<()>> {
        Ok((&self
            .0
            .write(
                key,
                &json(value)?,
                &string(mutation)?.to_utf8()?,
                &json(destination)?,
            )
            .await?)
            .clone())
    }
    async fn prepared_mutation_id_graph(
        &self,
        mutation: &Value,
    ) -> Result<Value, WriteFailure<()>> {
        Ok(Value::String(
            self.0
                .prepared_mutation_id(&string(mutation)?.to_utf8()?)
                .await?,
        ))
    }
    async fn write_prepared_graph(
        &self,
        key: &str,
        value: &Value,
        mutation: &Value,
        destination: &Value,
        _: &Value,
    ) -> Result<Value, WriteFailure<()>> {
        // This legacy interface has no retained-version parameter. The actual
        // raw factory store implements the explicit version slot directly.
        Ok((&self
            .0
            .write_prepared(key, &json(value)?, &string(mutation)?, &json(destination)?)
            .await?)
            .clone())
    }
}
