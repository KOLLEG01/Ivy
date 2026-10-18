use crate::{
    IvyError, IvyJson, IvyResult, builtin_schema::SchemaBundle, json::canonical,
    limits::MANAGEMENT_FRAME_BYTES,
};
use std::sync::OnceLock;

pub fn validate_automation(name: &str, value: &IvyJson) -> IvyResult<()> {
    canonical(value, MANAGEMENT_FRAME_BYTES)?;
    static BUNDLE: OnceLock<IvyResult<SchemaBundle>> = OnceLock::new();
    let bundle = BUNDLE
        .get_or_init(|| {
            SchemaBundle::new(&[
                include_str!("../../../../specs/schemas/automation.schema.json"),
                include_str!("../../../../specs/schemas/hive-operations.schema.json"),
                include_str!("../../../../specs/schemas/host.schema.json"),
                include_str!("../../../../specs/schemas/hive-wire.schema.json"),
            ])
        })
        .as_ref()
        .map_err(Clone::clone)?;
    if bundle
        .matches_named(
            "https://ivy.invalid/schemas/automation.schema.json",
            name,
            value,
        )?
        .unwrap_or(false)
    {
        Ok(())
    } else {
        Err(IvyError::new(
            "invalid_arguments",
            "Value does not match its automation example contract.",
        ))
    }
}
