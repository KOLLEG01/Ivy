//! Authoritative MCP preparation before acquiring the output filesystem lock.
use crate::{
    environment::{
        EnvironmentKind, SelectedEnvironment, read_environment_document,
        select_environment_document,
    },
    mcp_output::{McpOutput, McpOutputInput, plan_mcp_output},
    mcp_render::{hive_mcp_urls, render_mcp_sections},
};
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString, agent_schema::validate_agent,
    core_schema::validate_output, hash::hash_json,
};
use ivy_sdk::rpc::{RequestOptions, RpcClient};

pub struct PreparedMcp {
    pub sections: Option<Vec<String>>,
    pub server_names: Vec<JsString>,
    pub app_id: Option<String>,
    pub authority: String,
    pub desired_version: String,
}
impl PreparedMcp {
    /// Call after reading the current bytes/state under the output lock. No
    /// state cached during preparation can authorize overwriting later edits.
    pub fn plan(&self, source: &str, state: Option<&IvyJson>) -> IvyResult<McpOutput> {
        plan_mcp_output(McpOutputInput {
            source,
            state,
            authority: &self.authority,
            sections: self.sections.as_deref(),
            server_names: &self.server_names,
            app_id: self.app_id.as_deref(),
            desired_version: &self.desired_version,
        })
    }
}

/// Defaults and configured identities have already passed their configuration
/// schemas. Preserve the original read order and failure boundaries, including
/// the OpenAI request and URL validation when the selected document is disabled.
pub async fn prepare_mcp(
    connection: &dyn RpcClient,
    defaults: &IvyJson,
    host_id: &JsString,
    hive_url: &JsString,
    credential: &JsString,
    credentials: &crate::mcp_credentials::McpCredentials,
) -> IvyResult<PreparedMcp> {
    let global = read_environment_document(connection, EnvironmentKind::Mcp, None).await?;
    let host = read_environment_document(connection, EnvironmentKind::Mcp, Some(host_id)).await?;
    let desired = select_environment_document(defaults, global.as_ref(), host.as_ref())?;
    let urls = hive_mcp_urls(hive_url)?;
    let openai = connection
        .request(
            "openai.getConfiguration",
            IvyJson::object([] as [(&str, IvyJson); 0]),
            RequestOptions::default(),
        )
        .await?;
    assemble_mcp(
        desired,
        openai,
        urls,
        host_id,
        hive_url,
        credential,
        credentials,
    )
}

/// Explicit import uses an operator-supplied selected document and OpenAI
/// snapshot. Credentials enter through protected input, never command arguments.
pub fn prepare_mcp_import_snapshot(
    document: &IvyJson,
    openai: &IvyJson,
    host_id: &JsString,
    hive_url: &JsString,
    credential: &JsString,
) -> IvyResult<PreparedMcp> {
    validate_agent("McpDocument", document)?;
    if document.get("hostId") != Some(&IvyJson::Null)
        && document.get("hostId") != Some(&IvyJson::String(host_id.clone()))
    {
        return Err(IvyError::new(
            "target_conflict",
            "Managed environment document belongs to another scope.",
        ));
    }
    if !matches!(document.get("enabled"), Some(IvyJson::Bool(_))) {
        return Err(IvyError::new(
            "invalid_arguments",
            "Explicit MCP import requires a selected enabled or disabled document.",
        ));
    }
    let desired = select_environment_document(document, None, None)?;
    let urls = hive_mcp_urls(hive_url)?;
    assemble_mcp(
        desired,
        openai.clone(),
        urls,
        host_id,
        hive_url,
        credential,
        &Default::default(),
    )
}

fn assemble_mcp(
    desired: SelectedEnvironment,
    openai: IvyJson,
    urls: std::collections::HashSet<String>,
    host_id: &JsString,
    hive_url: &JsString,
    credential: &JsString,
    credentials: &crate::mcp_credentials::McpCredentials,
) -> IvyResult<PreparedMcp> {
    validate_output("openai.getConfiguration", &openai)?;
    let app_id = openai
        .get("configuration")
        .and_then(|v| v.get("appId"))
        .and_then(IvyJson::as_js_string);
    let sections = render_mcp_sections(&desired.document, &urls, credential, app_id, credentials)?;
    let desired_version = hash_json(&IvyJson::object([
        ("document", desired.version.into()),
        ("approvalPolicy", "never".into()),
        ("defaultPermissions", ":danger-full-access".into()),
        ("openai", openai.clone()),
        (
            "credentials",
            crate::mcp_credentials::credentials_hash(credentials)?.into(),
        ),
    ]))?;
    let server_names = desired
        .document
        .get("servers")
        .and_then(IvyJson::as_array)
        .ok_or_else(IvyError::internal)?
        .iter()
        .map(|server| {
            server
                .get("name")
                .and_then(IvyJson::as_js_string)
                .cloned()
                .ok_or_else(IvyError::internal)
        })
        .collect::<IvyResult<_>>()?;
    Ok(PreparedMcp {
        sections,
        server_names,
        app_id: app_id.map(JsString::to_utf8).transpose()?,
        authority: hash_json(&IvyJson::object([
            ("hive", IvyJson::String(hive_url.clone())),
            ("hostId", IvyJson::String(host_id.clone())),
        ]))?,
        desired_version,
    })
}
