//! Retain original assignment calls before dispatch through the shared native owner.
use crate::inbox_evidence::{InboxEvidence, verify_owner};
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString};
use ivy_sdk::{
    native_evidence::NativeOwnerIdentity,
    native_operation::NativeOperationCall,
    native_owner::{GuardFuture, NativeDispatchGuard, NativeOwner},
    rpc::{RequestOptions, RpcClient},
    tools::ToolClients,
};
use std::sync::Arc;

const CALL_KEY: &str = "secretary/assignment-native-call";
fn text(value: &IvyJson, key: &str) -> IvyResult<JsString> {
    value
        .required(key)?
        .as_js_string()
        .cloned()
        .ok_or_else(IvyError::internal)
}
pub(crate) struct DispatchGuard {
    client: Arc<dyn RpcClient>,
    service_node_id: JsString,
    settings: IvyJson,
    cancellation: RequestOptions,
}
impl DispatchGuard {
    pub(crate) fn new(evidence: &InboxEvidence<'_, '_>) -> Self {
        Self {
            client: evidence.store.client.clone(),
            service_node_id: evidence.service_node_id.clone(),
            settings: evidence.settings.clone(),
            cancellation: evidence.cancellation.clone(),
        }
    }
}
impl NativeDispatchGuard for DispatchGuard {
    fn check(&self) -> GuardFuture<'_> {
        Box::pin(verify_owner(
            self.client.as_ref(),
            &self.service_node_id,
            &self.settings,
            &self.cancellation,
        ))
    }
}
pub struct AssignmentNative<'a, 'engine, 'journal> {
    pub evidence: &'a InboxEvidence<'engine, 'journal>,
    pub tools: Arc<ToolClients>,
}
impl AssignmentNative<'_, '_, '_> {
    pub fn owner(&self, execution: &IvyJson) -> IvyResult<NativeOwner> {
        let target = execution
            .get("nativeTarget")
            .filter(|v| !v.is_null())
            .ok_or_else(|| {
                IvyError::new(
                    "secretary_execution_target_unavailable",
                    "Secretary has not retained the exact AgentManager target.",
                )
            })?;
        Ok(NativeOwner::new(
            self.tools.clone(),
            text(self.evidence.settings.required("identity")?, "principalId")?,
            NativeOwnerIdentity {
                service_node_id: text(target, "serviceNodeId")?,
                host_id: Some(text(target, "hostId")?),
                native_version: text(target, "nativeVersion")?,
                native_executable_hash: text(target, "nativeExecutableHash")?,
                catalog_hash: text(target, "catalogHash")?,
            },
            self.evidence.cancellation.clone(),
        ))
    }
    pub async fn completed(
        &self,
        owner: &NativeOwner,
        operation_id: &JsString,
        method: &JsString,
        params: &IvyJson,
    ) -> IvyResult<IvyJson> {
        let journal = self.evidence.store.journal;
        let name = operation_id.to_utf8()?;
        let saved = match journal.named(CALL_KEY, &name, true)? {
            Some(saved) => saved,
            None => {
                let params = if method == &JsString::from("thread/start") {
                    owner.thread_start_params(params).await?
                } else {
                    params.clone()
                };
                let binding = owner.binding(method, None).await?;
                self.evidence.verify_owner().await?;
                journal.create_technical(
                    CALL_KEY,
                    &name,
                    &IvyJson::object([
                        ("operationId", IvyJson::String(operation_id.clone())),
                        ("method", IvyJson::String(method.clone())),
                        ("params", params),
                        (
                            "definitionHash",
                            binding.value().required("definitionHash")?.clone(),
                        ),
                    ]),
                )?
            }
        };
        // Recovery deliberately ignores regenerated params and reads only the
        // immutable call saved under this operation ID.
        if saved.value.get("operationId") != Some(&IvyJson::String(operation_id.clone()))
            || saved.value.get("method") != Some(&IvyJson::String(method.clone()))
        {
            return Err(IvyError::new(
                "secretary_execution_native_mismatch",
                "The retained native call belongs to a different operation.",
            ));
        }
        let call = NativeOperationCall {
            operation_id: operation_id.clone(),
            method: method.clone(),
            params: saved.value.required("params")?.clone(),
            definition_hash: text(&saved.value, "definitionHash")?,
        };
        let mut observed = owner.operation(&call).await?;
        if observed.get("phase").is_none() {
            let guard = DispatchGuard::new(self.evidence);
            observed = match owner.dispatch(&call, &guard).await? {
                Some(observed) => observed,
                None => owner.operation(&call).await?,
            };
        }
        if observed.get("phase").is_some() {
            owner.check_operation(&observed, &call)?;
        }
        let succeeded = observed.get("phase") == Some(&"succeeded".into());
        if !succeeded && observed.get("phase") != Some(&"failed".into()) {
            return Err(IvyError::new(
                "secretary_execution_native_pending",
                "The original Secretary native operation is still pending.",
            ));
        }
        if !succeeded
            || observed
                .get("reply")
                .and_then(|v| v.get("result"))
                .is_none()
        {
            return Err(IvyError::new(
                "secretary_execution_native_failed",
                "The original Secretary native operation failed.",
            ));
        }
        Ok(observed)
    }
}
