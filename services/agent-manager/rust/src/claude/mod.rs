//! Rust-owned Claude protocol/state and its private pinned SDK transport.
//! Public protocol and saved data stay with the native owner; JavaScript is
//! limited to calls into the vendor SDK over a private framed transport.
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
pub mod stdio;
mod subagents;
mod turns;
mod value;
pub mod workflow;
