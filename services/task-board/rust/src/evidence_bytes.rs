//! Immutable byte mechanics shared by native evidence and native read evidence.
//! Each evidence type separately verifies the decoded value's identity and scope.
use ivy_contracts::{
    IvyError, IvyJson, JsString,
    hash::digest,
    json::{canonical, parse},
    limits::MANAGEMENT_FRAME_BYTES,
};

pub const CHUNK_BYTES: usize = 1024 * 1024;
pub const MAXIMUM_BYTES: usize = MANAGEMENT_FRAME_BYTES;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChunkKey {
    NativeEvidence,
    NativeReadEvidence,
}
impl ChunkKey {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NativeEvidence => "task-board/native-evidence-chunk",
            Self::NativeReadEvidence => "task-board/native-read-evidence-chunk",
        }
    }
}

/// Local synchronous operations execute on the journal owner's blocking context.
pub trait BlobStore {
    type Pin;
    type Error: From<IvyError>;
    fn save(
        &self,
        key: ChunkKey,
        name: &str,
        parent: &JsString,
        bytes: &[u8],
    ) -> Result<Self::Pin, Self::Error>;
    fn read(
        &self,
        key: ChunkKey,
        pin: &Self::Pin,
        parent: &JsString,
    ) -> Result<Vec<u8>, Self::Error>;
}

#[derive(Debug, Clone)]
pub struct Chunk<P> {
    pub object: P,
    pub content_hash: String,
    pub byte_length: usize,
}
#[derive(Debug, Clone)]
pub struct Manifest<P> {
    pub byte_length: usize,
    pub content_hash: String,
    pub chunks: Vec<Chunk<P>>,
}

fn require(ok: bool, message: &str) -> Result<(), IvyError> {
    if ok {
        Ok(())
    } else {
        Err(IvyError::new("task_board_evidence_mismatch", message))
    }
}

pub fn save_chunks<S: BlobStore>(
    store: &S,
    parent: &JsString,
    body: &[u8],
    key: ChunkKey,
) -> Result<Vec<Chunk<S::Pin>>, S::Error> {
    require(
        !body.is_empty() && body.len() <= MAXIMUM_BYTES,
        "Native evidence exceeds its complete byte bound.",
    )?;
    let content_hash = digest(body);
    let mut chunks = Vec::new();
    // Deliberately no encompassing transaction: an interrupted save retains earlier
    // immutable chunks, and replay resolves their stable names to the original pins.
    for (index, bytes) in body.chunks(CHUNK_BYTES).enumerate() {
        let name = format!("Native evidence chunk {} {index}", &content_hash[7..]);
        chunks.push(Chunk {
            object: store.save(key, &name, parent, bytes)?,
            content_hash: digest(bytes),
            byte_length: bytes.len(),
        });
    }
    Ok(chunks)
}

/// Verify ordered chunks before decoding. These bytes are not yet trusted JSON.
pub fn read_verified_bytes<S: BlobStore>(
    store: &S,
    parent: &JsString,
    value: &Manifest<S::Pin>,
    key: ChunkKey,
) -> Result<Vec<u8>, S::Error> {
    require(
        value.byte_length > 0 && value.byte_length <= MAXIMUM_BYTES,
        "Evidence exceeds its complete byte bound.",
    )?;
    require(
        value.chunks.len() == value.byte_length.div_ceil(CHUNK_BYTES),
        "Evidence must have the exact bounded ordered chunk count.",
    )?;
    let mut body = Vec::with_capacity(value.byte_length);
    for (index, chunk) in value.chunks.iter().enumerate() {
        let part = store.read(key, &chunk.object, parent)?;
        let expected = CHUNK_BYTES.min(value.byte_length - index * CHUNK_BYTES);
        require(
            part.len() == expected
                && part.len() == chunk.byte_length
                && digest(&part) == chunk.content_hash,
            "Native evidence chunk bytes do not match their ordered immutable length/hash pin.",
        )?;
        body.extend_from_slice(&part);
    }
    require(
        body.len() == value.byte_length && digest(&body) == value.content_hash,
        "The complete native evidence differs from the saved body hash or length.",
    )?;
    Ok(body)
}

/// Decode only after immutable byte verification. Syntax errors are evidence
/// errors; canonical validation errors retain their original domain and order.
pub fn read_evidence<S: BlobStore>(
    store: &S,
    parent: &JsString,
    value: &Manifest<S::Pin>,
    key: ChunkKey,
) -> Result<IvyJson, S::Error> {
    let body = read_verified_bytes(store, parent, value, key)?;
    let invalid_json = || {
        IvyError::new(
            "task_board_evidence_mismatch",
            "Native evidence is not valid UTF-8 JSON.",
        )
    };
    let decoded = std::str::from_utf8(&body).map_err(|_| invalid_json())?;
    // TextDecoder strips one leading UTF-8 BOM before JSON.parse and comparison.
    let decoded = decoded.strip_prefix('\u{feff}').unwrap_or(decoded);
    let observation = parse(decoded, MAXIMUM_BYTES).map_err(|_| invalid_json())?;
    require(
        canonical(&observation, MAXIMUM_BYTES)? == decoded,
        "Native evidence must retain canonical JSON bytes.",
    )?;
    Ok(observation)
}
