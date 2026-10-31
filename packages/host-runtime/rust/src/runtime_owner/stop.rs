use crate::process::{OwnedProcess, ProcessResult};
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString, Outcome};
use std::{future::Future, sync::Arc, time::Duration};

/// AgentManager owns asynchronous native turns after their accepting RPC ends.
/// Its process must establish completion before the host can replace it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StopPolicy {
    Cooperative,
    Deadline(Duration),
}
impl StopPolicy {
    pub fn for_manifest(manifest: &IvyJson) -> Self {
        if manifest
            .get("componentId")
            .and_then(IvyJson::as_js_string)
            .is_some_and(|id| id.eq_str("agent-manager"))
        {
            Self::Cooperative
        } else {
            Self::Deadline(Duration::from_millis(
                manifest
                    .get("shutdown")
                    .and_then(|value| value.get("timeoutMs"))
                    .and_then(IvyJson::as_f64)
                    .unwrap_or(15_000.0) as u64,
            ))
        }
    }
    pub fn timeout(self) -> Option<Duration> {
        match self {
            Self::Cooperative => None,
            Self::Deadline(timeout) => Some(timeout),
        }
    }
}

/// Health must already have passed the shared Host.Health schema validator.
/// Never write a shutdown request for another instance, launch or absent boot.
pub fn shutdown_control(
    health: &IvyJson,
    instance_id: &JsString,
    launch_id: &JsString,
    requested_at: &str,
) -> Option<IvyJson> {
    if health.get("instanceId")?.as_js_string()? != instance_id
        || health.get("launchId")?.as_js_string()? != launch_id
    {
        return None;
    }
    let boot_id = health.get("bootId")?.as_js_string()?;
    if boot_id.0.is_empty() {
        return None;
    }
    Some(IvyJson::Object(vec![
        ("schemaVersion".into(), IvyJson::Number(1.0)),
        ("instanceId".into(), IvyJson::String(instance_id.clone())),
        ("launchId".into(), IvyJson::String(launch_id.clone())),
        ("bootId".into(), IvyJson::String(boot_id.clone())),
        ("action".into(), "shutdown".into()),
        ("requestedAt".into(), requested_at.into()),
    ]))
}

/// Deadline services share one grace budget for the control attempt and exit.
/// Cooperative owners retain their process until it establishes completion.
/// The owner durably fences replacements on any returned unknown outcome.
pub async fn stop_child(
    child: &OwnedProcess,
    policy: StopPolicy,
    request_shutdown: impl Future<Output = IvyResult<()>>,
) -> IvyResult<Arc<ProcessResult>> {
    if policy
        .timeout()
        .is_some_and(|grace| grace > Duration::from_secs(600))
    {
        return Err(IvyError::new(
            "invalid_arguments",
            "Cooperative shutdown grace must be finite and at most ten minutes.",
        ));
    }
    let completion = child.completion.clone();
    let cooperative = async {
        // An already completed process must not receive a late control request.
        if let Some(result) = completion.current() {
            return result;
        }
        let _ = request_shutdown.await;
        completion.wait().await
    };
    let result = match policy.timeout() {
        None => cooperative.await,
        Some(grace) => match tokio::time::timeout(grace, cooperative).await {
            Ok(result) => result,
            Err(_) => child.stop(Duration::ZERO).await?,
        },
    };
    if result.error_code.as_deref() == Some("outcome_unknown") {
        return Err(IvyError::new(
            "outcome_unknown",
            "The owned process tree did not establish a stopped outcome.",
        )
        .with_outcome(Outcome::Unknown));
    }
    Ok(result)
}
