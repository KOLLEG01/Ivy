use ivy_contracts::{IvyError, JsString, hash::digest};
use ivy_task_board::evidence_bytes::*;
use serde_json::{Value, json};
use std::cell::RefCell;

#[path = "../fixtures/task-board-evidence-support.rs"]
mod support;
use support::oracle;

#[path = "../fixtures/task-board-native-evidence.rs"]
mod native_evidence;
#[path = "../fixtures/task-board-read-evidence.rs"]
mod read_evidence;

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

#[test]
fn canonical_decoding_matches_original_error_boundaries() {
    for case in oracle("decode", None).as_array().unwrap() {
        let body: Vec<u8> = case["hex"]
            .as_str()
            .unwrap()
            .as_bytes()
            .chunks_exact(2)
            .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
            .collect();
        let store = Memory {
            mutation: String::new(),
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
            read_evidence(
                &store,
                &owner,
                &Manifest {
                    byte_length: body.len(),
                    content_hash: digest(&body),
                    chunks,
                },
                key,
            )
        });
        let actual = match result {
            Ok(value) => {
                json!({"canonicalHash":digest(ivy_contracts::json::canonical(&value, MAXIMUM_BYTES).unwrap().as_bytes())})
            }
            Err(error) => json!({"code":error.code,"message":error.message}),
        };
        assert_eq!(actual, case["result"], "{}", case["name"]);
        let reads: Vec<Value> = store
            .reads
            .borrow()
            .iter()
            .map(|entry| entry["index"].clone())
            .collect();
        assert_eq!(json!(reads), case["reads"], "{}", case["name"]);
    }
}
