//! Limits from packages/contracts/src/limits.ts.
pub const MANAGEMENT_FRAME_BYTES: usize = 33_554_432;
pub const MANAGEMENT_SOCKET_BUFFER_BYTES: usize = 2 * MANAGEMENT_FRAME_BYTES;
pub const JSON_OBJECT_CONTENT_BYTES: usize = 1_048_576;
pub const TEXT_OBJECT_CONTENT_BYTES: usize = 1_048_576;
pub const BINARY_OBJECT_CONTENT_BYTES: usize = 8_388_608;
pub const CONNECTION_IN_FLIGHT_REQUESTS: usize = 64;
pub const RPC_BATCH_REQUESTS: usize = 8;
pub const RPC_BATCH_REQUEST_BYTES: usize = 65_536;
pub const CONSUMER_IN_FLIGHT_REQUESTS: usize = 256;
pub const MUTATION_RECEIPT_COUNT: usize = 100_000;
pub const MUTATION_RECEIPT_JOURNAL_BYTES: usize = 256 * 1024 * 1024;
pub const MUTATION_RECEIPT_RESULT_BYTES: usize = 65_536;
pub const MCP_DISCOVERY_RESULT_BYTES: usize = 512 * 1024;
