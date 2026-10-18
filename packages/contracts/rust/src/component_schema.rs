use crate::{IvyError, IvyJson, IvyResult, builtin_schema::SchemaBundle};
use std::sync::OnceLock;

pub fn validate_component(value: &IvyJson) -> IvyResult<()> {
    static BUNDLE: OnceLock<IvyResult<SchemaBundle>> = OnceLock::new();
    let bundle = BUNDLE
        .get_or_init(|| {
            SchemaBundle::new(&[
                include_str!("../../../../specs/schemas/component.schema.json"),
                include_str!("../../../../specs/schemas/host.schema.json"),
                include_str!("../../../../specs/schemas/hive-wire.schema.json"),
            ])
        })
        .as_ref()
        .map_err(Clone::clone)?;
    if bundle.matches("https://ivy.invalid/schemas/component.schema.json", value)? {
        Ok(())
    } else {
        Err(IvyError::new(
            "invalid_arguments",
            "Invalid component manifest.",
        ))
    }
}
