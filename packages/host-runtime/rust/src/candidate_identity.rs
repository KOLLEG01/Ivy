use crate::{IvyJson, IvyResult};
use ivy_contracts::{JsString, hash::hash_json};

/// One logical release for exact source bytes and the universal package recipe.
pub fn candidate_build_id(component_id: &JsString, version: &JsString, source_identity: &JsString) -> IvyResult<String> {
    hash_json(&IvyJson::object([
        ("schemaVersion", IvyJson::Number(3.0)), ("packageRecipe", IvyJson::Number(1.0)),
        ("componentId", IvyJson::String(component_id.clone())), ("version", IvyJson::String(version.clone())),
        ("sourceIdentity", IvyJson::String(source_identity.clone())),
    ]))
}
