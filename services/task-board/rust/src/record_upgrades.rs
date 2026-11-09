//! Upgrade stored JSON through the shared record contracts.
use super::verification_sizes::{fill_verification_sizes_owned_with, fill_verification_sizes_with};
use ivy_contracts::{IvyError, IvyJson as Json, IvyResult};
use ivy_sdk::{
    rpc::{RequestOptions, RpcClient},
    upgrade_configuration_record, upgrade_result_record, upgrade_result_record_owned,
    upgrade_run_record, upgrade_run_record_owned, upgrade_task_record_owned,
};
use std::{borrow::Cow, future::Future};

pub(super) async fn read_verification(
    client: &dyn RpcClient,
    params: Option<Json>,
) -> IvyResult<Json> {
    client
        .request(
            "objects.read",
            params.ok_or_else(IvyError::internal)?,
            RequestOptions::default(),
        )
        .await
}
pub async fn upgrade_stored<'a>(
    client: &dyn RpcClient,
    key: &str,
    value: &'a Json,
) -> IvyResult<Cow<'a, Json>> {
    upgrade_stored_with(key, value, |params| read_verification(client, params)).await
}
pub async fn upgrade_stored_with<'a, E, F, Fut>(
    key: &str,
    value: &'a Json,
    read: F,
) -> Result<Cow<'a, Json>, E>
where
    E: From<IvyError>,
    F: FnMut(Option<Json>) -> Fut,
    Fut: Future<Output = Result<Json, E>>,
{
    match key {
        "task-board/task" => {
            let stored = fill_verification_sizes_with(key, value, read).await?;
            Ok(Cow::Owned(upgrade_task_record_owned(stored)?))
        }
        "task-board/run" => Ok(Cow::Owned(upgrade_run_record(value)?)),
        "task-board/configuration" => upgrade_configuration_record(value).map_err(E::from),
        "task-board/result" => Ok(Cow::Owned(upgrade_result_record(value)?)),
        _ => Ok(Cow::Borrowed(value)),
    }
}

/// Stored-document owners retain their payload buffers across current migrations.
pub async fn upgrade_stored_owned(
    client: &dyn RpcClient,
    key: &str,
    value: Json,
) -> IvyResult<Json> {
    upgrade_stored_owned_with(key, value, |params| read_verification(client, params)).await
}
pub async fn upgrade_stored_owned_with<E, F, Fut>(
    key: &str,
    value: Json,
    read: F,
) -> Result<Json, E>
where
    E: From<IvyError>,
    F: FnMut(Option<Json>) -> Fut,
    Fut: Future<Output = Result<Json, E>>,
{
    match key {
        "task-board/task" => {
            let stored = fill_verification_sizes_owned_with(key, value, read).await?;
            Ok(upgrade_task_record_owned(stored)?)
        }
        "task-board/run" => Ok(upgrade_run_record_owned(value)?),
        "task-board/result" => Ok(upgrade_result_record_owned(value)?),
        "task-board/configuration" => match upgrade_configuration_record(&value)? {
            Cow::Owned(upgraded) => Ok(upgraded),
            Cow::Borrowed(_) => Ok(value),
        },
        _ => Ok(value),
    }
}
