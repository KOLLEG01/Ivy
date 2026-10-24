use ivy_contracts::{IvyJson, json};
use ivy_phone_bridge::diagnostics::PhoneDiagnostics;

#[test]
fn media_cleanup_and_work_reconciliation_recover_independently() {
    let mut diagnostics = PhoneDiagnostics::new("fixture.phone".into());
    let at = "2026-10-24T08:00:00.000Z";
    diagnostics.cleanup_observed("original", Some("phone_stop_unknown"), at);
    diagnostics.work_observed("original", Some("deadline_exceeded"), at);
    diagnostics.call_succeeded();
    let scopes = |diagnostics: &PhoneDiagnostics| diagnostics.snapshot().into_iter()
        .map(|v| v.required("resource").unwrap().required("scope").unwrap().as_str().unwrap()).collect::<Vec<_>>();
    assert_eq!(scopes(&diagnostics), ["call_cleanup", "work_observation"]);
    diagnostics.cleanup_observed("another", None, at);
    assert_eq!(diagnostics.snapshot().len(), 2);
    diagnostics.cleanup_observed("original", None, at);
    assert_eq!(scopes(&diagnostics), ["work_observation"]);
    diagnostics.work_observed("original", None, at);
    assert!(diagnostics.snapshot().is_empty());
}

#[test]
fn diagnostics_preserve_recovery_order_bounds_and_redact_private_details() {
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
    assert_eq!(trace.len(), 18);
    for (index, codes) in [
        (0, vec!["phone_registration_unavailable"]),
        (1, vec!["phone_registration_keepalive_failed"]),
        (2, vec![]),
        (3, vec!["phone_registration_unavailable"]),
        (4, vec![]),
        (7, vec!["voice_unavailable"]),
        (10, vec![]),
        (11, vec!["cleanup_unknown", "native_unavailable"]),
        (12, vec!["cleanup_unknown"]),
        (13, vec!["cleanup_unknown", "other_unknown"]),
        (14, vec!["new_code", "other_unknown"]),
        (15, vec![]),
    ] {
        let actual = trace[index]
            .as_array()
            .unwrap()
            .iter()
            .map(|warning| warning.get("code").unwrap().as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(actual, codes, "observation {index}");
    }
    assert_eq!(trace[5], true.into());
    assert_eq!(trace[6], false.into());
    assert_eq!(trace[8], true.into());
    assert_eq!(trace[9], false.into());
    let call = |index: usize| &trace[index].as_array().unwrap()[0];
    assert_eq!(
        call(12).get("firstObservedAt"),
        Some(&"2026-01-01T00:00:00.000Z".into())
    );
    assert_eq!(call(12).get("lastObservedAt"), Some(&at.into()));
    assert_eq!(call(14).get("firstObservedAt"), Some(&at.into()));
    let bounded = trace[16].as_array().unwrap();
    assert_eq!(bounded.len(), 32);
    assert_eq!(
        bounded
            .iter()
            .map(|warning| warning.get("code").unwrap().as_str().unwrap())
            .collect::<Vec<_>>(),
        (2..=33)
            .map(|index| format!("error{index}"))
            .collect::<Vec<_>>()
    );
    let all = trace[17].as_array().unwrap();
    assert_eq!(all.len(), 35);
    assert_eq!(all[32].get("code"), Some(&"poll".into()));
    assert_eq!(all[33].get("code"), Some(&"voice".into()));
    assert_eq!(
        all[34].get("code"),
        Some(&"phone_registration_unavailable".into())
    );
    for warning in all {
        assert_eq!(warning.get("source"), Some(&"phone-bridge".into()));
        assert_eq!(warning.get("severity"), Some(&"warning".into()));
        assert_eq!(
            warning.get("resource").unwrap().get("serviceNodeId"),
            Some(&"phone".into())
        );
    }
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
