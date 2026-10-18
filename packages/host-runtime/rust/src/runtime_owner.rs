//! Windows service ownership policy and lifecycle, composed with shared host primitives.
pub mod policy;
pub mod stop;
mod supervision;
pub use supervision::{RuntimeOwner, TargetPolicyResolver};
