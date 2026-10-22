use crate::{IvyJson, IvyResult};
use ivy_contracts::{JsString, hash::hash_json};

/// Retain existing IDs for plans whose artifact does not select a native platform.
pub fn candidate_build_id(
    component_id: &JsString,
    version: &JsString,
    source_identity: &JsString,
) -> IvyResult<String> {
    candidate_build_id_for_platform(component_id, version, source_identity, None)
}
pub fn candidate_build_id_for_platform(
    component_id: &JsString,
    version: &JsString,
    source_identity: &JsString,
    native_platform: Option<&JsString>,
) -> IvyResult<String> {
    let mut recipe = IvyJson::object([
        ("schemaVersion", IvyJson::Number(3.0)),
        ("packageRecipe", IvyJson::Number(1.0)),
        ("componentId", IvyJson::String(component_id.clone())),
        ("version", IvyJson::String(version.clone())),
        ("sourceIdentity", IvyJson::String(source_identity.clone())),
    ]);
    if let Some(platform) = native_platform {
        recipe.insert("nativePlatform", IvyJson::String(platform.clone()));
    }
    hash_json(&recipe)
}
