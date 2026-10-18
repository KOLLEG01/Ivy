//! Durable assignment/configuration write admission and original-receipt recovery.
use crate::{
    inbox_evidence::{InboxEvidence, operation_name},
    journal::Document,
};
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString, Outcome,
    hash::hash_json,
    operation::{derive_operation_id_js, parse_operation_id_js},
};
use ivy_sdk::{clock::Clock, rpc::RequestOptions};

pub const WORKFLOW_OPERATION_KEY: &str = "secretary/public-operation";
#[derive(Clone, Copy)]
pub enum WorkflowAction {
    CreateAssignment,
    UpdateAssignment,
    ConfigureExecution,
}
impl WorkflowAction {
    pub fn name(self) -> &'static str {
        match self {
            Self::CreateAssignment => "create_assignment",
            Self::UpdateAssignment => "update_assignment",
            Self::ConfigureExecution => "configure_execution",
        }
    }
}
fn need(ok: bool, code: &str, message: &str) -> IvyResult<()> {
    if ok {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}

/// The engine supplies its original runtime epoch and clock. Public handlers
/// validate their request and current owner before preparing or replaying a write.
pub struct AssignmentWorkflow<'a, 'engine, 'journal> {
    pub evidence: &'a InboxEvidence<'engine, 'journal>,
    pub runtime_epoch: &'a JsString,
    pub clock: &'a dyn Clock,
}
impl AssignmentWorkflow<'_, '_, '_> {
    pub fn validate_operation_id(&self, id: &JsString) -> IvyResult<()> {
        let identity = parse_operation_id_js(id)?;
        need(
            identity.runtime_epoch == *self.runtime_epoch
                && identity.issued_at_unix_ms as f64 <= self.clock.utc_millis() as f64 + 60_000.0
                && identity.issued_at_unix_ms > self.evidence.store.journal.expired_before()?,
            "operation_expired",
            "The Secretary operation belongs to another runtime or predates the receipt ring.",
        )
    }
    pub async fn receipt(&self, value: &IvyJson) -> IvyResult<Option<IvyJson>> {
        let write = value.required("write")?;
        let mutation_id = write.required("mutationId")?;
        let expected = derive_operation_id_js(
            value
                .required("operationId")?
                .as_js_string()
                .ok_or_else(IvyError::internal)?,
            &IvyJson::Array(vec![
                "secretary-workflow".into(),
                value.required("callerPrincipalId")?.clone(),
            ]),
        )?;
        need(
            mutation_id == &IvyJson::String(expected),
            "secretary_evidence_conflict",
            "The saved write has a different operation identity.",
        )?;
        if value.get("phase") == Some(&"succeeded".into()) {
            if let Some(effect) = value.get("effect").filter(|v| !v.is_null()) {
                return Ok(Some(effect.clone()));
            }
        }
        let mut arguments = write.clone();
        arguments.remove("mutationId");
        let result = self
            .evidence
            .store
            .client
            .request(
                "objects.writeReceipt",
                IvyJson::object([
                    ("mutationId", mutation_id.clone()),
                    (
                        "expectedRequestHash",
                        hash_json(&IvyJson::object([
                            ("method", "objects.write".into()),
                            ("arguments", arguments),
                        ]))?
                        .into(),
                    ),
                ]),
                RequestOptions::default(),
            )
            .await?;
        if result.is_null() {
            return Ok(None);
        }
        let expected_key = if value.get("action") == Some(&"configure_execution".into()) {
            "secretary/configuration"
        } else {
            "secretary/assignment"
        };
        let object = result.required("object")?;
        need(
            object.get("contractKey") == Some(&expected_key.into())
                && object.get("parentId")
                    == Some(&IvyJson::String(self.evidence.store.root.clone()))
                && write
                    .get("objectId")
                    .filter(|v| !v.is_null())
                    .is_none_or(|id| object.get("id") == Some(id))
                && write
                    .get("create")
                    .filter(|v| !v.is_null())
                    .is_none_or(|create| object.get("name") == create.get("name")),
            "secretary_evidence_conflict",
            "The original write receipt names another Secretary object.",
        )?;
        Ok(Some(IvyJson::object([
            ("objectId", object.required("id")?.clone()),
            (
                "revision",
                result.required("revision")?.required("revision")?.clone(),
            ),
        ])))
    }
    pub fn complete(&self, row: &Document, effect: &IvyJson) -> IvyResult<IvyJson> {
        if row.value.get("phase") != Some(&"succeeded".into()) {
            let mut next = row.value.clone();
            next.insert("phase", "succeeded".into());
            next.insert("effect", effect.clone());
            self.evidence
                .store
                .journal
                .amend_technical(WORKFLOW_OPERATION_KEY, &row.pin, &next)?;
        }
        self.evidence
            .store
            .journal
            .complete_technical_workflow(&row.pin.object_id, self.clock.utc_millis())?;
        Ok(effect.clone())
    }
    pub async fn save(
        &self,
        caller: &IvyJson,
        request: &IvyJson,
        action: WorkflowAction,
        write: Option<&IvyJson>,
    ) -> IvyResult<IvyJson> {
        let id = request.required("operationId")?;
        let name = operation_name(caller, id)?.to_utf8()?;
        let request_hash = hash_json(request)?;
        need(
            self.evidence.find(caller, id).await?.is_none(),
            "mutation_conflict",
            "The operation identity already names an inbox action.",
        )?;
        let journal = self.evidence.store.journal;
        let prior = journal.named(WORKFLOW_OPERATION_KEY, &name, true)?;
        let retry = prior.is_some();
        let row = match prior {
            Some(row) => row,
            None => {
                self.validate_operation_id(id.as_js_string().ok_or_else(IvyError::internal)?)?;
                let write = write.ok_or_else(|| {
                    IvyError::new(
                        "secretary_evidence_missing",
                        "The original Secretary write was not prepared.",
                    )
                })?;
                journal.create_technical(
                    WORKFLOW_OPERATION_KEY,
                    &name,
                    &IvyJson::object([
                        ("operationId", id.clone()),
                        ("callerPrincipalId", caller.clone()),
                        ("action", action.name().into()),
                        ("requestHash", request_hash.clone().into()),
                        ("write", write.clone()),
                        ("phase", "prepared".into()),
                        ("errorCode", IvyJson::Null),
                    ]),
                )?
            }
        };
        need(
            row.value.get("callerPrincipalId") == Some(caller)
                && row.value.get("operationId") == Some(id)
                && row.value.get("action") == Some(&action.name().into())
                && row.value.get("requestHash") == Some(&request_hash.into()),
            "mutation_conflict",
            "The operation identity already names different Secretary arguments.",
        )?;
        if retry {
            if let Some(effect) = self.receipt(&row.value).await? {
                return self.complete(&row, &effect);
            }
        }
        need(
            row.value.get("phase") != Some(&"failed".into()),
            &row.value
                .get("errorCode")
                .filter(|v| !v.is_null())
                .map(IvyJson::as_str)
                .transpose()?
                .unwrap_or_else(|| "secretary_operation_failed".into()),
            "The original Secretary write failed.",
        )?;
        let result = match self
            .evidence
            .store
            .write_prepared(row.value.required("write")?.clone())
            .await
        {
            Ok(effect) => self.complete(&row, &effect),
            Err(error) => Err(error),
        };
        if let Err(failure) = &result {
            if !retry && failure.outcome == Outcome::NotExecuted {
                if let Some(latest) = journal.named(WORKFLOW_OPERATION_KEY, &name, true)? {
                    if latest.value.get("phase") == Some(&"prepared".into()) {
                        let mut next = latest.value.clone();
                        next.insert("phase", "failed".into());
                        next.insert("errorCode", failure.code.clone().into());
                        journal.amend_technical(WORKFLOW_OPERATION_KEY, &latest.pin, &next)?;
                        journal.complete_technical_workflow(
                            &latest.pin.object_id,
                            self.clock.utc_millis(),
                        )?;
                    }
                }
            }
        }
        result
    }
}
