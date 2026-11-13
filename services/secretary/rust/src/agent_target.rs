//! One explicit AgentManager selection for new work and project resolution.
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString, native_backend::BackendIdentity};
use ivy_sdk::{
    native_evidence::NativeOwnerIdentity,
    tools::{InterfaceRequirement, ToolCallOptions, ToolClients},
};

pub struct AgentSelection {
    pub service_node_id: JsString,
    pub backend: Option<BackendIdentity>,
}
impl AgentSelection {
    /// Automatic initialization requires a unique advertised owner. A model or
    /// today's default account cannot establish the owner of retained work.
    pub async fn discover(tools: &ToolClients, service_node_id: JsString, host_id: &JsString) -> IvyResult<Option<Self>> {
        let selected = tools.service_tools(service_node_id.clone(), &[InterfaceRequirement {
            namespace: "agent".into(), interface_version: "1.0.0".into(),
        }])?;
        let result = selected.call(&"agent.backends".into(), IvyJson::object(
            Vec::<(String, IvyJson)>::new()), None, ToolCallOptions::default()).await?;
        ivy_contracts::agent_schema::validate_agent("BackendsResult", &result)?;
        let owners = result.required("backends")?.as_array().ok_or_else(IvyError::internal)?;
        if owners.len() != 1 || owners[0].get("state") != Some(&"ready".into()) {
            return Ok(None);
        }
        let owner = &owners[0];
        if owner.get("serviceNodeId").and_then(IvyJson::as_js_string) != Some(&service_node_id)
            || owner.get("hostId").and_then(IvyJson::as_js_string) != Some(host_id) {
            return Err(IvyError::new("native_owner_mismatch", "Backend discovery belongs to another AgentManager host."));
        }
        Self::from_json(owner).map(Some)
    }
    pub fn from_json(value: &IvyJson) -> IvyResult<Self> {
        Ok(Self {
            service_node_id: value.required("serviceNodeId")?.as_js_string()
                .cloned().ok_or_else(IvyError::internal)?,
            backend: BackendIdentity::optional(value.get("backend"))?,
        })
    }
    pub fn arguments(&self, value: IvyJson) -> IvyResult<IvyJson> {
        BackendIdentity::select_arguments(self.backend.as_ref(), value)
    }
    pub async fn resolve(&self, tools: &ToolClients) -> IvyResult<NativeOwnerIdentity> {
        let selected = tools.service_tools(self.service_node_id.clone(), &[InterfaceRequirement {
            namespace: "agent".into(), interface_version: "1.0.0".into(),
        }])?;
        let status = selected.call(&"agent.status".into(), self.arguments(IvyJson::object(
            Vec::<(String, IvyJson)>::new()))?, None, ToolCallOptions::default()).await?;
        if !BackendIdentity::matches(&status, self.backend.as_ref())
            || status.get("serviceNodeId").and_then(IvyJson::as_js_string) != Some(&self.service_node_id)
            || status.get("state") != Some(&"ready".into())
            || !status.get("epoch").and_then(IvyJson::as_js_string).is_some_and(|s| !s.0.is_empty())
        {
            return Err(IvyError::new("secretary_execution_target_unavailable",
                "The configured AgentManager is not ready for a Secretary task."));
        }
        // Parse only the owner projection; status has additional diagnostic fields.
        let mut target = IvyJson::object(["serviceNodeId", "hostId", "nativeVersion",
            "nativeExecutableHash", "catalogHash"].map(|key|
            (key, status.get(key).cloned().unwrap_or(IvyJson::Null))));
        if let Some(backend) = &self.backend { target.insert("backend", backend.json()); }
        NativeOwnerIdentity::from_json(&target)
    }
}
