//! Current service-owned persistence reads and explicit Shared bindings.
use super::JournalStore;
use crate::engine_read::condition_policy::ConditionStore;
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString, json::parse, limits::MANAGEMENT_FRAME_BYTES,
};
use ivy_sdk::rpc::{RequestOptions, RpcClient};
use ivy_store::rusqlite::Connection;

#[path = "comment_port.rs"]
pub(crate) mod comment_port;
#[path = "comment_projections.rs"]
pub(crate) mod comment_projections;
#[path = "document_reads.rs"]
pub(crate) mod document_reads;
#[path = "document_writes.rs"]
pub(crate) mod document_writes;
#[path = "json_values.rs"]
pub(crate) mod json_values;
#[path = "legacy_reads.rs"]
mod legacy_reads;
#[path = "local_history.rs"]
pub mod local_history;
#[path = "local_reads.rs"]
pub mod local_reads;
#[path = "mutation_journal.rs"]
pub(crate) mod mutation_journal;
#[path = "operation_receipt.rs"]
pub mod operation_receipt;
#[path = "read_ports.rs"]
pub mod read_ports;
#[path = "record_upgrades.rs"]
pub mod record_upgrades;
#[path = "persistent_store.rs"]
pub mod store;
#[path = "task_comments.rs"]
pub mod task_comments;
#[path = "read_values.rs"]
mod values;
#[path = "verification_sizes.rs"]
pub mod verification_sizes;

/// Read the current failed handoff operation from the local journal.
pub fn failed_handoff(connection: &Connection, run_id: &JsString) -> IvyResult<Option<Json>> {
    local_reads::failed_handoff_with(
        connection,
        run_id,
        &|raw| parse(raw, MANAGEMENT_FRAME_BYTES),
        &|_| IvyError::internal(),
    )
}
impl<R, P, C, I> ConditionStore for JournalStore<'_, R, P, C, I> {
    fn failed_handoff(&self, run_id: &JsString) -> IvyResult<Option<Json>> {
        failed_handoff(self.journal.connection, run_id)
    }
}

/// Materialize inline records or an append-only retained conversation prefix.
/// `None` selects the separate immutable-tree reader, never an empty conversation.
pub async fn read_legacy_conversation(
    client: &dyn RpcClient,
    value: &Json,
    task_id: &JsString,
) -> IvyResult<Option<Json>> {
    let task_id = Json::String(task_id.clone());
    legacy_reads::read_legacy_with(value, Some(&task_id), |params| async move {
        client
            .request(
                "objects.read",
                params.ok_or_else(IvyError::internal)?,
                RequestOptions::default(),
            )
            .await
    })
    .await
}

/// Compatibility API for existing typed callers. Lossless read ports use the
/// same core with their original optional-parameter reader and raw failures.
pub async fn fill_verification_sizes(
    client: &dyn RpcClient,
    key: &str,
    value: &Json,
) -> IvyResult<Json> {
    verification_sizes::fill_verification_sizes_with(key, value, |params| {
        record_upgrades::read_verification(client, params)
    })
    .await
}
