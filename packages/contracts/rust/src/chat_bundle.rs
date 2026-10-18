use crate::{IvyJson, IvyResult, JsString, canonical_bundle::Bundle};
use std::sync::OnceLock;

/// A self-contained ChatBridge schema with its native and wire references.
pub fn bundled_schema(reference: &JsString, document: Option<&JsString>) -> IvyResult<IvyJson> {
    static BUNDLE: OnceLock<Bundle> = OnceLock::new();
    BUNDLE
        .get_or_init(|| {
            Bundle::new(
                &[
                    (
                        "Chat",
                        include_str!("../../../../specs/schemas/chat.schema.json"),
                    ),
                    (
                        "Agent",
                        include_str!("../../../../specs/schemas/agent.schema.json"),
                    ),
                    (
                        "Wire",
                        include_str!("../../../../specs/schemas/hive-wire.schema.json"),
                    ),
                ],
                "canonical",
            )
        })
        .bundle(reference, document, 0)
}
