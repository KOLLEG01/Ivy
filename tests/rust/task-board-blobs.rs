use ivy_contracts::{JsString, hash::digest};
use ivy_store::rusqlite::{Connection, params};
use ivy_task_board::{evidence_bytes::*, local_blobs::*};
use serde_json::json;
use std::path::PathBuf;

#[path = "../fixtures/task-board-artifact.rs"]
mod artifact;
#[path = "../fixtures/task-board-context-recovery.rs"]
mod context_recovery;
#[path = "../fixtures/task-board-gate-journal.rs"]
mod gates;
#[path = "../fixtures/task-board-journal-store.rs"]
mod journal_store;
#[cfg(target_os = "linux")]
#[path = "../fixtures/task-board-journal-memory.rs"]
mod journal_memory;
#[path = "../fixtures/task-board-native-driver.rs"]
mod native_driver;
#[path = "../fixtures/task-board-native-intent.rs"]
mod native_intent;
#[path = "../fixtures/task-board-native-journal.rs"]
mod native_journal;
#[path = "../fixtures/task-board-native-preparation.rs"]
mod native_preparation;
#[path = "../fixtures/task-board-publication.rs"]
mod publication;
#[path = "../fixtures/task-board-retention.rs"]
mod retention;
#[path = "../fixtures/task-board-evidence-support.rs"]
mod support;
#[path = "../fixtures/task-board-workspaces.rs"]
mod workspaces;
use support::{original_blobs, original_gates};

struct Temporary(PathBuf);
impl Temporary {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("ivy-task-board-blobs-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Temporary {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn domain(error: BlobError) -> (String, String) {
    match error {
        BlobError::Domain(error) => (error.code, error.message),
        other => panic!("{other}"),
    }
}

#[test]
fn original_journal_blobs_keep_identity_ownership_and_rollback() {
    let temp = Temporary::new();
    let path = temp.0.join("task-board.sqlite");
    let created = original_blobs(&path);
    let mut db = Connection::open(&path).unwrap();
    let store = LocalBlobs::new(&db);
    let parent = JsString("parent".encode_utf16().chain([0xdfff]).collect());
    let name = JsString("name".encode_utf16().chain([0xd800]).collect());
    let original = BlobPin {
        object_id: created["objectId"].as_str().unwrap().into(),
        revision: 1,
    };
    assert_eq!(
        store.read_blob(&"key".into(), &original, &parent).unwrap(),
        b"from node"
    );
    assert_eq!(
        store
            .save_blob(&"key".into(), &name, &parent, b"from node")
            .unwrap(),
        original
    );
    assert_eq!(
        domain(
            store
                .save_blob(&"key".into(), &name, &parent, b"different")
                .unwrap_err()
        ),
        (
            "revision_conflict".into(),
            "The local evidence identity differs.".into()
        )
    );
    for (key, parent) in [("wrong", parent.clone()), ("key", "wrong".into())] {
        assert_eq!(
            domain(
                store
                    .read_blob(&key.into(), &original, &parent)
                    .unwrap_err()
            ),
            (
                "task_board_evidence_mismatch".into(),
                "The local evidence blob is unavailable.".into()
            )
        );
    }
    let mut wrong = original.clone();
    wrong.revision = 2;
    assert_eq!(
        domain(
            store
                .read_blob(&"wrong".into(), &wrong, &"wrong".into())
                .unwrap_err()
        ),
        (
            "task_board_evidence_mismatch".into(),
            "Local evidence blobs have one revision.".into()
        )
    );
    let pin = store
        .save_blob(
            &"rust-key".into(),
            &"rust-name".into(),
            &"rust-parent".into(),
            b"from rust",
        )
        .unwrap();
    assert_eq!(
        uuid::Uuid::parse_str(&pin.object_id.to_utf8().unwrap())
            .unwrap()
            .get_version_num(),
        4
    );
    {
        let tx = db.transaction().unwrap();
        LocalBlobs::new(&tx)
            .save_blob(
                &"rollback".into(),
                &"name".into(),
                &"parent".into(),
                b"temporary",
            )
            .unwrap();
        tx.rollback().unwrap();
    }
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM blobs", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        2
    );
    drop(db);
    let reopened = Connection::open(&path).unwrap();
    assert_eq!(
        LocalBlobs::new(&reopened)
            .save_blob(
                &"rust-key".into(),
                &"rust-name".into(),
                &"rust-parent".into(),
                b"from rust"
            )
            .unwrap(),
        pin
    );
    let saved: (String, String, String, Vec<u8>, String) = reopened
        .query_row(
            "SELECT id,name,parent_id,bytes,typeof(bytes) FROM blobs WHERE key='rust-key'",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(
        saved,
        (
            pin.object_id.to_utf8().unwrap(),
            "rust-name".into(),
            "rust-parent".into(),
            b"from rust".to_vec(),
            "blob".into()
        )
    );
    let usage: (i64, i64) = reopened
        .query_row(
            "SELECT rows,bytes FROM storage_usage WHERE kind='blobs'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(usage, (2, 18));
}

#[test]
fn original_journal_partial_chunk_save_replays_pins_and_preserves_usage() {
    let temp = Temporary::new();
    let path = temp.0.join("task-board.sqlite");
    original_blobs(&path);
    let db = Connection::open(&path).unwrap();
    db.execute_batch("CREATE TRIGGER interrupt_chunk BEFORE INSERT ON blobs WHEN NEW.key='task-board/native-evidence-chunk' AND EXISTS(SELECT 1 FROM blobs WHERE key=NEW.key) BEGIN SELECT RAISE(ABORT,'fixture interruption'); END;").unwrap();
    let store = LocalBlobs::new(&db);
    let body = vec![65; CHUNK_BYTES + 7];
    assert!(matches!(
        save_chunks(&store, &"owner".into(), &body, ChunkKey::NativeEvidence),
        Err(BlobError::Sqlite(_))
    ));
    let id: String = db
        .query_row(
            "SELECT id FROM blobs WHERE key=?",
            [ChunkKey::NativeEvidence.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    db.execute_batch("DROP TRIGGER interrupt_chunk").unwrap();
    let chunks = save_chunks(&store, &"owner".into(), &body, ChunkKey::NativeEvidence).unwrap();
    assert_eq!(chunks[0].object.object_id.to_utf8().unwrap(), id);
    let manifest = Manifest {
        byte_length: body.len(),
        content_hash: digest(&body),
        chunks,
    };
    assert_eq!(
        read_verified_bytes(&store, &"owner".into(), &manifest, ChunkKey::NativeEvidence).unwrap(),
        body
    );
    let counts: (i64, i64) = db
        .query_row(
            "SELECT rows,bytes FROM storage_usage WHERE kind='blobs'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(counts, (3, (body.len() + 9) as i64));
    db.execute("DELETE FROM blobs WHERE id=?", params![id])
        .unwrap();
    assert!(
        read_verified_bytes(&store, &"owner".into(), &manifest, ChunkKey::NativeEvidence).is_err()
    );
}

#[test]
fn original_journal_gates_preserve_sql_order_and_transaction_boundaries() {
    use ivy_task_board::local_gates::LocalGates;
    let temp = Temporary::new();
    let path = temp.0.join("gates.sqlite");
    let expected = original_gates(&path);
    let db = Connection::open(&path).unwrap();
    let gates = LocalGates::new(&db);
    let operation = JsString("op".encode_utf16().chain([0xdfff]).collect());
    let caller = JsString("caller".encode_utf16().chain([0xd800]).collect());
    let strings = |values: Vec<JsString>| {
        values
            .into_iter()
            .map(|v| v.to_utf8().unwrap())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        gates
            .pending_coordination()
            .unwrap()
            .unwrap()
            .to_utf8()
            .unwrap(),
        expected["pending"]["objectId"]
    );
    assert_eq!(expected["pending"]["revision"], 1);
    for (index, (after, limit)) in [(None, 2), (Some("a".into()), 10), (Some("z".into()), 0)]
        .iter()
        .enumerate()
    {
        let page = gates.pending_page(after.as_ref(), *limit).unwrap().into_iter().map(|entry| json!({
            "gateId":entry.gate_id.to_utf8().unwrap(), "operationId":entry.operation_id.to_utf8().unwrap()
        })).collect::<Vec<_>>();
        assert_eq!(json!(page), expected["pages"][index]);
    }
    assert_eq!(
        json!(strings(gates.operation_gates(&operation).unwrap())),
        expected["gates"]
    );
    assert_eq!(
        gates
            .retired_operation(&caller, &"retired".into())
            .unwrap()
            .unwrap()
            .to_utf8()
            .unwrap(),
        expected["retired"][0]
    );
    assert!(
        gates
            .retired_operation(&"other".into(), &"retired".into())
            .unwrap()
            .is_none()
    );
    gates
        .retain_operation_gates(&operation, &["a".into(), "z".into()])
        .unwrap();
    assert_eq!(
        json!(strings(gates.operation_gates(&operation).unwrap())),
        expected["gates"]
    );
    db.execute_batch("CREATE TEMP TRIGGER fail_gate BEFORE INSERT ON operation_gates WHEN NEW.gate_name='fail' BEGIN SELECT RAISE(ABORT,'fixture gate interruption'); END;").unwrap();
    assert!(
        gates
            .retain_operation_gates(&operation, &["temporary".into(), "fail".into()])
            .is_err()
    );
    assert!(db.is_autocommit());
    assert_eq!(
        json!(strings(gates.operation_gates(&operation).unwrap())),
        expected["gates"]
    );
    db.execute_batch("DROP TRIGGER fail_gate; BEGIN IMMEDIATE;")
        .unwrap();
    db.execute("INSERT INTO markers VALUES('caller-owned','retained')", [])
        .unwrap();
    assert!(
        gates
            .retain_operation_gates(&operation, &["nested".into()])
            .is_err()
    );
    assert!(!db.is_autocommit());
    assert_eq!(
        db.query_row(
            "SELECT value FROM markers WHERE key='caller-owned'",
            [],
            |row| row.get::<_, String>(0)
        )
        .unwrap(),
        "retained"
    );
    db.execute_batch("ROLLBACK").unwrap();
    gates
        .retain_operation_gates(
            &"rust-operation".into(),
            &["b".into(), "a".into(), "b".into()],
        )
        .unwrap();
    drop(db);
    let reopened = Connection::open(&path).unwrap();
    let gates = LocalGates::new(&reopened);
    assert_eq!(
        gates
            .pending_coordination()
            .unwrap()
            .unwrap()
            .to_utf8()
            .unwrap(),
        expected["pending"]["objectId"]
    );
    assert_eq!(
        json!(strings(gates.operation_gates(&operation).unwrap())),
        expected["gates"]
    );
    let rows = reopened.prepare("SELECT operation_id,gate_name FROM operation_gates ORDER BY operation_id,gate_name").unwrap()
        .query_map([], |row| Ok(json!({"operation_id":row.get::<_, String>(0)?,"gate_name":row.get::<_, String>(1)?}))).unwrap()
        .collect::<Result<Vec<_>, _>>().unwrap();
    assert_eq!(
        json!(
            rows.iter()
                .filter(|row| row["operation_id"] != "rust-operation")
                .collect::<Vec<_>>()
        ),
        expected["rows"]
    );
    let rows = rows
        .iter()
        .filter(|row| row["operation_id"] == "rust-operation")
        .collect::<Vec<_>>();
    assert_eq!(
        rows,
        vec![
            &json!({"operation_id":"rust-operation","gate_name":"a"}),
            &json!({"operation_id":"rust-operation","gate_name":"b"})
        ]
    );
}
