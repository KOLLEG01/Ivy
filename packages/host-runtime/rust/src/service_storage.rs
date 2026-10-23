//! Read-only compatibility admission for Ivy-owned service journals.
use crate::{IvyError, IvyJson, IvyResult, layout, node_path};
use ivy_contracts::JsString;
use ivy_store::{rusqlite, sqlite};
use std::time::Duration;

fn internal<T, E>(value: Result<T, E>) -> IvyResult<T> {
    value.map_err(|_| IvyError::internal())
}
fn format(storage: &IvyJson, key: &str) -> IvyResult<f64> {
    storage
        .get(key)
        .and_then(IvyJson::as_f64)
        .filter(|value| {
            value.is_finite()
                && value.fract() == 0.0
                && (0.0..=9_007_199_254_740_991.0).contains(value)
        })
        .ok_or_else(|| {
            IvyError::new(
                "incompatible_storage",
                "Service journal requires a numeric storage contract.",
            )
        })
}
fn busy(error: &rusqlite::Error) -> bool {
    matches!(error,rusqlite::Error::SqliteFailure(error,_) if matches!(error.extended_code&255,5|6))
}

/// Only an actual SQLITE_BUSY/LOCKED read may use the optional running format.
/// The executor supplies it only after generation-bound ready evidence.
/// Opening, corruption and other errors never authorize that fallback.
pub async fn verify_service_storage(
    config: IvyJson,
    instance_id: JsString,
    plan: IvyJson,
    verified_running_format: Option<f64>,
) -> IvyResult<()> {
    internal(tokio::task::spawn_blocking(move || {
        if plan.required("componentId")?.as_str()? != "phone-bridge" { return Ok(()); }
        let storage = plan.get("storage").filter(|value| value.as_object().is_some())
            .ok_or_else(|| IvyError::new("incompatible_storage", "Service journal requires an explicit storage contract."))?;
        let (minimum, maximum, write) = (format(storage,"minReadableFormat")?, format(storage,"maxReadableFormat")?, format(storage,"writeFormat")?);
        if minimum > maximum || write < minimum || write > maximum {
            return Err(IvyError::new("incompatible_storage", "Service journal storage contract is inconsistent."));
        }
        let paths = layout::service_paths(&config, &IvyJson::String(instance_id))?;
        let path = node_path::to_native(paths.required("data")?.as_js_string().ok_or_else(IvyError::internal)?)
            .join("phone-commands.sqlite");
        match std::fs::symlink_metadata(&path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(_) => return Err(IvyError::internal()),
            Ok(_) => {}
        }
        let db = internal(sqlite::open(&path, sqlite::OpenOptions { read_only: true, busy_timeout: Duration::from_millis(100) }))?;
        let actual = match db.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0)) {
            Ok(value) => value as f64,
            Err(error) if busy(&error) && verified_running_format.is_some() => verified_running_format.unwrap(),
            Err(_) => return Err(IvyError::internal()),
        };
        if actual == 0.0 || actual >= minimum && actual <= maximum && write == actual { return Ok(()); }
        Err(IvyError::new("incompatible_storage", format!(
            "phone-bridge storage format {actual} cannot be used by candidate format {write}. Update refused before activation; retained data requires an explicit development decision."
        )))
    }).await)?
}
