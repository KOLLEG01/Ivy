//! Service-local schema admission uses the original SchemaValidators defaults.
use ivy_contracts::{IvyError, IvyJson, IvyResult, builtin_schema::SchemaBundle, json::canonical};
use std::sync::OnceLock;

#[derive(Clone, Copy)]
pub enum Schema {
    Assessment,
    GlobalRules,
    AssignmentTrigger,
    Assignment,
    ExecutionTarget,
    SecretaryConfiguration,
}
impl Schema {
    fn name(self) -> &'static str {
        match self {
            Self::Assessment => "Assessment",
            Self::GlobalRules => "GlobalRules",
            Self::AssignmentTrigger => "AssignmentTrigger",
            Self::Assignment => "Assignment",
            Self::ExecutionTarget => "ExecutionTarget",
            Self::SecretaryConfiguration => "SecretaryConfiguration",
        }
    }
}

pub fn validate(schema: Schema, value: &IvyJson) -> IvyResult<()> {
    canonical(value, 1024 * 1024)?;
    static BUNDLE: OnceLock<IvyResult<SchemaBundle>> = OnceLock::new();
    let bundle = BUNDLE
        .get_or_init(|| {
            SchemaBundle::new(&[include_str!(
                "../../../../specs/schemas/secretary.schema.json"
            )])
        })
        .as_ref()
        .map_err(Clone::clone)?;
    if bundle.matches(&format!("#/$defs/{}", schema.name()), value)? {
        Ok(())
    } else {
        Err(IvyError::new(
            "invalid_arguments",
            "Content does not satisfy its exact contract.",
        ))
    }
}
