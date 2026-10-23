//! Rust-owned Claude protocol/state and its private pinned SDK transport.
//! The legacy adapter/catalog identity is unchanged. Host bindings and
//! packaging are separate from protocol state and the private SDK owner.
pub mod ask;
pub mod bridge;
mod commands;
mod config;
pub mod driver;
pub mod error;
pub mod images;
mod items;
pub mod models;
pub mod policy;
pub mod projects;
pub mod protocol;
pub mod rate_limits;
pub mod sdk_events;
pub mod sdk_options;
pub mod store;
mod subagents;
mod turns;
mod value;
pub mod workflow;
