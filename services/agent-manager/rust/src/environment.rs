//! Managed environment document retrieval and inheritance. No local output writes.
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString,
    agent_schema::validate_agent,
    hash::{digest, hash_json},
};
use ivy_sdk::rpc::{RequestOptions, RpcClient};

pub const MCP_CONTRACT: &str = "agent/mcp-configuration";
pub const SKILLS_CONTRACT: &str = "agent/skills";
pub const INSTRUCTIONS_CONTRACT: &str = "agent/instructions";

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum EnvironmentKind {
    Mcp,
    Skills,
    Instructions,
}
impl EnvironmentKind {
    fn contract(self) -> &'static str {
        match self {
            Self::Mcp => MCP_CONTRACT,
            Self::Skills => SKILLS_CONTRACT,
            Self::Instructions => INSTRUCTIONS_CONTRACT,
        }
    }
    fn schema(self) -> &'static str {
        match self {
            Self::Mcp => "McpDocument",
            Self::Skills => "SkillsDocument",
            Self::Instructions => "InstructionsDocument",
        }
    }
}

pub fn environment_name(kind: EnvironmentKind, host_id: Option<&JsString>) -> String {
    let base = match kind {
        EnvironmentKind::Mcp => "ivy-agent-mcp",
        EnvironmentKind::Skills => "ivy-agent-skills",
        EnvironmentKind::Instructions => "ivy-agent-instructions",
    };
    match host_id {
        None => base.into(),
        // Node's hash.update(string) uses UTF-8 replacement for isolated surrogates.
        Some(host) => format!(
            "{base}-host-{}",
            &digest(String::from_utf16_lossy(&host.0))[7..]
        ),
    }
}

fn field<'a>(value: &'a IvyJson, key: &str) -> IvyResult<&'a IvyJson> {
    value.get(key).ok_or_else(IvyError::internal)
}

/// Pin the read to the stat revision. Only a missing stat or archived object is
/// absence; a failed pinned read remains an error, never a default-policy fallback.
pub async fn read_environment_document(
    connection: &dyn RpcClient,
    kind: EnvironmentKind,
    host_id: Option<&JsString>,
) -> IvyResult<Option<IvyJson>> {
    let object = match connection
        .request(
            "objects.stat",
            IvyJson::object([(
                "path",
                format!("/{}", environment_name(kind, host_id)).into(),
            )]),
            RequestOptions::default(),
        )
        .await
    {
        Ok(value) => value,
        Err(error) if error.code == "not_found" => return Ok(None),
        Err(error) => return Err(error),
    };
    let instructions = kind == EnvironmentKind::Instructions;
    if object == IvyJson::Null
        || object.get("effectivelyArchived") == Some(&IvyJson::Bool(true))
        || instructions
            && (!js_truthy(&object) || object.get("effectivelyArchived").is_some_and(js_truthy))
    {
        return Ok(None);
    }
    let object_id = field(&object, "id")?;
    let revision = field(&object, "currentRevision")?;
    let value = connection
        .request(
            "objects.read",
            IvyJson::object([
                ("objectId", object_id.clone()),
                ("revision", revision.clone()),
            ]),
            RequestOptions::default(),
        )
        .await?;
    let object_info = field(&value, "object")?;
    let revision_info = field(&value, "revision")?;
    let content = field(&value, "content")?;
    if object_info.get("contractKey") != Some(&IvyJson::from(kind.contract()))
        || revision_info.get("contractVersion") != Some(&IvyJson::from("1.0.0"))
        || content.get("encoding") != Some(&IvyJson::from("json"))
    {
        return Err(IvyError::new(
            "contract_mismatch",
            if instructions {
                "Instructions path contains a different document contract."
            } else {
                "Managed environment path contains a different document contract."
            },
        ));
    }
    let document = field(content, "value")?;
    validate_agent(kind.schema(), document)?;
    let scope = host_id
        .cloned()
        .map(IvyJson::String)
        .unwrap_or(IvyJson::Null);
    if document.get("hostId") != Some(&scope) {
        return Err(IvyError::new(
            "target_conflict",
            if instructions {
                "Instruction document belongs to another scope."
            } else {
                "Managed environment document belongs to another scope."
            },
        ));
    }
    Ok(Some(IvyJson::object([
        ("document", document.clone()),
        ("objectId", object_id.clone()),
        ("revision", revision.clone()),
        ("contentHash", field(revision_info, "contentHash")?.clone()),
    ])))
}

fn js_truthy(value: &IvyJson) -> bool {
    match value {
        IvyJson::Null | IvyJson::Bool(false) => false,
        IvyJson::Number(value) => *value != 0.0 && !value.is_nan(),
        IvyJson::String(value) => !value.0.is_empty(),
        _ => true,
    }
}

pub struct SelectedEnvironment {
    pub document: IvyJson,
    pub version: String,
}

/// Inputs are validated documents/envelopes. Inheritance changes the selected
/// document but retains every source, including inactive revisions, in its hash.
pub fn select_environment_document(
    defaults: &IvyJson,
    global: Option<&IvyJson>,
    host: Option<&IvyJson>,
) -> IvyResult<SelectedEnvironment> {
    let selected = |source: Option<&IvyJson>| -> IvyResult<Option<IvyJson>> {
        match source {
            None => Ok(None),
            Some(value) => {
                let document = field(value, "document")?;
                Ok((document.get("enabled") != Some(&IvyJson::Null)).then(|| document.clone()))
            }
        }
    };
    let inherited = selected(global)?.unwrap_or_else(|| defaults.clone());
    let document = selected(host)?.unwrap_or(inherited);
    let version = hash_json(&IvyJson::object([
        ("defaults", defaults.clone()),
        ("global", global.cloned().unwrap_or(IvyJson::Null)),
        ("host", host.cloned().unwrap_or(IvyJson::Null)),
    ]))?;
    Ok(SelectedEnvironment { document, version })
}
