use crate::{IvyError, IvyJson, IvyResult, JsString, canonical_bundle::Bundle};
use std::sync::OnceLock;

/// A self-contained core schema, without loading service-domain schemas.
pub fn bundled_schema(reference: &JsString, document: Option<&JsString>) -> IvyResult<IvyJson> {
    static BUNDLE: OnceLock<Bundle> = OnceLock::new();
    BUNDLE
        .get_or_init(|| {
            Bundle::new(
                &[
                    (
                        "Wire",
                        include_str!("../../../../specs/schemas/hive-wire.schema.json"),
                    ),
                    (
                        "Operation",
                        include_str!("../../../../specs/schemas/hive-operations.schema.json"),
                    ),
                    (
                        "Host",
                        include_str!("../../../../specs/schemas/host.schema.json"),
                    ),
                ],
                "canonical",
            )
        })
        .bundle(reference, document, 1)
}
pub fn operation_input_schema(method: &str) -> IvyResult<IvyJson> {
    let reference = crate::core_schema::operation(method, false)
        .ok_or_else(|| IvyError::new("not_found", "Unknown canonical operation."))?;
    let schema = bundled_schema(&reference.into(), None)?;
    if schema.get("type") != Some(&IvyJson::from("object")) {
        return Err(IvyError::new(
            "internal_error",
            "MCP operation input must be an object.",
        ));
    }
    Ok(schema)
}
