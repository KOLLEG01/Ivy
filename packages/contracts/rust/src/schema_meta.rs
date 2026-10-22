//! JSON Schema meta-validation against the packaged standard documents.
use crate::{IvyError, IvyJson, IvyResult, builtin_schema::SchemaBundle};
use std::sync::OnceLock;

const DEFAULT: &str = "https://json-schema.org/draft/2020-12/schema";
static BUNDLE: OnceLock<IvyResult<SchemaBundle>> = OnceLock::new();

fn invalid() -> IvyError {
    IvyError::new("registry_invalid", "Invalid or unsupported JSON schema.")
}

pub(super) fn validate(schema: &IvyJson) -> IvyResult<()> {
    if matches!(schema, IvyJson::Bool(_)) {
        return Ok(());
    }
    let selected = schema
        .get("$schema")
        .map(IvyJson::as_str)
        .transpose()
        .map_err(|_| invalid())?;
    let selected = selected.as_deref().unwrap_or(DEFAULT);
    let selected = if selected == "http://json-schema.org/schema" {
        DEFAULT
    } else {
        selected
    };
    let bundle = BUNDLE
        .get_or_init(|| SchemaBundle::from_documents(super::resources::meta_documents()?.to_vec()))
        .as_ref()
        .map_err(Clone::clone)?;
    if bundle.matches(selected, schema).map_err(|_| invalid())? {
        Ok(())
    } else {
        Err(invalid())
    }
}
