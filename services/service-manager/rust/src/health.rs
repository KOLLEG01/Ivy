use ivy_contracts::{IvyJson, IvyResult, core_schema::validate_host};
use ivy_host_runtime::{
    config::{CONFIGURATION_BYTES, json_file},
    health::HealthFile,
};
use ivy_sdk::{clock::parse_date, service::ServiceState};
use std::{
    path::Path,
    sync::{Arc, Mutex},
};
use tokio::sync::watch;

pub fn current_executor(value: IvyJson, host_id: &IvyJson, now_ms: i64) -> Option<IvyJson> {
    validate_host("ExecutorStatus", &value).ok()?;
    if value.get("hostId") != Some(host_id) {
        return None;
    }
    let observed = parse_date(value.get("observedAt")?.as_js_string()?);
    if !observed.is_finite() || !crate::policy::executor_fresh(now_ms, observed as i64) {
        return None;
    }
    Some(value)
}

pub async fn refresh(runtime_root: &Path, host_id: &IvyJson, now_ms: i64) -> Option<IvyJson> {
    let value = json_file(runtime_root.join("executor.json"), CONFIGURATION_BYTES)
        .await
        .ok()?;
    current_executor(value, host_id, now_ms)
}

#[derive(Clone)]
struct Publication { sequence: u64, ready: bool, generation: Option<u64>, message: ivy_contracts::JsString, final_write: bool }
struct Writes {
    sender: watch::Sender<Option<Publication>>,
    completed: watch::Receiver<(u64, IvyResult<()>)>,
    sequence: u64,
    generation: Option<u64>,
    final_sequence: Option<u64>,
    sealed: bool,
}

/// Invocation order is synchronous. One state may be writing while the newest
/// pending state replaces ordinary pulses; waiters share its completion watermark.
#[derive(Clone)]
pub struct HealthReporter {
    writes: Arc<Mutex<Writes>>,
}
impl HealthReporter {
    pub fn new(health: Arc<HealthFile>) -> Self {
        Self::with_writer(move |ready, message, generation| {
            let health = health.clone();
            async move { health.write(ready, message, generation).await }
        })
    }
    fn with_writer<F, Fut>(write: F) -> Self
    where F: Fn(bool, ivy_contracts::JsString, Option<u64>) -> Fut + Send + 'static,
          Fut: std::future::Future<Output = IvyResult<()>> + Send,
    {
        let (writes, mut receiver) = watch::channel::<Option<Publication>>(None);
        let (finished, completed) = watch::channel((0, Ok(())));
        tokio::spawn(async move {
            while receiver.changed().await.is_ok() {
                let Some(publication) = receiver.borrow_and_update().clone() else { continue; };
                let result = write(publication.ready, publication.message, publication.generation).await;
                finished.send_replace((publication.sequence, result));
                if publication.final_write { break; }
            }
        });
        Self {
            writes: Arc::new(Mutex::new(Writes {
                sender: writes,
                completed,
                sequence: 0,
                generation: None,
                final_sequence: None,
                sealed: false,
            })),
        }
    }

    pub fn write(
        &self,
        ready: bool,
        generation: Option<u64>,
        message: ivy_contracts::JsString,
    ) -> impl std::future::Future<Output = IvyResult<()>> + Send + 'static {
        self.enqueue(ready, generation, message, false)
    }
    fn enqueue(
        &self,
        ready: bool,
        generation: Option<u64>,
        message: ivy_contracts::JsString,
        final_write: bool,
    ) -> impl std::future::Future<Output = IvyResult<()>> + Send + 'static {
        let mut writes = self
            .writes
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let through = (|| {
            if final_write && let Some(sequence) = writes.final_sequence { return Ok(sequence); }
            if !final_write && (writes.sealed || generation.zip(writes.generation).is_some_and(|(new, current)| new < current)) {
                return Err(ivy_contracts::IvyError::new("service_unavailable", "ServiceManager health publication is stopped or superseded."));
            }
            writes.sequence = writes.sequence.checked_add(1).ok_or_else(ivy_contracts::IvyError::internal)?;
            if final_write { writes.sealed = true; writes.final_sequence = Some(writes.sequence); }
            if let Some(generation) = generation { writes.generation = Some(generation); }
            let sequence = writes.sequence;
            writes.sender.send(Some(Publication { sequence, ready, generation, message, final_write })).map_err(|_| ivy_contracts::IvyError::internal())?;
            Ok(sequence)
        })();
        wait_for_write(writes.completed.clone(), through)
    }
    pub fn seal(&self) {
        self.writes
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .sealed = true;
    }
    pub fn flush(&self) -> impl std::future::Future<Output = IvyResult<()>> + Send + 'static {
        let writes = self
            .writes
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        wait_for_write(writes.completed.clone(), Ok(writes.sequence))
    }
    /// Seal and enqueue the final unready write under the same lock, so even a
    /// readiness callback accepted before shutdown cannot publish ready afterward.
    pub fn finalize(&self) -> impl std::future::Future<Output = IvyResult<()>> + Send + 'static {
        self.enqueue(false, None, "ServiceManager stopped.".into(), true)
    }

    pub fn state(&self, state: ServiceState) {
        if !matches!(state.status, "offline" | "stopped" | "connecting") {
            return;
        }
        let mut message = ivy_contracts::JsString::from("Hive connection is not reconciled");
        if let Some(code) = state.code.filter(|code| !code.0.is_empty()) {
            message.0.extend(": ".encode_utf16());
            message.0.extend(code.0);
        }
        message.0.push(b'.' as u16);
        // Queueing is synchronous. The callback intentionally ignores write errors.
        drop(self.write(false, state.generation, message));
    }

    pub async fn readiness(
        &self,
        executor: Option<&IvyJson>,
        host_id: IvyJson,
        service_node_id: IvyJson,
        generation: u64,
        now_ms: i64,
    ) -> IvyResult<IvyJson> {
        let ready = executor.is_some_and(|value| {
            value.get("state") == Some(&IvyJson::from("ready"))
                && current_executor(value.clone(), &host_id, now_ms).is_some()
        });
        let message = if ready {
            "Independent executor is ready."
        } else {
            "Independent executor status is missing, stale or not ready."
        };
        let now = chrono::DateTime::from_timestamp_millis(now_ms)
            .ok_or_else(ivy_contracts::IvyError::internal)?
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        self.write(ready, Some(generation), message.into()).await?;
        Ok(IvyJson::object([
            ("ready", ready.into()),
            (
                "diagnostics",
                IvyJson::Array(vec![IvyJson::object([
                    ("code", "host_executor_unavailable".into()),
                    ("resource", IvyJson::object([("hostId", host_id)])),
                    ("source", service_node_id),
                    ("severity", if ready { "info" } else { "error" }.into()),
                    ("status", if ready { "resolved" } else { "current" }.into()),
                    ("firstObservedAt", now.clone().into()),
                    ("lastObservedAt", now.into()),
                    ("message", message.into()),
                ])]),
            ),
        ]))
    }
}

fn wait_for_write(mut result: watch::Receiver<(u64, IvyResult<()>)>, through: IvyResult<u64>) -> impl std::future::Future<Output = IvyResult<()>> + Send + 'static {
    async move {
        let through = through?;
        loop {
            let (sequence, outcome) = result.borrow_and_update().clone();
            if sequence >= through { return outcome; }
            result.changed().await.map_err(|_| ivy_contracts::IvyError::internal())?;
        }
    }
}

#[cfg(test)]
#[path = "../../../../tests/rust/service-manager-health.rs"]
mod tests;
