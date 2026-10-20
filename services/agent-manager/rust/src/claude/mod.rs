//! Rust-owned state for the Claude compatibility adapter.
//!
//! The legacy adapter/catalog identity is unchanged. Provider execution and the
//! SDK bridge are separate from this synchronous SQLite owner.
pub mod store;
