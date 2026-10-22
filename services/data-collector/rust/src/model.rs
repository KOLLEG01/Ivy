use ivy_contracts::{IvyError, IvyJson, IvyResult};

/// Keep records as lossless JSON: old optional fields and unknown extension fields
/// must survive a read/write during a binary upgrade or rollback.
pub type RecordTask = IvyJson;
pub type Run = IvyJson;

/// Encode wire strings at the SQLite TEXT boundary. JSON documents and canonical
/// request identities continue through IvyJson's lossless codec.
pub fn sqlite_key(value: &IvyJson) -> IvyResult<String> {
    match value {
        IvyJson::String(s) => Ok(String::from_utf16_lossy(&s.0)),
        _ => Err(IvyError::new("invalid_arguments", "Expected a string.")),
    }
}

pub fn field<'a>(value: &'a IvyJson, name: &str) -> IvyResult<&'a IvyJson> {
    value
        .get(name)
        .ok_or_else(|| IvyError::new("invalid_arguments", format!("Missing {name}.")))
}
pub fn text(value: &IvyJson, name: &str) -> IvyResult<String> {
    field(value, name)?.as_str()
}
pub fn number(value: &IvyJson, name: &str) -> IvyResult<f64> {
    field(value, name)?
        .as_f64()
        .ok_or_else(|| IvyError::new("invalid_arguments", format!("Invalid {name}.")))
}
pub fn truth(value: &IvyJson, name: &str) -> bool {
    value.get(name) == Some(&IvyJson::Bool(true))
}
pub fn optional_text(value: &IvyJson, name: &str) -> Option<String> {
    value.get(name).and_then(|v| v.as_str().ok())
}
pub fn optional_sqlite_key(value: &IvyJson, name: &str) -> Option<String> {
    value.get(name).and_then(|v| sqlite_key(v).ok())
}
pub fn object() -> IvyJson {
    IvyJson::Object(Vec::new())
}
pub fn unresolved(run: &Run) -> bool {
    optional_text(run, "error").is_some_and(|s| s.to_ascii_lowercase().contains("unknown"))
}
pub fn completed(run: &Run) -> bool {
    matches!(
        optional_text(run, "status").as_deref(),
        Some("succeeded" | "failed" | "cancelled")
    ) && !unresolved(run)
}
