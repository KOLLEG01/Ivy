use crate::process::{OwnedProcess, ProcessResult};
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString, Outcome};
use std::{future::Future, sync::Arc, time::Duration};

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

/// The control-file attempt and cooperative completion share one grace budget.
/// Missing/unresponsive health cannot prevent the authoritative bounded tree stop.
/// The owner must durably fence replacements on any returned unknown outcome.
pub async fn stop_child(
    child: &OwnedProcess,
    grace: Duration,
    request_shutdown: impl Future<Output = IvyResult<()>>,
) -> IvyResult<Arc<ProcessResult>> {
    if grace > Duration::from_secs(60) {
        return Err(IvyError::new(
            "invalid_arguments",
            "Process stop grace must be finite and at most one minute.",
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
    let result = match tokio::time::timeout(grace, cooperative).await {
        Ok(result) => result,
        Err(_) => child.stop(Duration::ZERO).await?,
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
