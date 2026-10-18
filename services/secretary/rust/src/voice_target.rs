//! Discover the sole ready Voice provider with one configured recipient.
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString};
use ivy_sdk::{
    rpc::{RequestOptions, RpcClient},
    tools::{InterfaceRequirement, ToolCallOptions, ToolClients},
};

pub fn unique_voice_target(candidates: &[IvyJson]) -> Option<IvyJson> {
    (candidates.len() == 1).then(|| candidates[0].clone())
}

/// `tools` belongs to the same RPC connection as `client`, preserving its selected
/// provider binding cache. Discovery does not place a call or select a recipient.
pub async fn discover_voice_target(
    client: &dyn RpcClient,
    tools: &ToolClients,
) -> IvyResult<Option<IvyJson>> {
    let mut nodes = Vec::new();
    let mut cursor: Option<IvyJson> = None;
    loop {
        let mut params = IvyJson::object([
            ("serviceName", "phone-bridge".into()),
            ("limit", IvyJson::Number(200.0)),
        ]);
        if let Some(cursor) = cursor {
            params.insert("cursor", cursor);
        }
        let page = client
            .request("serviceNodes.list", params, RequestOptions::default())
            .await?;
        nodes.extend(
            page.get("items")
                .and_then(IvyJson::as_array)
                .ok_or_else(IvyError::internal)?
                .iter()
                .cloned(),
        );
        cursor = page
            .get("nextCursor")
            .filter(|v| !v.is_null() && v.as_js_string().is_some_and(|s| !s.0.is_empty()))
            .cloned();
        if cursor.is_none() {
            break;
        }
    }
    let mut candidates = Vec::new();
    for node in nodes {
        if node.is_null() {
            return Err(IvyError::internal());
        }
        if node.get("serviceName").and_then(IvyJson::as_js_string)
            != Some(&JsString::from("phone-bridge"))
            || ["connected", "synced", "ready"]
                .iter()
                .any(|key| node.get(key).and_then(IvyJson::as_bool) != Some(true))
            || node.get("stale").and_then(IvyJson::as_bool) == Some(true)
        {
            continue;
        }
        let Some(service_node_id) = node.get("serviceNodeId").and_then(IvyJson::as_js_string)
        else {
            continue;
        };
        let selected = match tools.service_tools(
            service_node_id.clone(),
            &[InterfaceRequirement {
                namespace: "phone".into(),
                interface_version: "1.0.0".into(),
            }],
        ) {
            Ok(selected) => selected,
            Err(_) => continue,
        };
        let status = match selected
            .call(
                &"phone.status".into(),
                IvyJson::Object(Vec::new()),
                None,
                ToolCallOptions::default(),
            )
            .await
        {
            Ok(status) => status,
            Err(_) => continue,
        };
        // A raw null status throws outside the original IvyError-only catch;
        // provider IvyErrors above are deliberately ignored for automatic choice.
        if status.is_null() {
            return Err(IvyError::internal());
        }
        if status
            .get("registration")
            .and_then(|v| v.get("state"))
            .and_then(IvyJson::as_js_string)
            != Some(&JsString::from("registered"))
        {
            continue;
        }
        let Some(recipients) = status
            .get("recipients")
            .and_then(IvyJson::as_array)
            .filter(|v| v.len() == 1)
        else {
            continue;
        };
        let binding = match selected.binding(&"phone.request".into(), None).await {
            Ok(binding) => binding,
            Err(_) => continue,
        };
        let mut candidate = IvyJson::object([
            ("serviceNodeId", IvyJson::String(service_node_id.clone())),
            ("recipientId", recipients[0].clone()),
        ]);
        if let Some(hash) = binding.value().get("definitionHash") {
            candidate.insert("requestDefinitionHash", hash.clone());
        }
        candidates.push(candidate);
    }
    Ok(unique_voice_target(&candidates))
}
