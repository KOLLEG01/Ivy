use ivy_agent_manager::local_state::{Limits, LocalAgentState, Pin};
#[path = "agent-native-journal.rs"]
mod native_journal_cases;

fn normalized(
    mut saved: ivy_agent_manager::local_state::SavedState,
) -> ivy_agent_manager::local_state::SavedState {
    saved.value =
        value(&ivy_contracts::json::canonical(&saved.value, MANAGEMENT_FRAME_BYTES).unwrap());
    saved
}
use ivy_contracts::{
    IvyJson, JsString, hash::hash_json, json::parse, limits::MANAGEMENT_FRAME_BYTES,
};
use ivy_store::rusqlite::{Connection, params};
use std::{
    path::PathBuf,
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture {
    root: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "ivy-agent-state-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&root).unwrap();
        Self { root }
    }
    fn path(&self) -> PathBuf {
        self.root.join("agent-runtime.sqlite")
    }
    fn open(&self, count: i64, bytes: i64) -> LocalAgentState {
        let mut state = LocalAgentState::open(
            &self.path(),
            "agent".into(),
            Limits {
                max_operations: count,
                max_journal_bytes: bytes,
            },
        )
        .unwrap();
        state.set_runtime_epoch("epoch".into()).unwrap();
        state
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        assert_eq!(self.root.parent(), Some(std::env::temp_dir().as_path()));
        assert!(
            self.root
                .file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .starts_with("ivy-agent-state-")
        );
        std::fs::remove_dir_all(&self.root).unwrap();
    }
}
fn value(text: &str) -> IvyJson {
    parse(text, MANAGEMENT_FRAME_BYTES).unwrap()
}
fn operation(time: u64, phase: &str) -> IvyJson {
    IvyJson::object([
        ("operationId", format!("epoch:{time}:original").into()),
        ("phase", phase.into()),
        ("method", "thread/start".into()),
        ("callerPrincipalId", "caller".into()),
    ])
}
fn assert_code<T: std::fmt::Debug>(result: ivy_contracts::IvyResult<T>, code: &str) {
    assert_eq!(result.unwrap_err().code, code);
}

#[test]
fn canonical_values_and_pins_survive_close_and_compare_and_swap() {
    let f = Fixture::new();
    let mut state = f.open(10, 64 * 1024 * 1024);
    let original = value(r#"{"z":null,"a":[false,"\ud800",{"\udfff":"😀"}]}"#);
    let saved = state
        .write_at("allocation", "key", &original, None, 1_000)
        .unwrap();
    assert_eq!(
        saved.pin.object_id,
        hash_json(&IvyJson::Array(vec![
            "agent".into(),
            "allocation".into(),
            "key".into()
        ]))
        .unwrap()[7..]
    );
    assert_code(
        state.write_at("allocation", "key", &original, None, 1_001),
        "revision_conflict",
    );
    let wrong = Pin {
        object_id: "different".into(),
        revision: 1,
    };
    assert_code(
        state.write_at("allocation", "key", &original, Some(&wrong), 1_001),
        "revision_conflict",
    );
    state.close().unwrap();
    let mut state = f.open(10, 64 * 1024 * 1024);
    assert_eq!(
        state.read("allocation", "key").unwrap(),
        Some(normalized(saved.clone()))
    );
    let updated = state
        .write_at(
            "allocation",
            "key",
            &IvyJson::Bool(true),
            Some(&saved.pin),
            1_002,
        )
        .unwrap();
    assert_eq!(updated.pin.revision, 2);
    assert_code(
        state.write_at("allocation", "key", &original, Some(&saved.pin), 1_003),
        "revision_conflict",
    );
    assert_eq!(
        state.read("allocation", "key").unwrap(),
        Some(normalized(updated))
    );
}

#[test]
fn new_operations_require_current_epoch_and_nonexpired_issue_time() {
    let f = Fixture::new();
    let mut state = LocalAgentState::open(
        &f.path(),
        "agent".into(),
        Limits {
            max_operations: 10,
            max_journal_bytes: 64 * 1024 * 1024,
        },
    )
    .unwrap();
    assert_code(
        state.write_at("operation", "key", &operation(100, "failed"), None, 100),
        "operation_expired",
    );
    assert_code(
        state.set_runtime_epoch("bad:epoch".into()),
        "invalid_arguments",
    );
    state.set_runtime_epoch("epoch".into()).unwrap();
    let mut other = operation(100, "failed");
    other.insert("operationId", "old:100:id".into());
    assert_code(
        state.write_at("operation", "key", &other, None, 100),
        "operation_expired",
    );
    assert_code(
        state.write_at("operation", "key", &operation(60_101, "failed"), None, 100),
        "operation_expired",
    );
    assert_code(
        state.write_at("operation", "key", &operation(0, "failed"), None, 100),
        "operation_expired",
    );
    state
        .write_at("operation", "key", &operation(60_100, "failed"), None, 100)
        .unwrap();
}

#[test]
fn accepted_receipts_reserve_results_and_failed_admission_is_atomic() {
    let f = Fixture::new();
    let mut state = f.open(10, 30 * 1024 * 1024);
    let first = state
        .write_at("operation", "first", &operation(100, "accepted"), None, 100)
        .unwrap();
    assert_code(
        state.write_at(
            "operation",
            "second",
            &operation(101, "dispatched"),
            None,
            101,
        ),
        "native_journal_capacity",
    );
    assert_eq!(
        state.read("operation", "first").unwrap(),
        Some(normalized(first.clone()))
    );
    assert_eq!(state.read("operation", "second").unwrap(), None);
    state
        .write_at(
            "operation",
            "first",
            &operation(100, "failed"),
            Some(&first.pin),
            102,
        )
        .unwrap();
    state
        .write_at(
            "operation",
            "second",
            &operation(101, "dispatched"),
            None,
            103,
        )
        .unwrap();
}

#[test]
fn all_state_kinds_share_capacity_but_only_terminal_operations_are_evictable() {
    let f = Fixture::new();
    let mut state = f.open(2, 64 * 1024 * 1024);
    state
        .write_at("capability", "profile", &IvyJson::Null, None, 100)
        .unwrap();
    state
        .write_at("operation", "old", &operation(100, "failed"), None, 100)
        .unwrap();
    state
        .write_at("allocation", "new", &IvyJson::Bool(true), None, 101)
        .unwrap();
    assert_eq!(state.read("operation", "old").unwrap(), None);
    assert!(state.read("capability", "profile").unwrap().is_some());
    assert_code(
        state.write_at("operation", "another", &operation(102, "failed"), None, 102),
        "native_journal_capacity",
    );
    assert_code(
        state.write_at("operation", "old", &operation(100, "failed"), None, 102),
        "operation_expired",
    );
}

#[test]
fn nested_unknown_outcomes_and_unfenced_legacy_ids_are_never_evicted() {
    for nested in [
        r#"{"nested":{"execution":"unknown"}}"#,
        r#"{"nested":{"code":"native_epoch_lost"}}"#,
        r#"{"nested":{"errorCode":["prefix","outcome_unknown"]}}"#,
        r#"{"nested":{"code":"\ud800outcome_unknown"}}"#,
        r#"{"nested":[{"ownerPhase":"needs_attention"}]}"#,
    ] {
        let f = Fixture::new();
        let mut state = f.open(1, 64 * 1024 * 1024);
        let mut uncertain = operation(100, "failed");
        uncertain.insert("details", value(nested));
        let original = state
            .write_at("operation", "uncertain", &uncertain, None, 100)
            .unwrap();
        assert_code(
            state.write_at("operation", "new", &operation(101, "failed"), None, 101),
            "native_journal_capacity",
        );
        assert_eq!(
            state.read("operation", "uncertain").unwrap(),
            Some(normalized(original))
        );
    }
    let f = Fixture::new();
    let mut state = f.open(1, 64 * 1024 * 1024);
    let saved = state
        .write_at("operation", "old", &operation(100, "failed"), None, 100)
        .unwrap();
    let mut legacy = saved.value;
    legacy.insert("operationId", "legacy-id".into());
    state
        .write_at("operation", "old", &legacy, Some(&saved.pin), 101)
        .unwrap();
    assert_code(
        state.write_at("operation", "new", &operation(102, "failed"), None, 102),
        "native_journal_capacity",
    );
}

#[test]
fn partial_eviction_and_fence_updates_roll_back_when_capacity_still_fails() {
    let f = Fixture::new();
    let mut state = f.open(10, 1_024);
    let old = state
        .write_at("operation", "old", &operation(100, "failed"), None, 100)
        .unwrap();
    assert_code(
        state.write_at("operation", "new", &operation(101, "accepted"), None, 101),
        "native_journal_capacity",
    );
    assert_eq!(
        state.read("operation", "old").unwrap(),
        Some(normalized(old))
    );
    let db = Connection::open(f.path()).unwrap();
    assert_eq!(
        db.query_row::<String, _, _>(
            "SELECT value FROM state_metadata WHERE key='expired_before'",
            [],
            |row| row.get(0)
        )
        .unwrap(),
        "0"
    );
    assert_eq!(
        db.query_row::<i64, _, _>("SELECT count(*) FROM retired_operations", [], |row| row
            .get(0))
            .unwrap(),
        0
    );
}

#[test]
fn completion_owner_is_retained_across_restart_and_explicitly_released_for_eviction() {
    let f = Fixture::new();
    let mut state = f.open(1, 64 * 1024 * 1024);
    let mut completed = operation(100, "succeeded");
    completed.insert("method", "turn/start".into());
    completed.insert("params", value(r#"{"threadId":"thread"}"#));
    completed.insert("reply", value(r#"{"result":{"turn":{"id":"turn"}}}"#));
    state
        .write_at("operation", "original", &completed, None, 100)
        .unwrap();
    state.close().unwrap();
    let mut state = f.open(1, 64 * 1024 * 1024);
    assert_eq!(
        state.completion_caller("thread", "turn").unwrap(),
        Some("caller".into())
    );
    assert_eq!(state.completion_caller("thread", "other").unwrap(), None);
    assert_code(
        state.write_at("operation", "next", &operation(101, "failed"), None, 101),
        "native_journal_capacity",
    );
    state.release_completion("thread", "turn").unwrap();
    state.close().unwrap();
    let mut state = f.open(1, 64 * 1024 * 1024);
    state
        .write_at("operation", "next", &operation(101, "failed"), None, 101)
        .unwrap();
    assert_eq!(state.completion_caller("thread", "turn").unwrap(), None);
}

#[test]
fn a_completed_turn_with_different_callers_has_no_single_owner() {
    let f = Fixture::new();
    let mut state = f.open(10, 64 * 1024 * 1024);
    let mut completed = operation(100, "succeeded");
    completed.insert("method", "turn/start".into());
    completed.insert("params", value(r#"{"threadId":"thread"}"#));
    completed.insert("reply", value(r#"{"result":{"turn":{"id":"turn"}}}"#));
    state
        .write_at("operation", "first", &completed, None, 100)
        .unwrap();
    completed.insert("callerPrincipalId", "another".into());
    state
        .write_at("operation", "second", &completed, None, 100)
        .unwrap();
    assert_eq!(state.completion_caller("thread", "turn").unwrap(), None);
}

#[test]
fn eviction_uses_original_terminal_time_then_row_order_and_protects_updated_key() {
    let f = Fixture::new();
    let mut state = f.open(2, 64 * 1024 * 1024);
    let first = state
        .write_at("operation", "first", &operation(100, "failed"), None, 100)
        .unwrap();
    state
        .write_at("operation", "second", &operation(101, "failed"), None, 101)
        .unwrap();
    state
        .write_at(
            "operation",
            "first",
            &operation(100, "failed"),
            Some(&first.pin),
            102,
        )
        .unwrap();
    state
        .write_at("operation", "third", &operation(103, "failed"), None, 103)
        .unwrap();
    assert_eq!(state.read("operation", "first").unwrap(), None);
    assert!(state.read("operation", "second").unwrap().is_some());
    let db = Connection::open(f.path()).unwrap();
    assert_eq!(
        db.query_row::<String, _, _>(
            "SELECT value FROM state_metadata WHERE key='expired_before'",
            [],
            |row| row.get(0)
        )
        .unwrap(),
        "100"
    );
    assert_code(
        state.write_at("operation", "first", &operation(100, "failed"), None, 104),
        "operation_expired",
    );
}

#[test]
fn utf16_operation_nonces_survive_admission_reopen_eviction_and_replay_fence() {
    let f = Fixture::new();
    let mut state = f.open(1, 64 * 1024 * 1024);
    let mut original = operation(100, "failed");
    original.insert("operationId", value(r#""epoch:100:\ud800""#));
    let saved = state
        .write_at("operation", "first", &original, None, 100)
        .unwrap();
    state.close().unwrap();
    let mut state = f.open(1, 64 * 1024 * 1024);
    assert_eq!(
        state.read("operation", "first").unwrap(),
        Some(normalized(saved))
    );
    state
        .write_at("operation", "next", &operation(101, "failed"), None, 101)
        .unwrap();
    assert_eq!(state.read("operation", "first").unwrap(), None);
    assert_code(
        state.write_at("operation", "first", &original, None, 102),
        "operation_expired",
    );
    state.close().unwrap();
    let checked = Command::new("node").args(["--input-type=module", "-e", r#"
        import assert from 'node:assert/strict';
        import { DatabaseSync } from 'node:sqlite';
        const db = new DatabaseSync(process.argv[1]);
        assert.equal(db.prepare("SELECT value FROM state_metadata WHERE key='expired_before'").get().value, '100');
        assert.equal(db.prepare("SELECT count(*) AS n FROM state WHERE key='first'").get().n, 0);
        db.close();
    "#]).arg(f.path()).output().unwrap();
    assert!(
        checked.status.success(),
        "{}",
        String::from_utf8_lossy(&checked.stderr)
    );
}

#[test]
fn node_sqlite_and_rust_cross_open_without_rewriting_existing_rows() {
    let f = Fixture::new();
    let created = Command::new("node").args(["--input-type=module", "-e", r#"
        import {DatabaseSync} from 'node:sqlite';
        const db=new DatabaseSync(process.argv[1]);
        db.exec(`PRAGMA journal_mode=WAL; CREATE TABLE state (
            kind TEXT NOT NULL,key TEXT NOT NULL,revision INTEGER NOT NULL,value_json TEXT NOT NULL,
            byte_length INTEGER NOT NULL,terminal_at_ms INTEGER,updated_at_ms INTEGER NOT NULL,
            PRIMARY KEY(kind,key)) STRICT;`);
        const json='{"original":"\\ud800","opaque":null}';
        db.prepare('INSERT INTO state VALUES(?,?,?,?,?,?,?)').run('allocation','legacy',7,json,Buffer.byteLength(json),null,123);
        db.close();
    "#]).arg(f.path()).output().unwrap();
    assert!(
        created.status.success(),
        "{}",
        String::from_utf8_lossy(&created.stderr)
    );
    let mut state = f.open(10, 64 * 1024 * 1024);
    let legacy = state.read("allocation", "legacy").unwrap().unwrap();
    assert_eq!(legacy.pin.revision, 7);
    assert_eq!(
        legacy.value.get("original"),
        Some(&IvyJson::String(JsString(vec![0xd800])))
    );
    state
        .write_at(
            "allocation",
            "rust",
            &value(r#"{"exact":[null,false,"\udfff"]}"#),
            None,
            124,
        )
        .unwrap();
    state.close().unwrap();
    let checked = Command::new("node")
        .args([
            "--input-type=module",
            "-e",
            r#"
        import assert from 'node:assert/strict'; import {DatabaseSync} from 'node:sqlite';
        const db=new DatabaseSync(process.argv[1]);
        const legacy=db.prepare('SELECT * FROM state WHERE key=?').get('legacy');
        assert.equal(legacy.revision,7); assert.equal(legacy.updated_at_ms,123);
        assert.equal(legacy.value_json,'{"original":"\\ud800","opaque":null}');
        const rust=db.prepare('SELECT * FROM state WHERE key=?').get('rust');
        assert.deepEqual(JSON.parse(rust.value_json),{exact:[null,false,'\udfff']});
        assert.equal(rust.byte_length,Buffer.byteLength(rust.value_json));
        assert.equal(db.prepare('PRAGMA integrity_check').get().integrity_check,'ok'); db.close();
    "#,
        ])
        .arg(f.path())
        .output()
        .unwrap();
    assert!(
        checked.status.success(),
        "{}",
        String::from_utf8_lossy(&checked.stderr)
    );
    let db = Connection::open(f.path()).unwrap();
    let tables: i64 = db
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='table' AND name IN (?,?,?,?)",
            params![
                "state",
                "state_metadata",
                "completion_receipts",
                "retired_operations"
            ],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(tables, 4);
}
