//! Pure Collector policy. Process ownership and RPC transport belong to shared crates.
use crate::model::{self, RecordTask, Run};
use ivy_contracts::{IvyError, IvyJson, IvyResult, json::canonical};

pub const WORKER: &str = include_str!("../assets/worker.mjs");
pub const WORKER_BYTES: usize = 1_000_000;
pub const INPUT_BYTES: usize = 262_144;
pub const MCP_BYTES: usize = 1_048_576;
pub const EVENT_BYTES: usize = 4096;

pub fn effective_retention(task: &IvyJson, maximum: &IvyJson) -> IvyResult<IvyJson> {
    Ok(IvyJson::object(
        ["maximumCount", "maximumAgeDays", "maximumBytes"]
            .map(|key| {
                Ok((
                    key,
                    IvyJson::Number(model::number(task, key)?.min(model::number(maximum, key)?)),
                ))
            })
            .into_iter()
            .collect::<IvyResult<Vec<_>>>()?,
    ))
}

pub fn result_content(run: &Run) -> IvyResult<IvyJson> {
    let output = model::field(run, "output")?;
    Ok(IvyJson::object([
        ("schemaVersion", IvyJson::Number(1.0)),
        ("taskId", model::field(run, "taskId")?.clone()),
        ("runId", model::field(run, "id")?.clone()),
        ("collectedAt", model::field(run, "finishedAt")?.clone()),
        ("data", model::field(output, "data")?.clone()),
        (
            "events",
            output
                .get("events")
                .cloned()
                .unwrap_or(IvyJson::Array(Vec::new())),
        ),
    ]))
}
pub fn event_content(run: &Run, index: usize) -> IvyResult<IvyJson> {
    let IvyJson::Array(events) = model::field(model::field(run, "output")?, "events")? else {
        return Err(invalid());
    };
    let event = events.get(index).ok_or_else(invalid)?;
    Ok(IvyJson::object([
        ("kind", "collected".into()),
        ("taskId", model::field(run, "taskId")?.clone()),
        ("runId", model::field(run, "id")?.clone()),
        ("eventIndex", IvyJson::Number(index as f64)),
        ("name", model::field(event, "name")?.clone()),
        ("payload", model::field(event, "payload")?.clone()),
    ]))
}
pub fn validate_publication(run: &Run, maximum_retention: &IvyJson) -> IvyResult<()> {
    let retention = effective_retention(
        model::field(model::field(run, "task")?, "retention")?,
        maximum_retention,
    )?;
    let bytes = canonical(
        &result_content(run)?,
        ivy_contracts::limits::MANAGEMENT_FRAME_BYTES,
    )?
    .len();
    if bytes > 1_048_576.min(model::number(&retention, "maximumBytes")? as usize) {
        return Err(IvyError::new(
            "limit_exceeded",
            "Result exceeds the task retained byte budget.",
        ));
    }
    let output = model::field(run, "output")?;
    if let Some(IvyJson::Array(events)) = output.get("events") {
        for i in 0..events.len() {
            canonical(&event_content(run, i)?, EVENT_BYTES)?;
        }
    }
    Ok(())
}

/// Update only the cursor and schedule on the latest row, preserving a concurrent
/// disable or adoption. An absent state is distinct from an explicit null state.
pub fn commit_execution(row: &mut RecordTask, run: &mut Run, now_ms: f64) -> IvyResult<bool> {
    if model::truth(run, "executionCommitted") {
        return Ok(false);
    }
    if let Some(state) = model::field(run, "output")?.get("state") {
        row.insert("state", state.clone());
    }
    row.insert(
        "nextAt",
        IvyJson::Number(
            now_ms + model::number(model::field(row, "task")?, "intervalSeconds")? * 1000.0,
        ),
    );
    run.insert("executionCommitted", true.into());
    Ok(true)
}

pub fn worker_failure(exit_code: Option<i32>) -> (&'static str, &'static str) {
    match exit_code {
        Some(79) => ("limit_exceeded", "Task result exceeds 1 MB."),
        Some(80) => (
            "authentication_required",
            "Renew the configured credentials or token.",
        ),
        Some(81) => (
            "interaction_required",
            "Complete the provider verification described in the source setup guide.",
        ),
        Some(82) => (
            "configuration_invalid",
            "Check the task settings and assigned secrets.",
        ),
        Some(83) => (
            "provider_unavailable",
            "The provider is unavailable or its response is unsupported.",
        ),
        _ => (
            "task_failed",
            "Task process failed. Check script, credentials and provider availability.",
        ),
    }
}

/// DC02: configured secret values must not appear as either values or object keys
/// in centrally stored task material. Nothing here logs or returns the secret.
pub fn contains_configured_secret(value: &IvyJson, secret: &str) -> bool {
    contains_configured_secret_json(value, &secret.into())
}
pub fn contains_configured_secret_json(value: &IvyJson, secret: &ivy_contracts::JsString) -> bool {
    let secret = &secret.0;
    if secret.is_empty() {
        return false;
    }
    fn contains(haystack: &[u16], needle: &[u16]) -> bool {
        haystack.windows(needle.len()).any(|w| w == needle)
    }
    fn visit(value: &IvyJson, secret: &[u16]) -> bool {
        match value {
            IvyJson::String(text) => contains(&text.0, secret),
            IvyJson::Object(pairs) => pairs
                .iter()
                .any(|(key, v)| contains(&key.0, secret) || visit(v, secret)),
            IvyJson::Array(items) => items.iter().any(|v| visit(v, secret)),
            _ => false,
        }
    }
    visit(value, secret)
}
fn invalid() -> IvyError {
    IvyError::new("invalid_arguments", "Invalid Collector output.")
}
