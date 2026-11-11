use super::{isolated, peak};
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString,
    hash::digest,
    json::{canonical, parse},
};
use ivy_task_board::evidence_bytes::{
    BlobStore, CHUNK_BYTES, Chunk, ChunkKey, MAXIMUM_BYTES, Manifest, read_evidence,
    read_verified_bytes,
};
use std::cell::Cell;

struct Bytes<'a> {
    body: &'a [u8],
    key: ChunkKey,
    reads: Cell<usize>,
}
impl BlobStore for Bytes<'_> {
    type Pin = usize;
    type Error = IvyError;
    fn save(&self, _: ChunkKey, _: &str, _: &JsString, _: &[u8]) -> IvyResult<usize> {
        panic!("Reading original evidence must not write replacement state.");
    }
    fn read(&self, key: ChunkKey, pin: &usize, parent: &JsString) -> IvyResult<Vec<u8>> {
        assert_eq!(key, self.key);
        assert_eq!(parent, &JsString::from("original-evidence"));
        self.reads.set(self.reads.get() + 1);
        Ok(self.body.chunks(CHUNK_BYTES).nth(*pin).unwrap().to_vec())
    }
}
fn manifest(body: &[u8]) -> Manifest<usize> {
    Manifest {
        byte_length: body.len(),
        content_hash: digest(body),
        chunks: body
            .chunks(CHUNK_BYTES)
            .enumerate()
            .map(|(index, part)| Chunk {
                object: index,
                content_hash: digest(part),
                byte_length: part.len(),
            })
            .collect(),
    }
}
// The removed read path remains an independent compatibility and allocation
// oracle, including the original syntax/canonical error boundary and order.
fn former_read(store: &Bytes<'_>, value: &Manifest<usize>) -> IvyResult<Json> {
    let body = read_verified_bytes(store, &"original-evidence".into(), value, store.key)?;
    let invalid = || {
        IvyError::new(
            "task_board_evidence_mismatch",
            "Native evidence is not valid UTF-8 JSON.",
        )
    };
    let decoded = std::str::from_utf8(&body).map_err(|_| invalid())?;
    let decoded = decoded.strip_prefix('\u{feff}').unwrap_or(decoded);
    let observation = parse(decoded, MAXIMUM_BYTES).map_err(|_| invalid())?;
    if canonical(&observation, MAXIMUM_BYTES)? != decoded {
        return Err(IvyError::new(
            "task_board_evidence_mismatch",
            "Native evidence must retain canonical JSON bytes.",
        ));
    }
    Ok(observation)
}

#[test]
fn original_evidence_keeps_exact_text_bom_hash_chunk_and_error_semantics() {
    let mut bodies: Vec<Vec<u8>> = [
        "null",
        "true",
        "[]",
        "{}",
        "\u{feff}null",
        "\u{feff}\u{feff}null",
        " null",
        "null ",
        "\ntrue",
        "[0,-0]",
        "[0,0]",
        "1e0",
        "1",
        "1e400",
        "9007199254740992",
        "[0,9007199254740992]",
        r#"{"a":1,"b":2}"#,
        r#"{"b":2,"a":1}"#,
        r#"{"a":0,"a":1}"#,
        r#"{"text":"é日本語😀","zero":0}"#,
        r#""\u00e9""#,
        "\"é\"",
        r#""\ud800""#,
        r#""\uD800""#,
        r#""\n\"\\\u0000""#,
        "\"unterminated",
        "[",
        "[true,]",
    ]
    .into_iter()
    .map(|s| s.as_bytes().to_vec())
    .collect();
    bodies.extend([vec![0xff], b"\"\xff\"".to_vec()]);
    for depth in [128, 129] {
        bodies.push(format!("{}null{}", "[".repeat(depth), "]".repeat(depth)).into_bytes());
    }
    for key in [ChunkKey::NativeEvidence, ChunkKey::NativeReadEvidence] {
        for body in &bodies {
            let store = Bytes {
                body,
                key,
                reads: Cell::new(0),
            };
            let value = manifest(body);
            let expected = former_read(&store, &value).map_err(|e| e.to_wire());
            assert_eq!(
                read_evidence(&store, &"original-evidence".into(), &value, key)
                    .map_err(|e| e.to_wire()),
                expected
            );
            let mut wrong = manifest(body);
            wrong.content_hash = digest(b"different original");
            assert_eq!(
                read_evidence(&store, &"original-evidence".into(), &wrong, key)
                    .unwrap_err()
                    .to_wire(),
                former_read(&store, &wrong).unwrap_err().to_wire()
            );
        }
    }
    let body = vec![b'x'; CHUNK_BYTES + 1];
    let store = Bytes {
        body: &body,
        key: ChunkKey::NativeEvidence,
        reads: Cell::new(0),
    };
    let mut wrong = manifest(&body);
    wrong.chunks.swap(0, 1);
    assert_eq!(
        read_evidence(&store, &"original-evidence".into(), &wrong, store.key)
            .unwrap_err()
            .to_wire(),
        former_read(&store, &wrong).unwrap_err().to_wire()
    );
    wrong = manifest(&body);
    wrong.chunks[0].content_hash = digest(b"wrong chunk");
    assert_eq!(
        read_evidence(&store, &"original-evidence".into(), &wrong, store.key)
            .unwrap_err()
            .to_wire(),
        former_read(&store, &wrong).unwrap_err().to_wire()
    );
    wrong = manifest(&body);
    wrong.chunks.pop();
    assert_eq!(
        read_evidence(&store, &"original-evidence".into(), &wrong, store.key)
            .unwrap_err()
            .to_wire(),
        former_read(&store, &wrong).unwrap_err().to_wire()
    );
    assert_eq!(store.body, body);
}

#[test]
fn eight_mib_evidence_read_avoids_complete_canonical_temporary() {
    if isolated("evidence_bytes::eight_mib_evidence_read_avoids_complete_canonical_temporary") {
        return;
    }
    const TEXT_BYTES: usize = 8 * 1024 * 1024;
    let mut body = Vec::with_capacity(TEXT_BYTES + 2);
    body.push(b'"');
    body.resize(TEXT_BYTES + 1, b'x');
    body.push(b'"');
    assert!(body.len() <= MAXIMUM_BYTES);
    let store = Bytes {
        body: &body,
        key: ChunkKey::NativeEvidence,
        reads: Cell::new(0),
    };
    let value = manifest(&body);
    let former = peak(|| drop(former_read(&store, &value).unwrap()));
    let parent = JsString::from("original-evidence");
    let mut decoded = None;
    let current =
        peak(|| decoded = Some(read_evidence(&store, &parent, &value, store.key).unwrap()));
    let decoded = decoded.unwrap();
    let units = &decoded.as_js_string().unwrap().0;
    assert_eq!(units.len(), TEXT_BYTES);
    assert!(units.iter().all(|unit| *unit == u16::from(b'x')));
    assert_eq!(store.reads.get(), value.chunks.len() * 2);
    assert_eq!(value.content_hash, digest(&body));
    eprintln!(
        "8 MiB original evidence allocation peak: former={former} B; current={current} B; avoided={} B; original={} B",
        former - current,
        body.len()
    );
    assert!(
        former >= current + body.len() as isize,
        "The canonical output remained live beside original bytes and parsed JSON."
    );
}
