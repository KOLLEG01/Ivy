//! Trusted schemas for the retained dormant triage graph. Validation does not
//! register these families with Hive or start automatic triage work.
use ivy_contracts::{
    IvyError, IvyJson, IvyResult,
    builtin_schema::SchemaBundle,
    json::{canonical, parse},
    schema::bundle_local_schema,
};
use std::sync::OnceLock;

const DOCUMENT: &str = include_str!("../../../../specs/schemas/secretary-triage.schema.json");
#[derive(Clone, Copy)]
pub enum TriageSchema {
    Settings,
    Input,
    MediaAttachment,
    Media,
    Binding,
    Plan,
    Thread,
    Work,
    Call,
    Completion,
}
impl TriageSchema {
    pub fn name(self) -> &'static str {
        match self {
            Self::Settings => "TriageSettings",
            Self::Input => "TriageInput",
            Self::MediaAttachment => "TriageMediaAttachment",
            Self::Media => "TriageMedia",
            Self::Binding => "TriageBinding",
            Self::Plan => "TriagePlan",
            Self::Thread => "TriageThread",
            Self::Work => "TriageWork",
            Self::Call => "TriageCall",
            Self::Completion => "TriageCompletion",
        }
    }
}
pub fn bundled(schema: TriageSchema) -> IvyResult<IvyJson> {
    static VALUE: OnceLock<IvyResult<IvyJson>> = OnceLock::new();
    let document = VALUE
        .get_or_init(|| parse(DOCUMENT, 1024 * 1024))
        .as_ref()
        .map_err(Clone::clone)?;
    bundle_local_schema(document, &schema.name().into())
}
pub fn validate(schema: TriageSchema, value: &IvyJson) -> IvyResult<()> {
    canonical(value, 1024 * 1024)?;
    static BUNDLE: OnceLock<IvyResult<SchemaBundle>> = OnceLock::new();
    let bundle = BUNDLE
        .get_or_init(|| SchemaBundle::new(&[DOCUMENT]))
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
pub(crate) fn check(condition: bool, code: &str) -> IvyResult<()> {
    if condition {
        Ok(())
    } else {
        Err(IvyError::new(
            code,
            "Triage requires its original item, owner, native intent and complete evidence.",
        ))
    }
}
