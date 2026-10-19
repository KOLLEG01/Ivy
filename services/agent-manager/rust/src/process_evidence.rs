//! Original native stop records. A completed process alone is never tree proof.
use crate::{native_connection::NativeStopEvidence, native_handshake::NativeMode};
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString, json::parse};
use ivy_host_runtime::{
    config::atomic_json,
    process::{OwnedProcess, ProcessResult, owned_process_tree_stopped},
    process_fence::record_process_stop_fence,
};
use ivy_sdk::clock::parse_date;
use std::{fs, path::PathBuf, sync::Arc};

fn stop_date(value: &JsString) -> Option<i64> {
    let millis = parse_date(value);
    millis.is_finite().then_some(millis as i64)
}
pub trait StopClock: Send + Sync {
    fn observed_at(&self) -> JsString;
    fn millis(&self) -> i64;
}
struct SystemStopClock;
impl StopClock for SystemStopClock {
    fn observed_at(&self) -> JsString {
        chrono::Utc::now()
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
            .into()
    }
    fn millis(&self) -> i64 {
        chrono::Utc::now().timestamp_millis()
    }
}

#[derive(Clone)]
pub struct OwnedProcessEvidence {
    root: PathBuf,
    clock: Arc<dyn StopClock>,
}
impl OwnedProcessEvidence {
    pub fn new(root: PathBuf) -> Self {
        Self::with_clock(root, Arc::new(SystemStopClock))
    }
    pub fn with_clock(root: PathBuf, clock: Arc<dyn StopClock>) -> Self {
        Self { root, clock }
    }
    pub async fn stops(&self) -> IvyResult<Vec<IvyJson>> {
        let path = self.root.join("owned-process-stops.json");
        // This is the original stat/read guard, including ENOENT on either
        // operation. Unlike ownership files, this reader follows existing links.
        let saved = tokio::task::spawn_blocking(move || {
            let metadata = match fs::metadata(&path) {
                Ok(value) => value,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    return Ok(IvyJson::Array(Vec::new()));
                }
                Err(_) => return Err(IvyError::internal()),
            };
            if !metadata.is_file() || metadata.len() > 64 * 1024 {
                return Err(IvyError::new(
                    "invalid_arguments",
                    "Configuration must be a bounded regular file.",
                ));
            }
            match fs::read(&path) {
                Ok(bytes) => parse(&String::from_utf8_lossy(&bytes), usize::MAX)
                    .map_err(|_| IvyError::internal()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    Ok(IvyJson::Array(Vec::new()))
                }
                Err(_) => Err(IvyError::internal()),
            }
        })
        .await
        .map_err(|_| IvyError::internal())??;
        let invalid = || {
            IvyError::new(
                "native_stop_evidence_invalid",
                "Stored native process stop evidence is invalid.",
            )
        };
        let rows = saved
            .as_array()
            .filter(|rows| rows.len() <= 256)
            .ok_or_else(invalid)?;
        for row in rows {
            let valid = row.as_object().is_some_and(|fields| fields.len() == 2)
                && row
                    .get("epoch")
                    .and_then(IvyJson::as_js_string)
                    .is_some_and(|value| !value.0.is_empty() && value.0.len() <= 256)
                && row
                    .get("observedAt")
                    .and_then(IvyJson::as_js_string)
                    .is_some_and(|value| stop_date(value).is_some());
            if !valid {
                return Err(invalid());
            }
        }
        Ok(rows.to_vec())
    }
    pub async fn retain(&self, epoch: JsString) -> IvyResult<()> {
        let now = self.clock.observed_at();
        let previous = self.stops().await?;
        let mut retained = Vec::new();
        for row in previous {
            if row.get("epoch").and_then(IvyJson::as_js_string) == Some(&epoch) {
                continue;
            }
            let time = stop_date(
                row.required("observedAt")?
                    .as_js_string()
                    .ok_or_else(IvyError::internal)?,
            );
            let cutoff = self.clock.millis() as i128 - 14 * 24 * 60 * 60 * 1000;
            if time.is_some_and(|time| time as i128 > cutoff) {
                retained.push(row);
            }
        }
        retained.push(IvyJson::object([
            ("epoch", IvyJson::String(epoch)),
            ("observedAt", IvyJson::String(now)),
        ]));
        if retained.len() > 256 {
            retained.drain(..retained.len() - 256);
        }
        atomic_json(
            self.root.join("owned-process-stops.json"),
            IvyJson::Array(retained),
        )
        .await
    }
    pub async fn record(
        &self,
        epoch: JsString,
        process: &OwnedProcess,
        mode: NativeMode,
        result: Arc<ProcessResult>,
    ) -> IvyResult<()> {
        if result.error_code.as_deref() == Some("outcome_unknown") {
            record_process_stop_fence(
                self.root.clone(),
                IvyJson::object([
                    ("schemaVersion", 1.0.into()),
                    ("owner", "native".into()),
                    ("ownerId", IvyJson::String(epoch)),
                    ("processId", (process.pid as f64).into()),
                    ("observedAt", IvyJson::String(self.clock.observed_at())),
                    ("code", "outcome_unknown".into()),
                ]),
            )
            .await?;
        } else if mode != NativeMode::ExternalProxy && owned_process_tree_stopped(process).await? {
            self.retain(epoch).await?;
        }
        Ok(())
    }
    pub fn callback(self) -> Arc<NativeStopEvidence> {
        Arc::new(move |epoch, process, mode, result| {
            let evidence = self.clone();
            Box::pin(async move { evidence.record(epoch, process, mode, result).await })
        })
    }
}
