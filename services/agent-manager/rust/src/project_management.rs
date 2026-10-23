//! Validated management entry points for host project and workspace resolution.
use crate::{
    directories::{DirectoryComparator, list_directories},
    project_locations::ProjectLocations,
};
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString, agent_schema::validate_agent};
use ivy_sdk::service::{ServiceFuture, ToolHandler};
use std::{collections::HashMap, sync::Arc};

pub type ProjectAvailability = dyn Fn() -> IvyResult<()> + Send + Sync;
pub type FreshProjects = dyn Fn() -> ServiceFuture<()> + Send + Sync;
pub struct ProjectManagement {
    pub locations: ProjectLocations,
    pub host_id: JsString,
    pub service_node_id: JsString,
    pub available: Arc<ProjectAvailability>,
    pub fresh_projects: Arc<FreshProjects>,
    pub compare_directories: Arc<DirectoryComparator>,
}
impl ProjectManagement {
    pub fn handlers(self: &Arc<Self>) -> HashMap<JsString, ToolHandler> {
        ["resolveProject", "resolveWorkspace", "listDirectories"].into_iter().map(|name| {
            let owner = self.clone();
            let handler: ToolHandler = Arc::new(move |args, context| {
                let owner = owner.clone();
                Box::pin(async move {
                    let schema = match name {"resolveProject" => "ProjectResolveInput", "resolveWorkspace" => "WorkspaceResolveInput", _ => "DirectoryListInput"};
                    validate_agent(schema, &args)?;
                    (owner.available)()?;
                    match name {
                        "resolveProject" => {
                            let selection = args.required("selection")?;
                            if selection.get("kind") != Some(&"projectless".into()) {(owner.fresh_projects)().await?;}
                            owner.locations.resolve(selection, &context.caller_principal_id, args.get("expectedProjectId").and_then(IvyJson::as_js_string)).await
                        }
                        "resolveWorkspace" => {
                            (owner.fresh_projects)().await?;
                            let prepare = args.get("prepare") == Some(&IvyJson::Bool(true));
                            if prepare && !context.operation_id.as_ref().is_some_and(|value| !value.0.is_empty()) {
                                return Err(IvyError::new("operation_id_required", "Preparing a Task workspace requires a stable operation identity."));
                            }
                            owner.locations.workspace(args.required("taskKey")?.as_js_string().unwrap(), args.required("requirement")?, prepare, &owner.host_id, &owner.service_node_id, args.get("verifyOrigin") == Some(&IvyJson::Bool(true))).await
                        }
                        _ => list_directories(args.required("path")?.as_js_string().unwrap(), owner.compare_directories.clone()).await,
                    }
                }) as ServiceFuture<IvyJson>
            });
            (format!("agent.{name}").into(), handler)
        }).collect()
    }
}
