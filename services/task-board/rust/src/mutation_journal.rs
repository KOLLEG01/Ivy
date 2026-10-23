//! The raw operation Store on the existing read/write and SQLite owners.
//! All transport, parser and native causes retain their original payload.
#![cfg_attr(not(test), allow(dead_code))]
use super::{
    document_reads::{DocumentReads, TreeReads, sql_argument},
    document_writes::{DocumentWrites, named_graph},
    json_values::{self as v, Value},
    store::ObjectReads,
};
use crate::journal_store::{
    Contracts,
    mutation_ports::{LocalMutationSource, LocalMutationStore, WriteFailure},
};
use ivy_contracts::{IvyError, IvyJson as Json, JsString};
use std::future::Future;

pub(crate) struct MutationJournal<'a, D, W> {
    pub(crate) documents: &'a D,
    pub(crate) writes: &'a W,
}

impl<D, S, H, W, I, N, G> MutationJournal<'_, D, DocumentWrites<'_, S, H, W, I, N, G>> {
    pub(crate) async fn allocate_task_key_graph<E>(
        &self,
        operation_id: &Value,
    ) -> Result<Value, WriteFailure<E>>
    where
        Self: LocalMutationStore<E>,
        N: Fn() -> String,
    {
        use crate::task_keys::local::{self, Calls};
        local::allocate_graph(
            &Calls {
                store: self,
                now: || Value::String((self.writes.now)().into()),
                increment: |value| Ok(local::increment(&value)?),
                retained: local::retained,
            },
            operation_id,
        )
        .await
    }
}

fn completed_at(value: &Value) -> Result<JsString, IvyError> {
    let millis = match value {
        Value::String(text) => ivy_sdk::clock::parse_date(text),
        Value::Number(millis) => *millis,
        _ => return Err(v::invalid("Completion time must be a timestamp.")),
    };
    ivy_sdk::clock::date_to_iso(millis).map_err(|_| v::invalid("Invalid completion time."))
}

impl<R, C, Parse, SqlFailure, M, S, H, W, I, N, G, E, HFut, WFut, IFut> LocalMutationStore<E>
    for MutationJournal<
        '_,
        DocumentReads<'_, R, C, Parse, SqlFailure, M>,
        DocumentWrites<'_, S, H, W, I, N, G>,
    >
where
    R: ObjectReads<WriteFailure<E>>,
    C: Contracts,
    M: TreeReads<WriteFailure<E>>,
    S: LocalMutationSource<E>,
    Parse: Fn(&str) -> Result<Json, WriteFailure<E>>,
    SqlFailure: Fn(ivy_store::rusqlite::Error) -> WriteFailure<E>,
    H: Fn(Value, Value) -> HFut,
    HFut: Future<Output = Result<Value, WriteFailure<E>>>,
    W: Fn(Value, Value, Value, Value) -> WFut,
    WFut: Future<Output = Result<Value, WriteFailure<E>>>,
    I: Fn(Value) -> IFut,
    IFut: Future<Output = Result<Value, WriteFailure<E>>>,
    N: Fn() -> String,
    G: Fn() -> JsString,
{
    fn root_object_id_graph(&self) -> Value {
        self.documents
            .local
            .journal
            .root_object_id
            .clone()
            .map(Value::String)
            .unwrap_or(Value::Null)
    }
    fn contract_version_graph(&self, key: &str) -> Result<Value, WriteFailure<E>> {
        Ok(Value::String(
            self.documents.local.contracts.version(key)?.into(),
        ))
    }
    fn retired_operation_graph(&self, caller: &Value, id: &Value) -> Result<bool, WriteFailure<E>> {
        let caller = sql_argument(caller)?;
        let id = sql_argument(id)?;
        let row = self.documents.local.row::<WriteFailure<E>>(
            "SELECT request_hash FROM retired_operations WHERE caller_id=? AND operation_id=?",
            &[caller.as_ref(), id.as_ref()],
        )?;
        // The original returns String(request_hash) or null. Operations tests
        // its truthiness; an existing empty hash is therefore still false.
        Ok(row
            .map(|row| row[0].string())
            .transpose()?
            .is_some_and(|id| !id.is_empty()))
    }
    fn local_key_graph(&self, id: &Value) -> Result<Option<JsString>, WriteFailure<E>> {
        self.documents
            .local
            .key::<WriteFailure<E>>(sql_argument(id)?.as_ref())
            .map(|value| value.map(Into::into))
    }
    fn complete_local_workflow_graph(&self, id: &Value, at: &Value) -> Result<(), WriteFailure<E>> {
        let at = if matches!(at, Value::Null) {
            Value::String((self.writes.now)().into())
        } else {
            at.clone()
        };
        // Date conversion precedes SQL preparation and binding, including when
        // the root or its descendants cannot be passed to SQLite.
        let at = Json::String(completed_at(&at)?);
        self.documents.local.execute::<WriteFailure<E>>(
            "INSERT INTO cleanup_roots VALUES(?,?) ON CONFLICT(root_id) DO UPDATE SET delete_after=excluded.delete_after",
            &[sql_argument(id)?.as_ref(), Some(&at)],
        )
    }
    async fn named_graph(
        &self,
        key: &str,
        name: &Value,
        parent: Option<&Value>,
    ) -> Result<Option<Value>, WriteFailure<E>> {
        named_graph(self.documents, self.writes.source, key, name, parent).await
    }
    async fn read_graph(
        &self,
        key: &str,
        address: &Value,
        primary: bool,
        include_archived: bool,
        retained: bool,
    ) -> Result<Value, WriteFailure<E>> {
        self.documents
            .read_graph(key, address, primary, include_archived, retained)
            .await
    }
    async fn write_graph(
        &self,
        key: &str,
        value: &Value,
        mutation: &Value,
        destination: &Value,
    ) -> Result<Value, WriteFailure<E>> {
        self.writes
            .write_graph(
                self.documents,
                key,
                value,
                mutation,
                destination,
                false,
                &Value::Null,
            )
            .await
    }
    async fn prepared_mutation_id_graph(&self, mutation: &Value) -> Result<Value, WriteFailure<E>> {
        (self.writes.identity)(mutation.clone()).await
    }
    async fn write_prepared_graph(
        &self,
        key: &str,
        value: &Value,
        mutation: &Value,
        destination: &Value,
        selected_version: &Value,
    ) -> Result<Value, WriteFailure<E>> {
        self.writes
            .write_graph(
                self.documents,
                key,
                value,
                mutation,
                destination,
                true,
                selected_version,
            )
            .await
    }
}
