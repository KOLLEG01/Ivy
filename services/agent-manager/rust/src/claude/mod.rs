//! Rust-owned Claude protocol/state and its private pinned SDK transport.
//! The legacy adapter/catalog identity is unchanged. Host bindings, raw SDK
//! event translation and packaging are separate from the synchronous owner.
pub mod ask;
pub mod bridge;
pub mod error;
pub mod images;
mod items;
pub mod models;
pub mod policy;
pub mod projects;
pub mod protocol;
pub mod rate_limits;
pub mod sdk_options;
pub mod store;
mod subagents;
mod turns;
mod value;
