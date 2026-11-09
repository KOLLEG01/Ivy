//! Adapt the public store to the mutation policy’s native error type.
use super::{Document, Store};
use crate::{
    engine_read::read_failure::ReadCause,
    journal_store::{
        mutation_ports::{LocalMutationStore, WriteFailure},
        persistent_reads::json_values::Value,
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
fn string(value: &Value) -> Result<JsString, WriteFailure<()>> {
    match value {
        Value::String(value) => Ok(value.clone()),
        _ => Err(IvyError::new("invalid_arguments", "Expected a string.").into()),
    }
}
pub(crate) fn json_result(result: Result<Value, WriteFailure<()>>) -> IvyResult<Json> {
    result.map_err(failure)
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
    ) -> Result<Option<Document>, WriteFailure<()>> {
        let parent = parent
            .filter(|value| !matches!(value, Value::Null))
            .map(string)
            .transpose()?;
        Ok(self
            .0
            .named(key, &string(name)?.to_utf8()?, parent.as_ref())
            .await?)
    }
    async fn read_graph(
        &self,
        key: &str,
        address: &Value,
        primary: bool,
        _: bool,
        retained: bool,
    ) -> Result<Document, WriteFailure<()>> {
        Ok(if retained {
            self.0.read_prepared(key, address).await?
        } else {
            self.0.read(key, address, primary).await?
        })
    }
    async fn write_graph(
        &self,
        key: &str,
        value: &Value,
        mutation: &Value,
        destination: &Value,
    ) -> Result<Value, WriteFailure<()>> {
        Ok(self
            .0
            .write(key, value, &string(mutation)?.to_utf8()?, destination)
            .await?)
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
    async fn prepare_payload_graph(
        &self,
        key: &str,
        value: Value,
        destination: &Value,
    ) -> Result<super::PreparedPayload, WriteFailure<()>> {
        Ok(self.0.prepare_payload(key, value, destination).await?)
    }
    async fn write_prepared_graph(
        &self,
        key: &str,
        value: &Value,
        mutation: &Value,
        destination: &Value,
        version: &Value,
        references: Option<&Value>,
    ) -> Result<Value, WriteFailure<()>> {
        let version = if version.is_null() {
            None
        } else {
            Some(string(version)?)
        };
        Ok(self
            .0
            .write_prepared(
                key,
                value,
                &string(mutation)?,
                destination,
                version.as_ref(),
                references,
            )
            .await?)
    }
}
