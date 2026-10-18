//! Structural validation against the retained Agent schema, independent of native catalogs.
use crate::{IvyError, IvyJson, IvyResult, builtin_schema::SchemaBundle};
use std::sync::OnceLock;

#[derive(Clone, Copy)]
pub enum NativeFrameKind {
    RequestId,
    Reply,
}

/// Native JSONL has already been decoded and bounded by its transport.
pub fn validate_agent_frame(value: &IvyJson, kind: NativeFrameKind) -> IvyResult<()> {
    use crate::validation::{safe_integer, text};
    let invalid = |message| IvyError::new("native_invalid_frame", message);
    if matches!(kind, NativeFrameKind::RequestId) {
        return if text(Some(value), 256) || safe_integer(Some(value), -9_007_199_254_740_991.0) {
            Ok(())
        } else {
            Err(invalid("Native request ID is invalid."))
        };
    }
    if value.as_object().is_none() {
        return Err(invalid("Native reply must be an object."));
    }
    if value.get("result").is_some() == value.get("error").is_some() {
        return Err(invalid(
            "Native reply requires exactly one result or error.",
        ));
    }
    if let Some(error) = value.get("error") {
        if error.as_object().is_none() {
            return Err(invalid("Native error must be an object."));
        }
        if !safe_integer(error.get("code"), -9_007_199_254_740_991.0)
            || error
                .get("message")
                .and_then(IvyJson::as_js_string)
                .is_none()
        {
            return Err(invalid("Native error envelope is incomplete."));
        }
    }
    Ok(())
}

pub fn validate_agent(name: &str, value: &IvyJson) -> IvyResult<()> {
    static BUNDLE: OnceLock<IvyResult<SchemaBundle>> = OnceLock::new();
    let bundle = BUNDLE
        .get_or_init(|| {
            SchemaBundle::new(&[
                include_str!("../../../../specs/schemas/hive-wire.schema.json"),
                include_str!("../../../../specs/schemas/agent.schema.json"),
            ])
        })
        .as_ref()
        .map_err(Clone::clone)?;
    if bundle
        .matches(
            &format!("https://ivy.invalid/schemas/agent.schema.json#/$defs/{name}"),
            value,
        )
        .unwrap_or(false)
    {
        Ok(())
    } else {
        Err(IvyError::new(
            "invalid_arguments",
            "Value does not match its native projection/AgentManager contract.",
        ))
    }
}
