use super::*;
use crate::retention::graph_tests::{LIVE, PEAK};
use std::{path::Path, sync::{Arc, atomic::{AtomicUsize, Ordering}}};

fn fixture() -> Store {
    Store::open(Path::new(":memory:")).unwrap()
}
fn insert(store: &Store, id: &str, encoded: &str) {
    store.connection().execute("INSERT INTO metadata VALUES(?,?)",
        params![format!("oauth_refresh:{id}"), encoded]).unwrap();
}
fn remaining(store: &Store) -> Vec<String> {
    store.connection().prepare("SELECT key FROM metadata WHERE key LIKE 'oauth_refresh:%' ORDER BY key")
        .unwrap().query_map([], |row| row.get(0)).unwrap().collect::<rusqlite::Result<_>>().unwrap()
}

#[test]
fn oauth_cleanup_keeps_binding_expiry_and_revocation_semantics() {
    for active in [None, Some(vec![JsString(vec![0xd800, b'a' as u16])])] {
        let mut store = fixture();
        insert(&store, "revoked", r#"{"revoked":true,"expiresAt":{"toString":false}}"#);
        insert(&store, "expired", r#"{"expiresAt":[["100"]]}"#);
        insert(&store, "live", r#"{"expiresAt":101,"credentialReference":"\ud800a"}"#);
        insert(&store, "nan", r#"{"expiresAt":"not-a-number","credentialReference":"\ud800a"}"#);
        insert(&store, "inactive", r#"{"expiresAt":101,"credentialReference":"other"}"#);
        let count = store.transaction(|s| collect_oauth_families(s, 100, active.as_deref())).unwrap();
        let expected: Vec<_> = if active.is_some() { vec!["live", "nan"] } else { vec!["inactive", "live", "nan"] }
            .into_iter().map(|id| format!("oauth_refresh:{id}")).collect();
        assert_eq!(count, expected.len());
        assert_eq!(remaining(&store), expected);
    }
}

#[test]
fn oauth_cleanup_validates_all_json_before_writes_and_preserves_field_error_order() {
    for malformed_json in [true, false] {
        let mut store = fixture();
        insert(&store, "first", r#"{"expiresAt":0}"#);
        insert(&store, "bad", if malformed_json { "[" } else { r#"{"expiresAt":{"toString":false}}"# });
        insert(&store, "last", r#"{"expiresAt":0}"#);
        let writes = Arc::new(AtomicUsize::new(0));
        let observed = writes.clone();
        store.connection().create_scalar_function("observe_delete", 0, rusqlite::functions::FunctionFlags::SQLITE_UTF8,
            move |_| { observed.fetch_add(1, Ordering::Relaxed); Ok(0_i64) }).unwrap();
        store.connection().execute_batch("CREATE TRIGGER observe_oauth_delete BEFORE DELETE ON metadata WHEN OLD.key LIKE 'oauth_refresh:%' BEGIN SELECT observe_delete(); END;").unwrap();
        let before = remaining(&store);
        let error = store.transaction(|s| collect_oauth_families(s, 100, None)).unwrap_err();
        assert_eq!(error.code, if malformed_json { "storage_invalid" } else { "internal_error" });
        assert_eq!(writes.load(Ordering::Relaxed), usize::from(!malformed_json),
            "Syntax validation precedes every write; field errors follow preceding deletes");
        assert_eq!(remaining(&store), before, "The caller's transaction must roll back");
    }
}

#[test]
fn oauth_cleanup_releases_each_full_state_before_reading_the_next() {
    const CHILD: &str = "IVY_OAUTH_RETENTION_MEMORY_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "worker_identity::retention_tests::oauth_cleanup_releases_each_full_state_before_reading_the_next", "--nocapture"])
            .env(CHILD, "1").output().unwrap();
        eprint!("{}", String::from_utf8_lossy(&output.stderr));
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stdout));
        assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed;"), "No memory test selected");
        return;
    }
    let mut store = fixture();
    for index in 0..48 {
        insert(&store, &format!("family{index:02}"), &serde_json::json!({
            "expiresAt":if index % 2 == 0 { 0 } else { 1000 }, "opaque":"x".repeat(64 * 1024),
        }).to_string());
    }
    let before = LIVE.load(Ordering::Relaxed);
    PEAK.store(before, Ordering::Relaxed);
    let retained = store.transaction(|s| collect_oauth_families(s, 100, None)).unwrap();
    let peak = PEAK.load(Ordering::Relaxed) - before;
    eprintln!("OAuth cleanup Rust allocation peak: {peak} bytes above owned store, 48 families / 3 MiB opaque content");
    assert_eq!(retained, 24);
    assert_eq!(remaining(&store).len(), 24);
    assert!(peak < 512 * 1024, "OAuth cleanup retained the complete family population: {peak}");
}
