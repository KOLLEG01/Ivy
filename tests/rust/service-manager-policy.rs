#[path = "../../services/service-manager/rust/src/policy.rs"]
mod manager;
#[path = "../../packages/host-runtime/rust/src/runtime_owner/policy.rs"]
mod owner;

use owner::{ExitAction, Policy};

#[test]
fn restart_policies_preserve_exit_and_uncertain_outcomes() {
    for (policy, code, error, expected) in [
        ("never", Some(1), None, ExitAction::Exited),
        ("on-failure", Some(0), None, ExitAction::Exited),
        (
            "on-failure",
            Some(0),
            Some("spawn_failed"),
            ExitAction::Restart,
        ),
        ("on-failure", None, None, ExitAction::Restart),
        ("always", Some(0), None, ExitAction::Restart),
        (
            "never",
            Some(0),
            Some("outcome_unknown"),
            ExitAction::Blocked,
        ),
    ] {
        let mut state = Policy::new(false);
        assert_eq!(state.exited(policy, code, error), expected);
        assert_eq!(state.can_launch(0), expected == ExitAction::Restart);
    }
}

#[test]
fn target_changes_and_health_cannot_clear_unknown_process_fence() {
    let mut state = Policy::new(true);
    state.select_running_target();
    state.ready(60_000);
    assert!(!state.can_launch(60_000));
    assert!(state.blocked);
}

#[test]
fn backoff_caps_and_successful_health_resets_after_thirty_seconds() {
    let mut state = Policy::new(false);
    for delay in [1_000, 2_000, 4_000, 8_000, 16_000, 32_000, 60_000, 60_000] {
        state.backoff(100, 1_000, 60_000);
        assert_eq!(state.next_restart, Some(100 + delay));
        assert!(!state.can_launch(99 + delay));
        assert!(state.can_launch(100 + delay));
    }
    state.launched(100_000);
    state.ready(129_999);
    assert_eq!(state.restart_count, 8);
    state.ready(130_000);
    assert_eq!(state.restart_count, 0);
    state.backoff(131_000, 1_000, 60_000);
    assert_eq!(state.next_restart, Some(132_000));
    for _ in 0..100 {
        state.backoff(0, 1, u64::MAX);
    }
    assert_eq!(state.next_restart, Some(65_536));
}

#[test]
fn readiness_loss_uses_first_unready_time_without_fencing_read_dependency() {
    let mut state = Policy::new(false);
    state.launched(0);
    assert!(!state.readiness_expired(500, 500));
    assert!(state.readiness_expired(501, 500));
    state.ready(600);
    assert!(!state.readiness_expired(50_000, 500));
    state.unready(60_000);
    state.unready(60_500);
    assert!(!state.readiness_expired(60_500, 500));
    assert!(state.readiness_expired(60_501, 500));
    assert!(!state.blocked);
}

#[test]
fn only_matching_direct_health_helpers_skip_job_wrapper() {
    for path in [
        "dist/packages/host-runtime/src/health.js",
        "dist/services/sample-a1/src/health.js",
    ] {
        assert!(owner::direct_health_probe("node", &[path.into()]));
    }
    for path in [
        "dist/services/../src/health.js",
        "dist/services/A/src/health.js",
        "dist/services//src/health.js",
        "dist/services/a/b/src/health.js",
        "dist/services/a/src/main.js",
    ] {
        assert!(!owner::direct_health_probe("node", &[path.into()]));
    }
    assert!(!owner::direct_health_probe(
        "other",
        &["dist/packages/host-runtime/src/health.js".into()]
    ));
    assert!(!owner::direct_health_probe(
        "node",
        &[
            "dist/packages/host-runtime/src/health.js".into(),
            "extra".into()
        ]
    ));
}

#[test]
fn stderr_codes_never_admit_messages_or_unbounded_data() {
    for code in ["process_stopped", "a", "a1_b"] {
        assert!(owner::valid_process_code(code));
    }
    for code in ["", "1bad", "UPPER", "a-b", "a\n", "a b", "é"] {
        assert!(!owner::valid_process_code(code));
    }
    assert!(owner::valid_process_code(&"a".repeat(128)));
    assert!(!owner::valid_process_code(&"a".repeat(129)));
}

#[test]
fn executor_clock_boundaries_are_strict_and_allow_only_one_second_future() {
    for (age, fresh) in [
        (-1_001, false),
        (-1_000, true),
        (0, true),
        (14_999, true),
        (15_000, false),
    ] {
        assert_eq!(manager::executor_fresh(100_000, 100_000 - age), fresh);
    }
}
