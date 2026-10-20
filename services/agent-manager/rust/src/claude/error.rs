//! Native adapter errors retain JavaScript strings at the protocol boundary.
use ivy_contracts::{IvyJson, JsString};

#[derive(Clone, Debug)]
pub struct AdapterError {
    pub name: &'static str,
    pub message: JsString,
}
pub type Result<T> = std::result::Result<T, AdapterError>;
impl AdapterError {
    pub fn new(message: impl Into<JsString>) -> Self {
        Self {
            name: "Error",
            message: message.into(),
        }
    }
    pub fn typed(name: &'static str, message: impl Into<JsString>) -> Self {
        Self {
            name,
            message: message.into(),
        }
    }
    pub fn to_wire(&self) -> IvyJson {
        IvyJson::object([
            ("code", IvyJson::Number(-32000.0)),
            ("message", IvyJson::String(self.message.clone())),
        ])
    }
}
impl std::fmt::Display for AdapterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", String::from_utf16_lossy(&self.message.0))
    }
}
impl std::error::Error for AdapterError {}
impl From<std::io::Error> for AdapterError {
    fn from(error: std::io::Error) -> Self {
        Self::new(error.to_string())
    }
}
impl From<super::store::StoreError> for AdapterError {
    fn from(error: super::store::StoreError) -> Self {
        // Exact JSON.parse diagnostics are supplied by the separate parser intake.
        Self::typed(error.name, error.message)
    }
}
