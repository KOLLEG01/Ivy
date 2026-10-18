use ivy_contracts::{IvyJson, IvyResult, core_schema::validate_host};
use ivy_host_runtime::{
    config::{CONFIGURATION_BYTES, json_file},
    health::HealthFile,
};
use ivy_sdk::{clock::parse_date, service::ServiceState};
use std::{path::Path, sync::Arc};
use tokio::sync::{mpsc, oneshot};

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

type Write = (
    bool,
    Option<u64>,
    ivy_contracts::JsString,
    oneshot::Sender<IvyResult<()>>,
);

/// Queue at callback invocation, before any future is polled. A dropped waiter
/// cannot cancel or reorder an accepted health write. Errors do not poison later writes.
#[derive(Clone)]
pub struct HealthReporter {
    writes: mpsc::UnboundedSender<Write>,
}
impl HealthReporter {
    pub fn new(health: Arc<HealthFile>) -> Self {
        let (writes, mut receiver) = mpsc::unbounded_channel::<Write>();
        tokio::spawn(async move {
            while let Some((ready, generation, message, done)) = receiver.recv().await {
                let result = health.write(ready, message, generation).await;
                let _ = done.send(result);
            }
        });
        Self { writes }
    }

    pub fn write(
        &self,
        ready: bool,
        generation: Option<u64>,
        message: ivy_contracts::JsString,
    ) -> impl std::future::Future<Output = IvyResult<()>> + Send + 'static {
        let (done, result) = oneshot::channel();
        let sent = self.writes.send((ready, generation, message, done)).is_ok();
        async move {
            if !sent {
                return Err(ivy_contracts::IvyError::internal());
            }
            result
                .await
                .map_err(|_| ivy_contracts::IvyError::internal())?
        }
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
            "Independent executor and current host reports verified."
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
