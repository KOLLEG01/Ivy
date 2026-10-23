//! Explicit offline imports. Preparation never reads or changes the filesystem.
//! The operator selects stopped source owners; service startup never calls this.
use crate::{
    mcp_import::import_mcp,
    mcp_prepare::{PreparedMcp, prepare_mcp_import_snapshot},
    skills_files::SkillsComparator,
    skills_migration::import_skills,
};
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString, hash::hash_json};
use ivy_host_runtime::node_path::{self, Style};
use std::{path::PathBuf, sync::Arc};

const MCP_FIELDS: &[&str] = &[
    "home",
    "hostId",
    "hiveUrl",
    "credential",
    "document",
    "openai",
];
const SKILLS_FIELDS: &[&str] = &["sourceState", "root", "hostId", "hiveUrl"];

fn invalid(message: &str) -> IvyError {
    IvyError::new("invalid_arguments", message)
}
fn exact_fields(request: &IvyJson, names: &[&str], tagged: bool, message: &str) -> IvyResult<()> {
    let fields = request.as_object().ok_or_else(|| invalid(message))?;
    if fields.len() != names.len() + usize::from(tagged)
        || fields.iter().any(|(key, _)| {
            !(tagged && key == &JsString::from("kind"))
                && !names.iter().any(|name| key == &JsString::from(*name))
        })
    {
        return Err(invalid(message));
    }
    Ok(())
}
// InstanceConfig's existing string bounds count code points, not UTF-16 units.
fn string(request: &IvyJson, name: &str, maximum: usize, message: &str) -> IvyResult<JsString> {
    request
        .get(name)
        .and_then(IvyJson::as_js_string)
        .filter(|value| {
            !value.0.is_empty() && String::from_utf16_lossy(&value.0).chars().count() <= maximum
        })
        .cloned()
        .ok_or_else(|| invalid(message))
}
fn absolute(path: JsString, message: &str) -> IvyResult<JsString> {
    if !node_path::is_absolute(Style::native(), &path) {
        return Err(invalid(message));
    }
    Ok(path)
}

/// Contains the selected snapshot, including its credential; deliberately no Debug.
pub struct McpImport {
    home: PathBuf,
    prepared: PreparedMcp,
}
impl McpImport {
    fn prepare(request: &IvyJson, tagged: bool) -> IvyResult<Self> {
        let message =
            "MCP import requires home, hostId, hiveUrl, credential, document and openai on stdin.";
        exact_fields(request, MCP_FIELDS, tagged, message)?;
        let home = absolute(
            string(request, "home", 2048, message)?,
            "MCP import home must be absolute.",
        )?;
        let prepared = prepare_mcp_import_snapshot(
            request.required("document")?,
            request.required("openai")?,
            &string(request, "hostId", 128, message)?,
            &string(request, "hiveUrl", 2048, message)?,
            &string(request, "credential", 4096, message)?,
        )?;
        Ok(Self {
            home: node_path::filesystem_path(&home),
            prepared,
        })
    }
    /// Preserve the original six-field stdin boundary used by ivy-agent-mcp-import.
    pub fn from_snapshot(request: &IvyJson) -> IvyResult<Self> {
        Self::prepare(request, false)
    }
    pub async fn run(self) -> IvyResult<IvyJson> {
        let imported = import_mcp(self.home, self.prepared).await?;
        Ok(IvyJson::object([
            (
                "target",
                IvyJson::String(node_path::from_native(&imported.target)),
            ),
            (
                "statePath",
                IvyJson::String(node_path::from_native(&imported.state_path)),
            ),
            (
                "sourceBackup",
                imported
                    .source_backup
                    .as_ref()
                    .map(|path| IvyJson::String(node_path::from_native(path)))
                    .unwrap_or(IvyJson::Null),
            ),
            ("changed", imported.changed.into()),
        ]))
    }
}

pub struct SkillsImport {
    source_state: JsString,
    root: JsString,
    authority: String,
}
impl SkillsImport {
    fn prepare(request: &IvyJson) -> IvyResult<Self> {
        let message = "Skills import requires kind, sourceState, root, hostId and hiveUrl.";
        exact_fields(request, SKILLS_FIELDS, true, message)?;
        let source_state = absolute(
            string(request, "sourceState", 2048, message)?,
            "Skills import source state must be absolute.",
        )?;
        let root = absolute(
            string(request, "root", 2048, message)?,
            "Managed skills root must be absolute.",
        )?;
        let authority = hash_json(&IvyJson::object([
            (
                "hive",
                IvyJson::String(string(request, "hiveUrl", 2048, message)?),
            ),
            (
                "hostId",
                IvyJson::String(string(request, "hostId", 128, message)?),
            ),
        ]))?;
        Ok(Self {
            source_state,
            root,
            authority,
        })
    }
    pub async fn run(self, compare: Arc<SkillsComparator>) -> IvyResult<IvyJson> {
        let imported = import_skills(self.source_state, self.root, self.authority, compare).await?;
        Ok(IvyJson::object([
            ("statePath", IvyJson::String(imported.state_path)),
            (
                "sourceBackup",
                imported
                    .source_backup
                    .map(IvyJson::String)
                    .unwrap_or(IvyJson::Null),
            ),
            (
                "copiedSkills",
                IvyJson::Number(imported.copied_skills as f64),
            ),
            ("unchanged", imported.unchanged.into()),
        ]))
    }
}

/// A single selected operation, never a partly completed multi-import transaction.
pub enum OfflineImport {
    Mcp(McpImport),
    Skills(SkillsImport),
}
impl OfflineImport {
    pub fn from_snapshot(request: &IvyJson) -> IvyResult<Self> {
        match request.get("kind").and_then(IvyJson::as_js_string) {
            Some(kind) if kind == &JsString::from("mcp") => {
                McpImport::prepare(request, true).map(Self::Mcp)
            }
            Some(kind) if kind == &JsString::from("skills") => {
                SkillsImport::prepare(request).map(Self::Skills)
            }
            _ => Err(invalid("Offline import kind must be mcp or skills.")),
        }
    }
    /// The production entry supplies the native display comparator. There
    /// is no optional/default ordering, service launch, network read or retry.
    pub async fn run(self, compare: Arc<SkillsComparator>) -> IvyResult<IvyJson> {
        match self {
            Self::Mcp(import) => import.run().await,
            Self::Skills(import) => import.run(compare).await,
        }
    }
}
