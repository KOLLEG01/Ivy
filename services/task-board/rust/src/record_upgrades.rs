//! The remote materialized-record upgrade boundary. Hydration precedes this
//! function; retained/header and locally prepared reads do not call it.
use super::verification_sizes::fill_verification_sizes_with;
use ivy_contracts::{IvyError, IvyJson as Json, error::JavaScriptException};
use ivy_sdk::{
    rpc::RpcClient, upgrade_configuration_record, upgrade_result_record, upgrade_task_record,
};
use std::borrow::Cow;
use std::future::Future;

/// Keep typed backfill/RPC failures separate from the Shared transforms' raw
/// exceptions. The eventual caller owns the corresponding original catch.
#[derive(Debug)]
pub enum RecordFailure {
    Ivy(IvyError),
    JavaScript(JavaScriptException),
}
impl From<IvyError> for RecordFailure {
    fn from(error: IvyError) -> Self {
        Self::Ivy(error)
    }
}
impl From<JavaScriptException> for RecordFailure {
    fn from(error: JavaScriptException) -> Self {
        Self::JavaScript(error)
    }
}

/// The existing SDK RpcClient accepts JSON parameters. Keep its previous
/// missing-parameter refusal at this explicit compatibility adapter; raw owner
/// ports supply their own optional-parameter reader to `upgrade_stored_with`.
pub(super) async fn read_verification(
    client: &dyn RpcClient,
    params: Option<Json>,
) -> Result<Json, RecordFailure> {
    let params = params.ok_or_else(IvyError::internal)?;
    client
        .request(
            "objects.read",
            params,
            ivy_sdk::rpc::RequestOptions::default(),
        )
        .await
        .map_err(RecordFailure::Ivy)
}

pub async fn upgrade_stored<'a>(
    client: &dyn RpcClient,
    key: &str,
    value: &'a Json,
) -> Result<Cow<'a, Json>, RecordFailure> {
    upgrade_stored_with(key, value, |params| read_verification(client, params)).await
}

pub async fn upgrade_stored_with<'a, E, F, Fut>(
    key: &str,
    value: &'a Json,
    read: F,
) -> Result<Cow<'a, Json>, E>
where
    E: From<IvyError> + From<JavaScriptException>,
    F: FnMut(Option<Json>) -> Fut,
    Fut: Future<Output = Result<Json, E>>,
{
    if key == "task-board/task" {
        let stored = fill_verification_sizes_with(key, value, read).await?;
        return upgrade_task_record(&stored)
            .map(Cow::Owned)
            .map_err(E::from);
    }
    match key {
        "task-board/configuration" => upgrade_configuration_record(value).map_err(E::from),
        "task-board/result" => upgrade_result_record(value)
            .map(Cow::Owned)
            .map_err(E::from),
        _ => Ok(Cow::Borrowed(value)),
    }
}
