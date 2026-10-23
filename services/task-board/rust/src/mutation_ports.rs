//! JSON mutation requests and native failure bindings.
#![cfg_attr(not(test), allow(dead_code))]
use super::persistent_reads::json_values::Value;
use crate::engine_read::read_failure::{ReadCause, ReadFailure};
use ivy_contracts::{IvyError, JsString};
use ivy_sdk::rpc::{OptionalRpcClient, RequestOptions};

pub(crate) type WriteFailure<E> = ReadFailure<E>;

#[allow(async_fn_in_trait)]
pub(crate) trait LocalMutationSource<E> {
    async fn request_owned(
        &self,
        method: &str,
        params: Option<Value>,
    ) -> Result<Value, WriteFailure<E>>;
}

/// Forward JSON parameters and retain the transport’s typed error.
pub(crate) struct RpcMutationSource<'a, R: ?Sized>(pub(crate) &'a R);
impl<R: OptionalRpcClient + ?Sized, E> LocalMutationSource<E> for RpcMutationSource<'_, R> {
    async fn request_owned(
        &self,
        method: &str,
        params: Option<Value>,
    ) -> Result<Value, WriteFailure<E>> {
        let result = self
            .0
            .request_optional(method, params, RequestOptions::default())
            .await?;
        Ok(result)
    }
}

#[allow(async_fn_in_trait)]
pub(crate) trait LocalMutationStore<E> {
    fn root_object_id_graph(&self) -> Value;
    fn contract_version_graph(&self, key: &str) -> Result<Value, WriteFailure<E>>;
    fn retired_operation_graph(&self, caller: &Value, id: &Value) -> Result<bool, WriteFailure<E>>;
    fn local_key_graph(&self, id: &Value) -> Result<Option<JsString>, WriteFailure<E>>;
    fn complete_local_workflow_graph(&self, id: &Value, at: &Value) -> Result<(), WriteFailure<E>>;
    async fn named_graph(
        &self,
        key: &str,
        name: &Value,
        parent: Option<&Value>,
    ) -> Result<Option<Value>, WriteFailure<E>>;
    async fn read_graph(
        &self,
        key: &str,
        address: &Value,
        primary: bool,
        include_archived: bool,
        retained: bool,
    ) -> Result<Value, WriteFailure<E>>;
    async fn write_graph(
        &self,
        key: &str,
        value: &Value,
        mutation: &Value,
        destination: &Value,
    ) -> Result<Value, WriteFailure<E>>;
    async fn prepared_mutation_id_graph(&self, mutation: &Value) -> Result<Value, WriteFailure<E>>;
    async fn write_prepared_graph(
        &self,
        key: &str,
        value: &Value,
        mutation: &Value,
        destination: &Value,
        selected_version: &Value,
    ) -> Result<Value, WriteFailure<E>>;
}

#[allow(async_fn_in_trait)]
pub(crate) trait LocalMutationProgram<E> {
    async fn run_graph(&self, operation: Value) -> Result<Value, WriteFailure<E>>;
}

pub(crate) fn typed_write_cause<E>(cause: &WriteFailure<E>) -> Option<&IvyError> {
    match cause.cause() {
        ReadCause::Ivy(error) => Some(error),
        ReadCause::Source(_) => None,
    }
}

/// Corresponds to IvyError.from at failedPublication entry, before its saved
/// operation read. Earlier recovery sites use only typed_write_cause.
pub(crate) fn publication_write_cause<E>(cause: &WriteFailure<E>) -> IvyError {
    match typed_write_cause(cause) {
        Some(error) => error.clone(),
        None => IvyError::internal(),
    }
}
