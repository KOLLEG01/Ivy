use crate::{
    IvyError, IvyJson, IvyResult, JsString,
    rpc::{RequestOptions, RpcClient},
    tools::{ToolCallOptions, call_bound_with_boundary, discover_with_boundary, truthy},
};
use ivy_contracts::validation::equal;

pub const CHAT_INTERFACE_VERSION: &str = "1.2.0";
#[derive(Clone, Debug)]
pub struct MainChat {
    pub service_node_id: JsString,
    pub expected_bridge: IvyJson,
    pub channel: IvyJson,
}
impl MainChat {
    pub fn to_json(&self) -> IvyJson {
        IvyJson::object([
            (
                "serviceNodeId",
                IvyJson::String(self.service_node_id.clone()),
            ),
            ("expectedBridge", self.expected_bridge.clone()),
            ("channel", self.channel.clone()),
        ])
    }
}
/// Preserve the original `instanceof IvyError` catch until the consumer decides
/// whether to swallow it. The error code alone cannot identify that catch origin.
#[derive(Clone, Debug, PartialEq)]
pub enum MainChatFailure {
    Ivy(IvyError),
    Boundary,
}
impl From<IvyError> for MainChatFailure {
    fn from(error: IvyError) -> Self {
        Self::Ivy(error)
    }
}
impl MainChatFailure {
    /// Normalize an uncoded boundary failure only after the original catch.
    pub fn into_error(self) -> IvyError {
        match self {
            Self::Ivy(error) => error,
            Self::Boundary => IvyError::internal(),
        }
    }
}
fn property<'a>(
    value: Option<&'a IvyJson>,
    key: &str,
) -> Result<Option<&'a IvyJson>, MainChatFailure> {
    Ok(value
        .filter(|v| !v.is_null())
        .ok_or(MainChatFailure::Boundary)?
        .get(key))
}
fn same_principal(left: Option<&IvyJson>, right: Option<&IvyJson>) -> bool {
    // Separate RPC results cannot share a JS object identity.
    if matches!(left, Some(IvyJson::Object(_) | IvyJson::Array(_)))
        || matches!(right, Some(IvyJson::Object(_) | IvyJson::Array(_)))
    {
        return false;
    }
    left == right
}
/// Hive selects the unique Main service; consumers do not configure a destination.
pub async fn main_chat(client: &dyn RpcClient) -> IvyResult<MainChat> {
    main_chat_checked(client)
        .await
        .map_err(MainChatFailure::into_error)
}
/// As `main_chat`, retaining coded-Ivy versus uncoded property-access failures.
/// `RpcClient` errors are already coded Ivy errors; their exact metadata is retained.
pub async fn main_chat_checked(client: &dyn RpcClient) -> Result<MainChat, MainChatFailure> {
    let bound = discover_with_boundary(
        client,
        &"chat.workspace".into(),
        &IvyJson::object([("interfaceVersion", CHAT_INTERFACE_VERSION.into())]),
        || MainChatFailure::Boundary,
    )
    .await?;
    let mut target = IvyJson::object(Vec::<(&str, IvyJson)>::new());
    if let Some(id) = bound.value().get("serviceNodeId") {
        target.insert("serviceNodeId", id.clone());
    }
    let node = client
        .request("serviceNodes.get", target, RequestOptions::default())
        .await?;
    if !equal(property(Some(&node), "serviceName")?, "chat-bridge")
        || !["connected", "synced", "ready"]
            .iter()
            .all(|key| truthy(node.get(key)))
    {
        return Err(IvyError::new(
            "chat_main_unavailable",
            "The Main ChatBridge is unavailable.",
        )
        .into());
    }
    let workspace = call_bound_with_boundary(
        client,
        &bound,
        IvyJson::object(Vec::<(&str, IvyJson)>::new()),
        None,
        ToolCallOptions::default(),
        || MainChatFailure::Boundary,
    )
    .await?;
    let unavailable = || {
        MainChatFailure::Ivy(IvyError::new(
            "chat_main_unavailable",
            "Main requires its single WhatsApp transport and native owner.",
        ))
    };
    let expected = property(Some(&workspace), "expectedBridge")?;
    if !same_principal(property(expected, "principalId")?, node.get("principalId"))
        || !truthy(workspace.get("nativeOwnerReady"))
    {
        return Err(unavailable());
    }
    // Keep the original && short-circuit: channels is not read after an earlier
    // failed requirement. JSON primitives box in JS; only null/undefined throws.
    let channels = property(Some(&workspace), "channels")?
        .filter(|v| !v.is_null())
        .ok_or(MainChatFailure::Boundary)?;
    let one = match channels {
        IvyJson::Array(values) => values.len() == 1,
        IvyJson::String(value) => value.0.len() == 1,
        value => value.get("length").and_then(IvyJson::as_f64) == Some(1.0),
    };
    if !one {
        return Err(unavailable());
    }
    let entry = match channels {
        IvyJson::Array(values) => values.first(),
        IvyJson::Object(_) => channels.get("0"),
        _ => return Err(MainChatFailure::Boundary),
    };
    let channel = property(entry, "channel")?;
    if !equal(property(channel, "adapter")?, "whatsapp") {
        return Err(unavailable());
    }
    Ok(MainChat {
        service_node_id: node
            .get("serviceNodeId")
            .and_then(IvyJson::as_js_string)
            .cloned()
            .ok_or(MainChatFailure::Boundary)?,
        expected_bridge: expected.ok_or(MainChatFailure::Boundary)?.clone(),
        channel: channel.ok_or(MainChatFailure::Boundary)?.clone(),
    })
}
/// Transport metadata stays in receipts; forward only the original notification content.
pub fn main_notification(value: &IvyJson) -> IvyJson {
    IvyJson::object(
        ["action", "operationId", "source", "text"]
            .into_iter()
            .filter_map(|key| value.get(key).map(|value| (key, value.clone()))),
    )
}
