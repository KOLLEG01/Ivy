//! Current service-owned persistence reads and explicit Shared bindings.
use super::JournalStore;
use crate::engine_read::condition_policy::ConditionStore;
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString, json::parse, limits::MANAGEMENT_FRAME_BYTES,
};
use ivy_sdk::rpc::{RequestOptions, RpcClient};
use ivy_store::rusqlite::Connection;

#[path = "legacy_reads.rs"]
mod legacy_reads;
#[path = "local_reads.rs"]
pub mod local_reads;
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

enum LegacyFailure {
    Ivy(IvyError),
    JavaScript(ivy_sdk::task_comments::CommentException),
}
impl From<IvyError> for LegacyFailure {
    fn from(error: IvyError) -> Self {
        Self::Ivy(error)
    }
}
impl From<ivy_sdk::task_comments::CommentException> for LegacyFailure {
    fn from(error: ivy_sdk::task_comments::CommentException) -> Self {
        Self::JavaScript(error)
    }
}

fn truthy(value: &Json) -> bool {
    match value {
        Json::Null | Json::Bool(false) => false,
        Json::Number(n) => *n != 0.0 && !n.is_nan(),
        Json::String(s) => !s.0.is_empty(),
        _ => true,
    }
}
fn spread(value: &Json) -> Json {
    match value {
        Json::Object(_) => value.clone(),
        Json::Array(items) => Json::object(
            items
                .iter()
                .enumerate()
                .map(|(i, v)| (i.to_string(), v.clone())),
        ),
        Json::String(s) => Json::object(
            s.0.iter()
                .enumerate()
                .map(|(i, unit)| (i.to_string(), Json::String(JsString(vec![*unit])))),
        ),
        _ => Json::Object(vec![]),
    }
}
fn map_key_equal(a: Option<&Json>, b: Option<&Json>) -> bool {
    match (a, b) {
        (None, None) | (Some(Json::Null), Some(Json::Null)) => true,
        (Some(Json::String(a)), Some(Json::String(b))) => a == b,
        (Some(Json::Bool(a)), Some(Json::Bool(b))) => a == b,
        (Some(Json::Number(a)), Some(Json::Number(b))) => a == b || a.is_nan() && b.is_nan(),
        (Some(a @ (Json::Object(_) | Json::Array(_))), Some(b)) => std::ptr::eq(a, b),
        _ => false,
    }
}

/// Query the current retained Operation revision without using an action cache.
/// Native SQLite/JSON access failures use the original non-Ivy error boundary.
pub fn failed_handoff(connection: &Connection, run_id: &JsString) -> IvyResult<Option<Json>> {
    local_reads::failed_handoff_with(
        connection,
        run_id,
        &|raw| {
            parse(raw, MANAGEMENT_FRAME_BYTES).map_err(|_| LegacyFailure::Ivy(IvyError::internal()))
        },
        &|_| LegacyFailure::Ivy(IvyError::internal()),
    )
    .map_err(|failure| match failure {
        LegacyFailure::Ivy(error) => error,
        LegacyFailure::JavaScript(error) => {
            let _ = error;
            IvyError::internal()
        }
    })
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
            .request("objects.read", params.unwrap(), RequestOptions::default())
            .await
            .map_err(LegacyFailure::Ivy)
    })
    .await
    .map_err(|failure| match failure {
        LegacyFailure::Ivy(error) => error,
        LegacyFailure::JavaScript(error) => {
            let _ = error;
            IvyError::internal()
        }
    })
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
    .map_err(|error| match error {
        record_upgrades::RecordFailure::Ivy(error) => error,
        record_upgrades::RecordFailure::JavaScript(_) => IvyError::internal(),
    })
}
