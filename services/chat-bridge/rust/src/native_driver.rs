//! Configured native-owner transport and readiness. Durable Main/queue claims
//! remain the caller's responsibility; readiness alone never authorizes dispatch.
use crate::admission::ChatAdmission;
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString, checked_native_contract::CheckedNativeContracts,
};
use ivy_sdk::{
    connection_owner::connection_owner,
    native_evidence::{NativeOwnerIdentity, validate_native_status},
    native_owner::NativeOwner,
    rpc::{RequestOptions, RpcClient},
    tools::ToolClients,
};
use std::sync::Arc;

pub struct ChatNativeDriver<'a> {
    client: Arc<dyn RpcClient>,
    tools: Arc<ToolClients>,
    admission: &'a ChatAdmission,
    catalogs: &'a CheckedNativeContracts,
    service_node_id: JsString,
}

impl<'a> ChatNativeDriver<'a> {
    /// Reuse the provider connection's ToolClients, including its binding and
    /// selected-catalog caches. The service identity is distinct from the
    /// configured native project's owner.
    pub fn new(
        client: Arc<dyn RpcClient>,
        tools: Arc<ToolClients>,
        admission: &'a ChatAdmission,
        catalogs: &'a CheckedNativeContracts,
        service_node_id: JsString,
    ) -> Self {
        Self {
            client,
            tools,
            admission,
            catalogs,
            service_node_id,
        }
    }

    pub async fn verify_owner(&self) -> IvyResult<()> {
        let node = connection_owner(self.client.as_ref(), &self.service_node_id).await?;
        if node.get("principalId") != self.admission.definition().get("principalId")
            || node.get("serviceName") != Some(&Json::from("chat-bridge"))
            || !node.get("connected").is_some_and(truthy)
            || !node.get("synced").is_some_and(truthy)
        {
            return Err(IvyError::new(
                "chat_principal_mismatch",
                "Chat recovery requires its configured principal and a live synchronized service connection.",
            ));
        }
        Ok(())
    }

    pub fn native_owner(&self) -> IvyResult<NativeOwner> {
        let definition = self.admission.definition();
        let plan = definition.required("nativePlan")?;
        let source = self.catalogs.checked_native_contract(
            text(plan, "nativeVersion")?,
            Some(&Json::object([(
                "sourceHash",
                plan.required("catalogSourceHash")?.clone(),
            )])),
        )?;
        let target = NativeOwnerIdentity {
            service_node_id: text(definition.required("project")?, "serviceNodeId")?.clone(),
            host_id: None,
            native_version: text(source.catalog(), "version")?.clone(),
            native_executable_hash: text(source.catalog(), "nativeExecutableHash")?.clone(),
            catalog_hash: source.catalog_hash().into(),
        };
        Ok(NativeOwner::new(
            self.tools.clone(),
            text(definition, "principalId")?.clone(),
            target,
            RequestOptions::default(),
        ))
    }

    pub async fn management(&self, name: &str, args: Json) -> IvyResult<Json> {
        self.native_owner()?.management(name, args, None).await
    }

    pub async fn ready(&self) -> IvyResult<Json> {
        self.verify_owner().await?;
        let value = self
            .management("status", Json::object([] as [(&str, Json); 0]))
            .await?;
        validate_native_status(
            &value,
            self.native_owner()?.target(),
            Some("chat_native_unavailable"),
        )?;
        Ok(value)
    }
}

fn text<'a>(value: &'a Json, name: &str) -> IvyResult<&'a JsString> {
    value
        .required(name)?
        .as_js_string()
        .ok_or_else(IvyError::internal)
}

fn truthy(value: &Json) -> bool {
    match value {
        Json::Null => false,
        Json::Bool(value) => *value,
        Json::Number(value) => *value != 0.0 && !value.is_nan(),
        Json::String(value) => !value.0.is_empty(),
        _ => true,
    }
}
