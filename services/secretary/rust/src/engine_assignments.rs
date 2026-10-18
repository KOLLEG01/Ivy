//! Public assignment writes and operation reads compose the original durable
//! workflow; no alternate receipt or replay path lives in these entrypoints.
use crate::{
    assignment_schema::{
        assignment_name, unified_main_rules, unified_rule_overrides, validate_assignment,
        validate_configuration,
    },
    assignment_workflow::{AssignmentWorkflow, WORKFLOW_OPERATION_KEY, WorkflowAction},
    engine::SecretaryEngine,
    inbox_evidence::operation_name,
    journal::Document,
    schema::{Schema, validate},
    store::Record,
};
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString, hash::hash_json, operation::derive_operation_id_js,
};

fn need(condition: bool, code: &str, message: &str) -> IvyResult<()> {
    if condition {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn string<'a>(value: &'a IvyJson, key: &str) -> IvyResult<&'a JsString> {
    value
        .required(key)?
        .as_js_string()
        .ok_or_else(IvyError::internal)
}
fn same(a: &IvyJson, b: &IvyJson) -> IvyResult<bool> {
    Ok(hash_json(a)? == hash_json(b)?)
}
impl SecretaryEngine<'_, '_, '_> {
    pub(crate) fn workflow(&self) -> AssignmentWorkflow<'_, '_, '_> {
        AssignmentWorkflow {
            evidence: self.evidence,
            runtime_epoch: self.runtime_epoch,
            clock: self.operation_clock,
        }
    }
    pub(crate) fn prior_workflow(
        &self,
        caller: &IvyJson,
        id: &IvyJson,
    ) -> IvyResult<Option<Document>> {
        self.evidence.store.journal.named(
            WORKFLOW_OPERATION_KEY,
            &operation_name(caller, id)?.to_utf8()?,
            true,
        )
    }
    pub async fn save_configuration(
        &self,
        caller: &IvyJson,
        request: &IvyJson,
    ) -> IvyResult<IvyJson> {
        validate(Schema::SaveConfigurationRequest, request)?;
        validate_configuration(request.required("value")?)?;
        self.evidence.verify_owner().await?;
        let workflow = self.workflow();
        if self
            .prior_workflow(caller, request.required("operationId")?)?
            .is_some()
        {
            return workflow
                .save(caller, request, WorkflowAction::ConfigureExecution, None)
                .await;
        }
        workflow.validate_operation_id(string(request, "operationId")?)?;
        let store = self.evidence.store;
        let current = store
            .named(Record::Configuration, &"configuration".into())
            .await?;
        let matches = match &current {
            Some(current) => same(current.required("pin")?, request.required("configuration")?)?,
            None => false,
        };
        need(
            matches,
            "revision_conflict",
            "Reload the current Secretary configuration before saving.",
        )?;
        let current = current.ok_or_else(IvyError::internal)?;
        let mut value = request.required("value")?.clone();
        value.insert("rules", unified_main_rules(value.required("rules")?));
        value.insert("updatedAt", IvyJson::String(self.now()?));
        let write = store.object_write_request(
            Record::Configuration,
            &value,
            &IvyJson::object([("object", current.required("pin")?.clone())]),
            &derive_operation_id_js(
                string(request, "operationId")?,
                &IvyJson::Array(vec!["secretary-workflow".into(), caller.clone()]),
            )?,
        )?;
        workflow
            .save(
                caller,
                request,
                WorkflowAction::ConfigureExecution,
                Some(&write),
            )
            .await
    }
    pub async fn save_assignment(&self, caller: &IvyJson, request: &IvyJson) -> IvyResult<IvyJson> {
        validate(Schema::SaveAssignmentRequest, request)?;
        validate_assignment(request.required("value")?)?;
        self.evidence.verify_owner().await?;
        let create = request.required("assignment")?.is_null();
        let action = if create {
            WorkflowAction::CreateAssignment
        } else {
            WorkflowAction::UpdateAssignment
        };
        let workflow = self.workflow();
        if self
            .prior_workflow(caller, request.required("operationId")?)?
            .is_some()
        {
            return workflow.save(caller, request, action, None).await;
        }
        workflow.validate_operation_id(string(request, "operationId")?)?;
        let name = assignment_name(string(request.required("value")?, "assignmentId")?)?;
        let store = self.evidence.store;
        let current = store
            .named(Record::Assignment, &name.clone().into())
            .await?;
        let now = self.now()?;
        let mut value = request.required("value")?.clone();
        value.insert("rules", unified_rule_overrides(value.required("rules")?));
        let destination = if create {
            need(
                current.is_none(),
                "secretary_identity_conflict",
                "A new assignment needs a fresh identity.",
            )?;
            value.insert("createdAt", IvyJson::String(now.clone()));
            IvyJson::object([("createName", name.into())])
        } else {
            let matches = match &current {
                Some(current) => same(current.required("pin")?, request.required("assignment")?)?,
                None => false,
            };
            need(
                matches,
                "revision_conflict",
                "Reload the current Secretary assignment before saving.",
            )?;
            let current = current.ok_or_else(IvyError::internal)?;
            let prior = current.required("value")?;
            need(
                prior.get("assignmentId") == value.get("assignmentId")
                    && prior.get("createdAt") == value.get("createdAt"),
                "secretary_identity_conflict",
                "An assignment update cannot replace its retained identity.",
            )?;
            IvyJson::object([("object", current.required("pin")?.clone())])
        };
        value.insert("updatedAt", IvyJson::String(now));
        let write = store.object_write_request(
            Record::Assignment,
            &value,
            &destination,
            &derive_operation_id_js(
                string(request, "operationId")?,
                &IvyJson::Array(vec!["secretary-workflow".into(), caller.clone()]),
            )?,
        )?;
        workflow.save(caller, request, action, Some(&write)).await
    }
    pub async fn operation_read(
        &self,
        caller: &IvyJson,
        scope: &IvyJson,
        id: &IvyJson,
    ) -> IvyResult<IvyJson> {
        self.evidence.authorize(caller, scope, "operation", None)?;
        self.evidence.verify_owner().await?;
        let inbox = self.evidence.find(caller, id).await?;
        let at = self.now()?;
        if let Some(inbox) = inbox {
            let value = inbox.required("value")?;
            return Ok(IvyJson::object([
                ("operationId", id.clone()),
                ("action", value.required("action")?.clone()),
                ("phase", value.required("phase")?.clone()),
                ("effect", value.required("effect")?.clone()),
                ("errorCode", value.required("errorCode")?.clone()),
                ("observedAt", IvyJson::String(at)),
            ]));
        }
        let row = self.prior_workflow(caller, id)?;
        need(
            row.as_ref().is_some_and(|row| {
                row.value.get("callerPrincipalId") == Some(caller)
                    && row.value.get("operationId") == Some(id)
            }),
            "not_found",
            "The caller has no retained operation with this ID.",
        )?;
        let row = row.ok_or_else(IvyError::internal)?;
        let workflow = self.workflow();
        let effect = workflow.receipt(&row.value).await?;
        if let Some(effect) = &effect {
            workflow.complete(&row, effect)?;
        }
        let phase = if effect.is_some() {
            "succeeded"
        } else if row.value.get("phase") == Some(&"failed".into()) {
            "failed"
        } else {
            "unknown"
        };
        Ok(IvyJson::object([
            ("operationId", id.clone()),
            ("action", row.value.required("action")?.clone()),
            ("phase", phase.into()),
            ("effect", effect.clone().unwrap_or(IvyJson::Null)),
            (
                "errorCode",
                if effect.is_some() {
                    IvyJson::Null
                } else {
                    row.value.required("errorCode")?.clone()
                },
            ),
            ("observedAt", IvyJson::String(at)),
        ]))
    }
}
