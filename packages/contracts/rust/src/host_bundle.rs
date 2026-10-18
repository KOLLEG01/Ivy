use crate::{IvyJson, IvyResult, JsString, canonical_bundle::Bundle};
use std::sync::OnceLock;

/// A self-contained host schema, without loading agent or product-domain schemas.
pub fn bundled_schema(reference: &JsString, document: Option<&JsString>) -> IvyResult<IvyJson> {
    static BUNDLE: OnceLock<Bundle> = OnceLock::new();
    BUNDLE
        .get_or_init(|| {
            Bundle::new(
                &[
                    (
                        "Host",
                        include_str!("../../../../specs/schemas/host.schema.json"),
                    ),
                    (
                        "Wire",
                        include_str!("../../../../specs/schemas/hive-wire.schema.json"),
                    ),
                ],
                "host",
            )
        })
        .bundle(reference, document, 0)
}
