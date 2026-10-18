use crate::{
    IvyError, IvyJson, IvyResult, builtin_schema::SchemaBundle, json::canonical,
    limits::MANAGEMENT_FRAME_BYTES,
};
use std::sync::OnceLock;

static CORE: OnceLock<IvyResult<SchemaBundle>> = OnceLock::new();
static HOST: OnceLock<IvyResult<SchemaBundle>> = OnceLock::new();
fn core() -> IvyResult<&'static SchemaBundle> {
    CORE.get_or_init(|| {
        SchemaBundle::new(&[
            include_str!("../../../../specs/schemas/hive-wire.schema.json"),
            include_str!("../../../../specs/schemas/hive-operations.schema.json"),
            include_str!("../../../../specs/schemas/hive-transport.schema.json"),
            include_str!("../../../../specs/schemas/host.schema.json"),
        ])
    })
    .as_ref()
    .map_err(Clone::clone)
}
pub(crate) fn operation(method: &str, output: bool) -> Option<String> {
    static CATALOG: OnceLock<serde_json::Value> = OnceLock::new();
    let catalog = CATALOG.get_or_init(|| {
        serde_json::from_str(include_str!("../../../../specs/schemas/operations.json"))
            .expect("checked-in operation catalog")
    });
    catalog["operations"]
        .get(method)
        .and_then(|definition| definition[if output { "output" } else { "input" }].as_str())
        .map(|fragment| {
            format!("https://ivy.invalid/schemas/hive-operations.schema.json{fragment}")
        })
}
pub fn validate_input(method: &str, value: &IvyJson) -> IvyResult<()> {
    let reference = operation(method, false)
        .ok_or_else(|| IvyError::new("not_found", "Unknown Hive operation."))?;
    if core()?.matches(&reference, value)? {
        Ok(())
    } else {
        Err(IvyError::new(
            "invalid_arguments",
            "Arguments do not match the operation contract.",
        ))
    }
}
pub fn validate_output(method: &str, value: &IvyJson) -> IvyResult<()> {
    if let Some(reference) = operation(method, true) {
        if core()?.matches(&reference, value)? {
            return Ok(());
        }
    }
    Err(IvyError::new(
        "internal_error",
        "Result does not match the operation contract.",
    ))
}
pub fn validate_transport(name: &str, value: &IvyJson) -> IvyResult<()> {
    if core()?.matches(
        &format!("https://ivy.invalid/schemas/hive-transport.schema.json#/$defs/{name}"),
        value,
    )? {
        Ok(())
    } else {
        Err(IvyError::new(
            "invalid_frame",
            "Invalid provider transport frame.",
        ))
    }
}
pub fn validate_host(name: &str, value: &IvyJson) -> IvyResult<()> {
    canonical(value, MANAGEMENT_FRAME_BYTES)?;
    let bundle = HOST
        .get_or_init(|| {
            SchemaBundle::new(&[
                include_str!("../../../../specs/schemas/hive-wire.schema.json"),
                include_str!("../../../../specs/schemas/host.schema.json"),
            ])
        })
        .as_ref()
        .map_err(Clone::clone)?;
    if bundle
        .matches(
            &format!("https://ivy.invalid/schemas/host.schema.json#/$defs/{name}"),
            value,
        )
        .unwrap_or(false)
    {
        Ok(())
    } else {
        Err(IvyError::new(
            "invalid_arguments",
            "Value does not match its host configuration/operation contract.",
        ))
    }
}
pub fn is_ui_release(value: &IvyJson) -> IvyResult<bool> {
    core()?.matches(
        "https://ivy.invalid/schemas/hive-operations.schema.json#/$defs/UiRelease",
        value,
    )
}
pub fn is_ui_metadata(value: &IvyJson) -> IvyResult<bool> {
    core()?.matches(
        "https://ivy.invalid/schemas/hive-operations.schema.json#/$defs/UiMetadata",
        value,
    )
}
pub fn validate_ui_definition(value: &IvyJson) -> IvyResult<()> {
    if core()?.matches(
        "https://ivy.invalid/schemas/hive-operations.schema.json#/$defs/UiDefinition",
        value,
    )? {
        Ok(())
    } else {
        Err(IvyError::new(
            "invalid_arguments",
            "Invalid static ui definition.",
        ))
    }
}
