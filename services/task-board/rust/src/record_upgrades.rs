//! The remote materialized-record upgrade boundary. Hydration precedes this
//! function; retained/header and locally prepared reads do not call it.
use super::fill_verification_sizes;
use ivy_contracts::{IvyError, IvyJson as Json, error::JavaScriptException};
use ivy_sdk::{
    rpc::RpcClient, upgrade_configuration_record, upgrade_result_record, upgrade_task_record,
};
use std::borrow::Cow;

/// Keep typed backfill/RPC failures separate from the Shared transforms' raw
/// exceptions. The eventual caller owns the corresponding original catch.
#[derive(Debug)]
pub enum RecordFailure {
    Ivy(IvyError),
    JavaScript(JavaScriptException),
}

pub async fn upgrade_stored<'a>(
    client: &dyn RpcClient,
    key: &str,
    value: &'a Json,
) -> Result<Cow<'a, Json>, RecordFailure> {
    if key == "task-board/task" {
        let stored = fill_verification_sizes(client, key, value)
            .await
            .map_err(RecordFailure::Ivy)?;
        return upgrade_task_record(&stored)
            .map(Cow::Owned)
            .map_err(RecordFailure::JavaScript);
    }
    match key {
        "task-board/configuration" => {
            upgrade_configuration_record(value).map_err(RecordFailure::JavaScript)
        }
        "task-board/result" => upgrade_result_record(value)
            .map(Cow::Owned)
            .map_err(RecordFailure::JavaScript),
        _ => Ok(Cow::Borrowed(value)),
    }
}
