//! Chat's retained evidence and collection budgets, independent of RPC frames.
pub const EVIDENCE_CHUNK_BYTES: usize = 1024 * 1024;
pub const MAXIMUM_EVIDENCE_BYTES: usize = 16 * EVIDENCE_CHUNK_BYTES;
pub const COLLECTION_BYTES: usize = 64 * EVIDENCE_CHUNK_BYTES;
pub const COLLECTION_READS: usize = 1024;
