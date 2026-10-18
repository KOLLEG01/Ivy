use crate::{IvyJson, IvyResult, JsString, canonical_bundle::Bundle};
use std::sync::OnceLock;

/// A self-contained native schema, without loading host or product-domain schemas.
pub fn bundled_schema(reference: &JsString, document: Option<&JsString>) -> IvyResult<IvyJson> {
    static BUNDLE: OnceLock<Bundle> = OnceLock::new();
    BUNDLE
        .get_or_init(|| {
            Bundle::new(
                &[
                    (
                        "Agent",
                        include_str!("../../../../specs/schemas/agent.schema.json"),
                    ),
                    (
                        "Wire",
                        include_str!("../../../../specs/schemas/hive-wire.schema.json"),
                    ),
                ],
                "native",
            )
        })
        .bundle(reference, document, 0)
}
