//! Schemas for the dormant Outlook and Teams collectors' local workflow graph.
use ivy_contracts::{
    IvyError, IvyJson, IvyResult,
    builtin_schema::SchemaBundle,
    json::{canonical, parse},
    schema::bundle_local_schema,
};
use std::sync::OnceLock;

const MICROSOFT: &str = include_str!("../../../../specs/schemas/secretary-microsoft.schema.json");
const TEAMS: &str = include_str!("../../../../specs/schemas/secretary-teams.schema.json");
pub const MICROSOFT_METHODS: [&str; 4] = [
    "account/read",
    "thread/start",
    "mcpServer/tool/call",
    "thread/unsubscribe",
];

#[derive(Clone, Copy, Debug)]
pub enum MicrosoftRecord {
    OutlookHead,
    OutlookPlan,
    OutlookCall,
    OutlookEvidence,
    OutlookPublication,
    TeamsHead,
    TeamsPlan,
    TeamsCall,
    TeamsEvidence,
    TeamsPublication,
}
impl MicrosoftRecord {
    pub fn key(self) -> &'static str {
        match self {
            Self::OutlookHead => "secretary/outlook-head",
            Self::OutlookPlan => "secretary/outlook-plan",
            Self::OutlookCall => "secretary/outlook-call",
            Self::OutlookEvidence => "secretary/outlook-evidence",
            Self::OutlookPublication => "secretary/outlook-publication",
            Self::TeamsHead => "secretary/teams-head",
            Self::TeamsPlan => "secretary/teams-plan",
            Self::TeamsCall => "secretary/teams-call",
            Self::TeamsEvidence => "secretary/teams-evidence",
            Self::TeamsPublication => "secretary/teams-publication",
        }
    }
    pub fn schema(self) -> &'static str {
        match self {
            Self::OutlookHead => "OutlookHead",
            Self::OutlookPlan => "OutlookPlan",
            Self::OutlookCall => "OutlookCall",
            Self::OutlookEvidence => "OutlookEvidence",
            Self::OutlookPublication => "OutlookPublication",
            Self::TeamsHead => "TeamsHead",
            Self::TeamsPlan => "TeamsPlan",
            Self::TeamsCall => "TeamsCall",
            Self::TeamsEvidence => "TeamsEvidence",
            Self::TeamsPublication => "TeamsPublication",
        }
    }
}
fn document(name: &str) -> IvyResult<&'static IvyJson> {
    static MICROSOFT_VALUE: OnceLock<IvyResult<IvyJson>> = OnceLock::new();
    static TEAMS_VALUE: OnceLock<IvyResult<IvyJson>> = OnceLock::new();
    let (cell, source) = if name.starts_with("Teams") {
        (&TEAMS_VALUE, TEAMS)
    } else {
        (&MICROSOFT_VALUE, MICROSOFT)
    };
    cell.get_or_init(|| parse(source, 1024 * 1024))
        .as_ref()
        .map_err(Clone::clone)
}
pub fn bundled(name: &str) -> IvyResult<IvyJson> {
    bundle_local_schema(document(name)?, &name.into())
}
pub fn validate_microsoft(name: &str, value: &IvyJson) -> IvyResult<()> {
    canonical(value, 1024 * 1024)?;
    // Preserve missing-definition admission as well as the original Teams routing.
    if document(name)?.required("$defs")?.get(name).is_none() {
        bundled(name)?;
    }
    static MICROSOFT_BUNDLE: OnceLock<IvyResult<SchemaBundle>> = OnceLock::new();
    static TEAMS_BUNDLE: OnceLock<IvyResult<SchemaBundle>> = OnceLock::new();
    let bundle = if name.starts_with("Teams") {
        TEAMS_BUNDLE.get_or_init(|| SchemaBundle::new(&[TEAMS]))
    } else {
        MICROSOFT_BUNDLE.get_or_init(|| SchemaBundle::new(&[MICROSOFT]))
    }
    .as_ref()
    .map_err(Clone::clone)?;
    if bundle.matches(&format!("#/$defs/{name}"), value)? {
        Ok(())
    } else {
        Err(IvyError::new(
            "invalid_arguments",
            "Content does not satisfy its exact contract.",
        ))
    }
}
pub fn microsoft_registry() -> IvyJson {
    IvyJson::object([
        ("namespaces", IvyJson::Array(vec![])),
        ("contracts", IvyJson::Array(vec![])),
        ("requiredContracts", IvyJson::Array(vec![])),
    ])
}
