use super::*;
use ivy_contracts::{Outcome, wire_schema::validate_shared};
use ivy_sdk::rpc::RequestOptions;

fn context(generation: u64) -> InvocationContext {
    InvocationContext::detached(
        "caller".into(),
        None,
        generation,
        RequestOptions::default().cancellation,
        IvyError::new("service_unavailable", "Stopping").with_outcome(Outcome::Unknown),
    )
}

#[tokio::test(flavor = "current_thread")]
async fn fast_readiness_expires_truthfully_and_preserves_original_owner_errors() {
    let (control, _owner) = generation(7, 1000.0);
    assert_eq!(
        control.readiness(context(7)).await.unwrap_err().code,
        "service_not_ready"
    );
    control.owner_readiness(Ok(vec![]));
    let ready = control.readiness(context(7)).await.unwrap();
    validate_shared("ServiceHeartbeat", &ready).unwrap();
    assert_eq!(ready.get("ready"), Some(&true.into()));
    assert_eq!(
        control.readiness(context(8)).await.unwrap_err().code,
        "service_not_ready"
    );
    {
        let mut cached = control.readiness.lock().unwrap();
        let cached = cached.as_mut().unwrap();
        cached.observed -= READINESS_MAX_AGE + Duration::from_secs(1);
        cached.utc_millis -= 11_000.0;
    }
    let stale = control.readiness(context(7)).await.unwrap();
    validate_shared("ServiceHeartbeat", &stale).unwrap();
    assert_eq!(stale.get("ready"), Some(&false.into()));
    let original = IvyError::new("fixture_owner_changed", "Original owner verification error")
        .with_outcome(Outcome::Unknown);
    control.owner_readiness(Err(original.clone()));
    assert_eq!(control.readiness(context(7)).await.unwrap_err(), original);
    control.owner_readiness(Ok(vec![]));
    assert_eq!(
        control.readiness(context(7)).await.unwrap().get("ready"),
        Some(&true.into())
    );
    let delayed = control.readiness(context(7));
    drop(control.retire());
    assert_eq!(delayed.await.unwrap_err().code, "service_not_ready");
    assert_eq!(
        control.readiness(context(7)).await.unwrap_err().code,
        "service_not_ready"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn heartbeat_snapshot_bounds_count_and_actual_capacity_before_reply_copies() {
    let (control, _owner) = generation(7, 1000.0);
    let now = SystemClock.utc_millis() as f64;
    let diagnostics = (0..150)
        .map(|_| control_diagnostic("fixture_warning", &"x".repeat(4096), now, now).unwrap())
        .collect();
    control.owner_readiness(Ok(diagnostics));
    let ready = control.readiness(context(7)).await.unwrap();
    validate_shared("ServiceHeartbeat", &ready).unwrap();
    assert!(ready.retained_bytes() <= 128 * 1024);
    let diagnostics = ready.required("diagnostics").unwrap().as_array().unwrap();
    assert!(diagnostics.len() < 100);
    assert_eq!(
        diagnostics.last().unwrap().get("code"),
        Some(&"task_board_diagnostics_truncated".into())
    );
}
