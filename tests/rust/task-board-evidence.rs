use ivy_contracts::{IvyError, JsString, hash::digest};
use ivy_store::rusqlite::{Connection, params};
use ivy_task_board::{evidence_bytes::*, local_blobs::*};
use serde_json::{Value, json};
use std::{
    cell::RefCell,
    path::{Path, PathBuf},
    process::Command,
};

fn oracle(mode: &str, path: Option<&Path>) -> Value {
    let mut command = Command::new("node");
    command
        .args([
            "--experimental-transform-types",
            "--disable-warning=ExperimentalWarning",
            "tests/fixtures/task-board-evidence-oracle.mjs",
            mode,
        ])
        .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."));
    if let Some(path) = path {
        command.arg(path);
    }
    let result = command.output().unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    serde_json::from_slice(&result.stdout).unwrap()
}

struct Memory {
    mutation: String,
    saved: RefCell<Vec<Value>>,
    reads: RefCell<Vec<Value>>,
    bytes: RefCell<Vec<Vec<u8>>>,
}
impl BlobStore for Memory {
    type Pin = usize;
    type Error = IvyError;
    fn save(
        &self,
        key: ChunkKey,
        name: &str,
        parent: &JsString,
        bytes: &[u8],
    ) -> Result<usize, IvyError> {
        let index = self.saved.borrow().len();
        if self.mutation == "save-failure" && index == 1 {
            return Err(IvyError::new("fixture_failed", "Interrupted save."));
        }
        self.saved.borrow_mut().push(json!({"key":key.as_str(),"name":name,"parent":parent.to_utf8().unwrap(),"byteLength":bytes.len(),"contentHash":digest(bytes)}));
        self.bytes.borrow_mut().push(bytes.to_vec());
        Ok(index)
    }
    fn read(&self, key: ChunkKey, index: &usize, parent: &JsString) -> Result<Vec<u8>, IvyError> {
        self.reads
            .borrow_mut()
            .push(json!({"key":key.as_str(),"index":index,"parent":parent.to_utf8().unwrap()}));
        if self.mutation == "read-failure" {
            return Err(IvyError::new("fixture_failed", "Unavailable chunk."));
        }
        Ok(self.bytes.borrow()[*index].clone())
    }
}

#[test]
fn ordered_chunk_mechanics_match_original_typescript() {
    let cases = oracle("cases", None);
    for case in cases.as_array().unwrap() {
        let size = case["size"].as_u64().unwrap() as usize;
        let mutation = case["mutation"].as_str().unwrap();
        let mut body = vec![0; size];
        for (index, part) in body.chunks_mut(CHUNK_BYTES).enumerate() {
            part.fill(65 + (index % 26) as u8);
        }
        let store = Memory {
            mutation: mutation.into(),
            saved: RefCell::default(),
            reads: RefCell::default(),
            bytes: RefCell::default(),
        };
        let key = if case["readKey"] == true {
            ChunkKey::NativeReadEvidence
        } else {
            ChunkKey::NativeEvidence
        };
        let owner = "owner".into();
        let result = save_chunks(&store, &owner, &body, key).and_then(|chunks| {
            let mut manifest = Manifest {
                byte_length: size,
                content_hash: digest(&body),
                chunks,
            };
            match mutation {
                "count" => {
                    manifest.chunks.pop();
                }
                "empty" => manifest.byte_length = 0,
                "oversize" => manifest.byte_length = MAXIMUM_BYTES + 1,
                "length" => manifest.byte_length -= 1,
                "chunk-length" => manifest.chunks[0].byte_length += 1,
                "chunk-hash" => manifest.chunks[0].content_hash = "wrong".into(),
                "body-hash" => manifest.content_hash = "wrong".into(),
                "reorder" => manifest.chunks.reverse(),
                "bytes" => store.bytes.borrow_mut()[0][0] ^= 1,
                "short" => {
                    store.bytes.borrow_mut()[0].remove(0);
                }
                _ => {}
            }
            read_verified_bytes(&store, &owner, &manifest, key)
        });
        let actual = match result {
            Ok(bytes) => json!({"byteLength":bytes.len(),"contentHash":digest(&bytes)}),
            Err(error) => json!({"code":error.code,"message":error.message}),
        };
        assert_eq!(actual, case["result"], "size={size} mutation={mutation}");
        assert_eq!(json!(*store.saved.borrow()), case["saved"]);
        assert_eq!(json!(*store.reads.borrow()), case["reads"]);
    }
}

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
fn journal_cross_open_identity_ownership_and_rollback() {
    let temp = Temporary::new();
    let path = temp.0.join("task-board.sqlite");
    let created = oracle("create", Some(&path));
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
        db.query_row("SELECT COUNT(*) FROM blobs", [], |row| row.get::<_, u64>(0))
            .unwrap(),
        2
    );
    drop(db);
    let reopened = oracle("inspect", Some(&path));
    assert_eq!(
        reopened["pin"]["objectId"],
        pin.object_id.to_utf8().unwrap()
    );
    assert_eq!(reopened["text"], "from rust");
    assert_eq!(reopened["usage"], json!({"rows":2,"bytes":18}));
}

#[test]
fn partial_chunk_save_replays_original_pins_and_preserves_usage() {
    let temp = Temporary::new();
    let path = temp.0.join("task-board.sqlite");
    oracle("create", Some(&path));
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
    let counts: (u64, u64) = db
        .query_row(
            "SELECT rows,bytes FROM storage_usage WHERE kind='blobs'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(counts, (3, (body.len() + 9) as u64));
    db.execute("DELETE FROM blobs WHERE id=?", params![id])
        .unwrap();
    assert!(
        read_verified_bytes(&store, &"owner".into(), &manifest, ChunkKey::NativeEvidence).is_err()
    );
}
