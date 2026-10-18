use crate::{
    IvyError, IvyJson, IvyResult, JsString,
    native_project_path::{contains, path_key},
    rpc::RequestOptions,
    tools::{InterfaceRequirement, ToolCallOptions, ToolClients},
};
use ivy_contracts::agent_schema::validate_agent;

fn explicit(params: &IvyJson) -> Option<IvyJson> {
    if params.get("projectId") == Some(&IvyJson::from("")) {
        let mut value = params.clone();
        value.insert("projectId", IvyJson::Null);
        return Some(value);
    }
    if params.get("projectId").is_some_and(|v| !v.is_null())
        || params.get("ephemeral") == Some(&IvyJson::Bool(true))
    {
        Some(params.clone())
    } else {
        None
    }
}
/// Choose the default before journaling; an explicit empty ID opts out of membership.
pub fn default_native_thread_project(params: &IvyJson, inventory: &IvyJson) -> IvyResult<IvyJson> {
    if let Some(value) = explicit(params) {
        return Ok(value);
    }
    let internal = inventory
        .get("defaults")
        .and_then(|d| d.get("internalProjectRoot"));
    let cwd=params.get("cwd").filter(|v|!v.is_null()).or(internal).and_then(IvyJson::as_js_string).ok_or_else(||IvyError::new("native_internal_project_missing","A service task requires an internal project directory or an explicit working directory."))?;
    let key = path_key(cwd)?;
    let mut selected = None;
    let mut length = 0;
    for project in inventory
        .required("projects")?
        .as_array()
        .ok_or_else(IvyError::internal)?
    {
        if project.get("source") != Some(&IvyJson::from("native")) {
            continue;
        }
        for path in project
            .required("paths")?
            .as_array()
            .ok_or_else(IvyError::internal)?
        {
            let path = path_key(path.as_js_string().ok_or_else(IvyError::internal)?)?;
            if contains(&path.0, &key.0) && (selected.is_none() || path.0.len() > length) {
                selected = Some(project.required("nativeId")?.clone());
                length = path.0.len();
            }
        }
    }
    let mut result = params.clone();
    if let Some(id) = selected {
        result.insert("cwd", IvyJson::String(cwd.clone()));
        result.insert("projectId", id);
        return Ok(result);
    }
    if let Some(root) = internal
        .and_then(IvyJson::as_js_string)
        .filter(|s| !s.0.is_empty())
    {
        if contains(&path_key(root)?.0, &key.0) {
            return Err(IvyError::new(
                "native_internal_project_missing",
                "Register the configured internal project root with Codex before starting service tasks.",
            ));
        }
    }
    Ok(result)
}
pub async fn native_thread_project(
    tools: &ToolClients,
    service_node_id: &JsString,
    params: &IvyJson,
    options: RequestOptions,
) -> IvyResult<IvyJson> {
    if let Some(value) = explicit(params) {
        return Ok(value);
    }
    let selected = tools.service_tools(
        service_node_id.clone(),
        &[InterfaceRequirement {
            namespace: "agent".into(),
            interface_version: "1.0.0".into(),
        }],
    )?;
    let inventory = selected
        .read(
            &"agent.projects".into(),
            IvyJson::object(Vec::<(String, IvyJson)>::new()),
            ToolCallOptions {
                request: options,
                ..Default::default()
            },
        )
        .await?;
    validate_agent("ProjectsResult", &inventory)?;
    default_native_thread_project(params, &inventory)
}
