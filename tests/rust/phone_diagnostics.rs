use ivy_contracts::{IvyJson, json};
use ivy_phone_bridge::diagnostics::PhoneDiagnostics;
use std::process::Command;

#[test]
fn diagnostic_recovery_order_bounds_and_copy_match_existing_typescript() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let oracle = Command::new("node")
        .args([
            "--disable-warning=ExperimentalWarning",
            "--experimental-transform-types",
            "tests/fixtures/phone-diagnostics-oracle.mjs",
        ])
        .current_dir(root)
        .output()
        .expect("Node is required for the retained TypeScript oracle");
    assert!(
        oracle.status.success(),
        "{}",
        String::from_utf8_lossy(&oracle.stderr)
    );
    let expected = json::parse(std::str::from_utf8(&oracle.stdout).unwrap(), 1_048_576).unwrap();
    let mut diagnostics = PhoneDiagnostics::new("phone".into());
    let mut at = "2026-01-01T00:00:00.000Z";
    let mut trace = Vec::new();
    let snapshot = |trace: &mut Vec<IvyJson>, diagnostics: &PhoneDiagnostics| {
        trace.push(IvyJson::Array(diagnostics.snapshot()))
    };
    let registration = |state: &str, error: IvyJson| {
        IvyJson::object([("state", state.into()), ("lastKeepAliveError", error)])
    };
    diagnostics.registration_observed(
        true,
        Some(&registration("failed", "private OS detail".into())),
        at,
    );
    diagnostics.poll_succeeded();
    snapshot(&mut trace, &diagnostics);
    diagnostics.registration_observed(
        true,
        Some(&registration("registered", "private OS detail".into())),
        at,
    );
    snapshot(&mut trace, &diagnostics);
    diagnostics.registration_observed(true, Some(&registration("registered", IvyJson::Null)), at);
    snapshot(&mut trace, &diagnostics);
    diagnostics.registration_observed(true, None, at);
    snapshot(&mut trace, &diagnostics);
    diagnostics.registration_observed(false, None, at);
    snapshot(&mut trace, &diagnostics);
    trace.push(diagnostics.voice_failed("voice_unavailable", at).into());
    trace.push(diagnostics.voice_failed("voice_unavailable", at).into());
    diagnostics.poll_succeeded();
    diagnostics.call_succeeded();
    snapshot(&mut trace, &diagnostics);
    trace.push(diagnostics.voice_succeeded().into());
    trace.push(diagnostics.voice_succeeded().into());
    snapshot(&mut trace, &diagnostics);
    diagnostics.call_failed("original-private-id", "cleanup_unknown", at);
    diagnostics.poll_failed("native_unavailable", at);
    snapshot(&mut trace, &diagnostics);
    at = "2026-01-01T00:00:01.000Z";
    diagnostics.call_failed("original-private-id", "cleanup_unknown", at);
    diagnostics.poll_succeeded();
    snapshot(&mut trace, &diagnostics);
    diagnostics.call_failed("second", "other_unknown", at);
    snapshot(&mut trace, &diagnostics);
    diagnostics.call_failed("original-private-id", "new_code", at);
    snapshot(&mut trace, &diagnostics);
    diagnostics.call_succeeded();
    snapshot(&mut trace, &diagnostics);
    for index in 0..33 {
        diagnostics.call_failed(&format!("call{index}"), &format!("error{index}"), at);
    }
    diagnostics.call_failed("call1", "replacement", at);
    diagnostics.call_failed("call33", "error33", at);
    snapshot(&mut trace, &diagnostics);
    diagnostics.poll_failed("poll", at);
    diagnostics.voice_failed("voice", at);
    diagnostics.registration_observed(true, None, at);
    snapshot(&mut trace, &diagnostics);
    assert_eq!(IvyJson::Array(trace.clone()), expected);
    let serialized = json::canonical(&IvyJson::Array(trace), 1_048_576).unwrap();
    assert!(!serialized.contains("private OS detail"));
    assert!(!serialized.contains("original-private-id"));
    let mut detached = diagnostics.snapshot();
    detached[0].insert("code", "mutated".into());
    assert_ne!(
        diagnostics.snapshot()[0].get("code"),
        Some(&"mutated".into())
    );
}
