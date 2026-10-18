//! Scheduler configuration selection and idempotent execution admission.
use crate::{
    assignment_schema::{
        default_configuration, effective_rules, execution_name, unified_main_rules,
        validate_configuration, validate_execution_target,
    },
    inbox_evidence::InboxEvidence,
    store::Record,
};
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString, hash::hash_json};
use ivy_sdk::{
    clock::{Clock, date_to_iso},
    operation_ids::scoped_operation_id,
    rpc::RequestOptions,
    tools::{InterfaceRequirement, ToolCallOptions, ToolClients},
};
use std::sync::Arc;

pub fn select_default_agent_manager<'a>(
    nodes: &'a [IvyJson],
    host_id: &JsString,
) -> Option<&'a IvyJson> {
    let ready = |node: &&IvyJson| {
        node.get("serviceName") == Some(&"agent-manager".into())
            && ["connected", "synced", "ready"]
                .iter()
                .all(|key| node.get(key) == Some(&IvyJson::Bool(true)))
            && node.get("stale") != Some(&IvyJson::Bool(true))
    };
    nodes
        .iter()
        .filter(ready)
        .find(|node| node.get("hostId") == Some(&IvyJson::String(host_id.clone())))
        .or_else(|| nodes.iter().find(ready))
}
pub struct AssignmentScheduler<'a, 'engine, 'journal> {
    pub evidence: &'a InboxEvidence<'engine, 'journal>,
    pub tools: Arc<ToolClients>,
    pub clock: &'a dyn Clock,
    pub fallback_execution: Option<&'a IvyJson>,
}
impl AssignmentScheduler<'_, '_, '_> {
    async fn operation(&self, parts: Vec<IvyJson>) -> IvyResult<JsString> {
        let mut scope = vec![
            "secretary".into(),
            self.evidence
                .settings
                .required("identity")?
                .required("scope")?
                .required("secretaryId")?
                .clone(),
        ];
        scope.extend(parts);
        scoped_operation_id(self.evidence.store.client.clone(), &IvyJson::Array(scope)).await
    }
    pub async fn default_execution(&self) -> IvyResult<Option<IvyJson>> {
        if let Some(fallback) = self.fallback_execution.filter(|v| !v.is_null()) {
            validate_execution_target(fallback)?;
            return Ok(Some(fallback.clone()));
        }
        let mut nodes = Vec::new();
        let mut cursor: Option<JsString> = None;
        loop {
            let mut request = IvyJson::object([
                ("serviceName", "agent-manager".into()),
                ("limit", IvyJson::Number(200.0)),
            ]);
            if let Some(cursor) = &cursor {
                request.insert("cursor", IvyJson::String(cursor.clone()));
            }
            let page = self
                .evidence
                .store
                .client
                .request("serviceNodes.list", request, RequestOptions::default())
                .await?;
            nodes.extend(
                page.required("items")?
                    .as_array()
                    .ok_or_else(IvyError::internal)?
                    .iter()
                    .cloned(),
            );
            cursor = page
                .get("nextCursor")
                .and_then(IvyJson::as_js_string)
                .filter(|s| !s.0.is_empty())
                .cloned();
            if cursor.is_none() {
                break;
            }
        }
        let identity = self.evidence.settings.required("identity")?;
        let host_id = identity
            .required("hostId")?
            .as_js_string()
            .ok_or_else(IvyError::internal)?;
        let Some(manager) = select_default_agent_manager(&nodes, host_id) else {
            return Ok(None);
        };
        let service_node_id = manager
            .required("serviceNodeId")?
            .as_js_string()
            .cloned()
            .ok_or_else(IvyError::internal)?;
        let selected = self.tools.service_tools(
            service_node_id.clone(),
            &[InterfaceRequirement {
                namespace: "agent".into(),
                interface_version: "1.0.0".into(),
            }],
        )?;
        let key = format!(
            "secretary-{}",
            &hash_json(identity.required("scope")?.required("secretaryId")?)?[7..23]
        );
        let location = selected
            .call(
                &"agent.resolveProject".into(),
                IvyJson::object([(
                    "selection",
                    IvyJson::object([
                        ("kind", "internal".into()),
                        ("key", key.into()),
                        ("name", "Secretary assignments".into()),
                    ]),
                )]),
                None,
                ToolCallOptions::default(),
            )
            .await?;
        let target = IvyJson::object([
            ("serviceNodeId", IvyJson::String(service_node_id)),
            (
                "threadCwd",
                location.get("cwd").cloned().ok_or_else(|| {
                    IvyError::new("invalid_arguments", "Value is outside the JSON domain.")
                })?,
            ),
            ("model", IvyJson::Null),
            ("effort", "medium".into()),
            ("permissions", ":read-only".into()),
        ]);
        validate_execution_target(&target)?;
        Ok(Some(target))
    }
    pub async fn initialize(&self) -> IvyResult<()> {
        self.evidence.verify_owner().await?;
        let now = date_to_iso(self.clock.utc_millis() as f64)?;
        let store = self.evidence.store;
        let configuration = store
            .named(Record::Configuration, &"configuration".into())
            .await?;
        match configuration {
            None => {
                let execution = self.default_execution().await?;
                let value =
                    default_configuration(self.evidence.settings, &now, execution.as_ref())?;
                let operation = self.operation(vec!["configuration".into()]).await?;
                store
                    .create(
                        Record::Configuration,
                        &"configuration".into(),
                        &value,
                        &operation,
                    )
                    .await?;
            }
            Some(configuration) => {
                let value = configuration.required("value")?;
                let rules = unified_main_rules(value.required("rules")?);
                let execution = match value.get("execution").filter(|v| !v.is_null()) {
                    Some(execution) => Some(execution.clone()),
                    None => self.default_execution().await?,
                };
                if hash_json(&rules)? != hash_json(value.required("rules")?)?
                    || (execution.is_some() && value.get("execution").is_none_or(IvyJson::is_null))
                {
                    let mut next = value.clone();
                    next.insert("rules", rules);
                    next.insert("execution", execution.unwrap_or(IvyJson::Null));
                    next.insert("updatedAt", IvyJson::String(now));
                    validate_configuration(&next)?;
                    store
                        .amend(&configuration, Record::Configuration, &next)
                        .await?;
                }
            }
        }
        Ok(())
    }
    pub async fn configuration(&self) -> IvyResult<IvyJson> {
        let store = self.evidence.store;
        match store
            .named(Record::Configuration, &"configuration".into())
            .await?
        {
            Some(configuration) => {
                validate_configuration(configuration.required("value")?)?;
                Ok(configuration)
            }
            None => {
                self.initialize().await?;
                Ok(store
                    .named(Record::Configuration, &"configuration".into())
                    .await?
                    .unwrap_or(IvyJson::Null))
            }
        }
    }
    pub async fn enqueue(
        &self,
        assignment: &IvyJson,
        trigger: &IvyJson,
        configuration: &IvyJson,
    ) -> IvyResult<IvyJson> {
        let target = configuration
            .get("execution")
            .filter(|v| !v.is_null())
            .ok_or_else(|| {
                IvyError::new(
                    "secretary_execution_unconfigured",
                    "Secretary execution target is not configured.",
                )
            })?;
        let value = assignment.required("value")?;
        let assignment_id = value
            .required("assignmentId")?
            .as_js_string()
            .ok_or_else(IvyError::internal)?;
        let trigger_key = trigger
            .required("key")?
            .as_js_string()
            .ok_or_else(IvyError::internal)?;
        let name = JsString::from(execution_name(assignment_id, trigger_key)?);
        let store = self.evidence.store;
        if let Some(existing) = store.named(Record::Execution, &name).await? {
            return Ok(existing);
        }
        let mut target = target.clone();
        if let Some(overrides) = value.get("execution") {
            for key in ["model", "effort"] {
                if let Some(value) = overrides.get(key).filter(|v| !v.is_null()) {
                    target.insert(key, value.clone());
                }
            }
        }
        let now = date_to_iso(self.clock.utc_millis() as f64)?;
        let execution = IvyJson::object([
            ("schemaVersion", IvyJson::Number(1.0)),
            (
                "executionId",
                hash_json(&IvyJson::Array(vec![
                    IvyJson::String(assignment_id.clone()),
                    IvyJson::String(trigger_key.clone()),
                ]))?
                .into(),
            ),
            ("assignment", assignment.required("pin")?.clone()),
            ("assignmentSnapshot", value.clone()),
            ("trigger", trigger.clone()),
            (
                "effectiveRules",
                effective_rules(configuration.required("rules")?, value.required("rules")?)?,
            ),
            ("executionTarget", target),
            ("phase", "queued".into()),
            (
                "serviceNodeId",
                configuration
                    .required("execution")?
                    .required("serviceNodeId")?
                    .clone(),
            ),
            ("nativeTarget", IvyJson::Null),
            ("threadId", IvyJson::Null),
            ("turnId", IvyJson::Null),
            (
                "nativeOperations",
                IvyJson::object([
                    ("threadStart", IvyJson::Null),
                    ("turnStart", IvyJson::Null),
                    ("archive", IvyJson::Null),
                    ("delete", IvyJson::Null),
                ]),
            ),
            ("preflightOutput", IvyJson::Null),
            ("result", IvyJson::Null),
            ("errorCode", IvyJson::Null),
            ("createdAt", IvyJson::String(now.clone())),
            ("startedAt", IvyJson::Null),
            ("completedAt", IvyJson::Null),
            ("archivedAt", IvyJson::Null),
            ("deleteAfter", IvyJson::Null),
            ("deletedAt", IvyJson::Null),
            ("updatedAt", IvyJson::String(now)),
        ]);
        let operation = self
            .operation(vec![
                "execution".into(),
                IvyJson::String(assignment_id.clone()),
                IvyJson::String(trigger_key.clone()),
            ])
            .await?;
        store
            .create(Record::Execution, &name, &execution, &operation)
            .await
    }
}
