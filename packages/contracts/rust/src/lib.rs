//! Wire-domain primitives. Use IvyJson at protocol and identity boundaries;
//! serde_json::Value cannot retain JavaScript strings containing lone surrogates.
pub mod agent_schema;
pub mod builtin_schema;
pub mod browser_notice;
pub mod core_schema;
pub mod error;
pub mod hash;
pub mod json;
pub mod limits;
pub mod operation;
pub mod read_request;
pub mod re2;
pub mod schema;
pub mod validation;
pub mod wire_schema;

pub use error::{IvyError, IvyResult, Outcome};
pub use json::{IvyJson, JsString};
