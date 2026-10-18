use crate::{IvyError, IvyJson, IvyResult, builtin_schema::SchemaBundle};
use std::sync::OnceLock;

pub const WIRE_ID: &str = "https://ivy.invalid/schemas/hive-wire.schema.json";
static WIRE: OnceLock<IvyResult<SchemaBundle>> = OnceLock::new();
pub fn bundle() -> IvyResult<&'static SchemaBundle> {
    WIRE.get_or_init(|| {
        SchemaBundle::new(&[include_str!(
            "../../../../specs/schemas/hive-wire.schema.json"
        )])
    })
    .as_ref()
    .map_err(Clone::clone)
}
pub fn matches_shared(name: &str, value: &IvyJson) -> IvyResult<bool> {
    bundle()?
        .matches_named(WIRE_ID, name, value)?
        .ok_or_else(IvyError::internal)
}
pub fn validate_shared(name: &str, value: &IvyJson) -> IvyResult<()> {
    if matches_shared(name, value)? {
        Ok(())
    } else {
        Err(IvyError::new(
            "invalid_arguments",
            "Value does not match its shared contract.",
        ))
    }
}
