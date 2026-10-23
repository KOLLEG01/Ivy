use super::*;
use ivy_contracts::json::parse;

fn policy() -> PhoneAdmission {
    PhoneAdmission::new(parse(r#"{
        "recipients":[{"id":"personal","destination":"+49123456789"}],
        "incoming":[{"peerAddress":"127.0.0.1","transport":"udp","fromUri":"sip:+49123456789@example.test"}],
        "allowDirectDial":true,"dialDomain":"example.test"
    }"#, 4096).unwrap()).unwrap()
}
fn outgoing() -> IvyJson {
    IvyJson::object([
        ("epoch", "10000000-0000-4000-8000-000000000001".into()),
        ("operationId", "10000000-0000-4000-8000-000000000002".into()),
        ("principalId", "authenticated".into()),
        ("recipientId", "personal".into()),
        ("route", "windows".into()),
    ])
}
#[test]
fn configured_and_direct_destinations_retain_policy_and_cleanup_authority() {
    let policy = policy();
    let args = outgoing();
    let mut call = policy.outgoing(&args).unwrap();
    assert_eq!(
        call.get("destination"),
        Some(&"sip:+49123456789@example.test".into())
    );
    assert_eq!(call.get("policyHash"), Some(&policy.hash().into()));
    call.insert("callId", "10000000-0000-4000-8000-000000000003".into());
    policy
        .authorize(&"authenticated".into(), &call, "connect")
        .unwrap();
    call.insert("policyHash", format!("sha256:{}", "0".repeat(64)).into());
    assert_eq!(
        policy
            .authorize(&"authenticated".into(), &call, "connect")
            .unwrap_err()
            .code,
        "phone_policy_changed"
    );
    policy
        .authorize(&"authenticated".into(), &call, "cleanup")
        .unwrap();
    let mut direct = args;
    direct.insert("recipientId", IvyJson::Null);
    direct.insert("destination", "sips:direct@example.test".into());
    assert!(policy.outgoing(&direct).is_ok());
    direct.insert("route", "voice".into());
    assert_eq!(
        policy.outgoing(&direct).unwrap_err().code,
        "phone_destination_refused"
    );
}
#[test]
fn malformed_optional_fields_cannot_be_coerced_into_admission() {
    let policy = policy();
    for (field, value) in [
        ("route", 0.0.into()),
        ("voicePrompt", IvyJson::Array(vec!["prompt".into()])),
        ("screening", false.into()),
        ("destination", 123.0.into()),
    ] {
        let mut args = outgoing();
        args.insert(field, value);
        assert_eq!(
            policy.outgoing(&args).unwrap_err().code,
            "invalid_arguments",
            "{field}"
        );
    }
    let mut voice = outgoing();
    voice.insert("route", "voice".into());
    voice.insert("voicePrompt", "A real prompt".into());
    assert_eq!(
        policy.outgoing(&voice).unwrap().get("voicePrompt"),
        voice.get("voicePrompt")
    );
}
#[test]
fn caller_identity_matches_numbers_without_merging_named_sip_users() {
    for uri in [
        "sip:+49123456789@example.test;user=phone",
        "sips:+49123456789@other.test",
        "TEL:+49123456789",
    ] {
        assert_eq!(
            phone_caller_identity(&uri.into()),
            JsString::from("+49123456789")
        );
    }
    assert_ne!(
        phone_caller_identity(&"sip:alice@example.test".into()),
        phone_caller_identity(&"sip:alice@other.test".into())
    );
}
