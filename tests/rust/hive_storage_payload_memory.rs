use super::*;
use crate::retention::graph_tests::{LIVE, PEAK};
use std::sync::atomic::Ordering;
use crate::store::Credential;

#[test]
fn authentication_payloads_move_digests_and_charge_allocated_capacity() {
    use crate::store::AuthContext;
    for end_session in [false, true] {
        let mut credential = Vec::with_capacity(8192);
        credential.extend("digest".encode_utf16());
        let mut session = Vec::with_capacity(16384);
        session.extend("session".encode_utf16());
        let credential_pointer = credential.as_ptr();
        let session_pointer = session.as_ptr();
        let bytes = (credential.capacity() + session.capacity()) * 2;
        let context = AuthContext {
            credential_digest: JsString(credential), session_digest: Some(JsString(session)),
        };
        let payload = if end_session { Payload::EndSession(context) } else { Payload::Authenticate(context) };
        assert_eq!(payload.retained_bytes(), bytes);
        let (Payload::Authenticate(context) | Payload::EndSession(context)) = payload else { unreachable!() };
        assert_eq!(context.credential_digest.0.as_ptr(), credential_pointer);
        assert_eq!(context.session_digest.unwrap().0.as_ptr(), session_pointer);
    }
    for resolve in [false, true] {
        let mut digest = Vec::with_capacity(8192);
        digest.extend("digest".encode_utf16());
        let pointer = digest.as_ptr();
        let bytes = digest.capacity() * 2;
        let payload = if resolve { Payload::ResolveSession(JsString(digest)) } else { Payload::CreateSession(JsString(digest)) };
        assert_eq!(payload.retained_bytes(), bytes);
        let (Payload::ResolveSession(digest) | Payload::CreateSession(digest)) = payload else { unreachable!() };
        assert_eq!(digest.0.as_ptr(), pointer);
    }
}

#[test]
fn credential_payload_shares_its_backing_instead_of_copying_json_and_strings() {
    const CHILD: &str = "IVY_CREDENTIAL_PAYLOAD_MEMORY_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "storage::payload_tests::credential_payload_shares_its_backing_instead_of_copying_json_and_strings", "--nocapture"])
            .env(CHILD, "1").output().unwrap();
        eprint!("{}", String::from_utf8_lossy(&output.stderr));
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stdout));
        assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed;"), "Memory child selected no test");
        return;
    }
    let credentials: Vec<_> = (0..64).map(|index| Credential {
        principal_id: format!("credential{index:02}{}", "p".repeat(116)).into(),
        digest: format!("sha256:{}", "a".repeat(64)).into(),
    }).collect();
    let before = LIVE.load(Ordering::Relaxed);
    PEAK.store(before, Ordering::Relaxed);
    // Previous lifecycle envelope and the worker's decoded vector coexisted.
    let encoded = IvyJson::Array(credentials.iter().map(|value| IvyJson::object([
        ("principalId", value.principal_id.clone().into()), ("digest", value.digest.clone().into()),
    ])).collect());
    let decoded: Vec<_> = encoded.as_array().unwrap().iter().map(|value| Credential {
        principal_id: value.get("principalId").unwrap().as_js_string().unwrap().clone(),
        digest: value.get("digest").unwrap().as_js_string().unwrap().clone(),
    }).collect();
    let copied_peak = PEAK.load(Ordering::Relaxed) - before;
    assert_eq!(decoded, credentials);
    drop(decoded);
    drop(encoded);
    let pointer = credentials[0].digest.0.as_ptr();
    let before = LIVE.load(Ordering::Relaxed);
    PEAK.store(before, Ordering::Relaxed);
    let shared = Arc::new(credentials);
    let payload = Payload::ConfigureCredentials(shared.clone());
    let shared_peak = PEAK.load(Ordering::Relaxed) - before;
    let Payload::ConfigureCredentials(values) = payload else { unreachable!() };
    assert!(Arc::ptr_eq(&shared, &values));
    assert_eq!(values[0].digest.0.as_ptr(), pointer);
    eprintln!("Hive credential payload Rust allocation peak for 64 credentials: JSON plus decode {copied_peak} bytes, shared native vector {shared_peak} bytes above owned input");
    assert!(copied_peak > 32 * 1024);
    assert!(shared_peak < 1024);
}
