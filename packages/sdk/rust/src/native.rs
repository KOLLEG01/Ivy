//! Discovery against one pinned native owner, with a status check on both sides.
use crate::{
    IvyError, IvyJson, IvyResult, JsString,
    native_evidence::{NativeOwnerIdentity, validate_native_status},
    rpc::RequestOptions,
    tools::{InterfaceRequirement, ToolCallOptions, ToolClients},
};
use ivy_contracts::{agent_schema::validate_agent, native_contract::NativeContract};

pub type NativeTarget = NativeOwnerIdentity;
pub struct NativeContractSnapshot {
    pub target: NativeTarget,
    pub epoch: JsString,
    pub contract: NativeContract,
}

async fn management(
    tools: &ToolClients,
    target: &NativeTarget,
    name: &str,
    options: &RequestOptions,
) -> IvyResult<IvyJson> {
    if options.cancellation.is_cancelled() {
        return Err(options.cancel_error.clone());
    }
    tools
        .service_tools(
            target.service_node_id.clone(),
            &[InterfaceRequirement {
                namespace: "agent".into(),
                interface_version: "1.0.0".into(),
            }],
        )?
        .call(
            &format!("agent.{name}").into(),
            IvyJson::object(Vec::<(String, IvyJson)>::new()),
            None,
            ToolCallOptions {
                request: options.clone(),
                ..Default::default()
            },
        )
        .await
}

/// Read the selected catalog without selecting a newer version or replaying calls.
pub async fn read_native_contract(
    tools: &ToolClients,
    target: &NativeTarget,
    options: RequestOptions,
) -> IvyResult<NativeContractSnapshot> {
    let expected = target.clone();
    let before = management(tools, &expected, "status", &options).await?;
    validate_native_status(&before, &expected, None)?;
    let value = management(tools, &expected, "catalog", &options).await?;
    validate_agent("Catalog", &value)?;
    let contract = NativeContract::new(&value)?;
    if JsString::from(contract.catalog_hash()) != expected.catalog_hash
        || contract
            .catalog()
            .get("version")
            .and_then(IvyJson::as_js_string)
            != Some(&expected.native_version)
        || contract
            .catalog()
            .get("nativeExecutableHash")
            .and_then(IvyJson::as_js_string)
            != Some(&expected.native_executable_hash)
    {
        return Err(IvyError::new(
            "native_catalog_mismatch",
            "The provider catalog differs from the retained native version, executable or catalog hash.",
        ));
    }
    let after = management(tools, &expected, "status", &options).await?;
    validate_native_status(&after, &expected, None)?;
    if before.get("epoch") != after.get("epoch") {
        return Err(IvyError::new(
            "native_epoch_changed",
            "The native owner restarted during contract discovery.",
        ));
    }
    let epoch = before
        .required("epoch")?
        .as_js_string()
        .cloned()
        .ok_or_else(IvyError::internal)?;
    Ok(NativeContractSnapshot {
        target: expected,
        epoch,
        contract,
    })
}
