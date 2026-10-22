use crate::{
    IvyError, IvyJson, IvyResult, JsString,
    rpc::{RequestOptions, RpcClient},
    tools::{ToolCallOptions, call_bound, discover},
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
/// Hive selects the unique Main service; consumers do not configure a destination.
pub async fn main_chat(client: &dyn RpcClient) -> IvyResult<MainChat> {
    let bound = discover(
        client,
        &"chat.workspace".into(),
        &IvyJson::object([("interfaceVersion", CHAT_INTERFACE_VERSION.into())]),
    )
    .await?;
    let node = client
        .request(
            "serviceNodes.get",
            IvyJson::object([("serviceNodeId", IvyJson::String(bound.service_node_id()?))]),
            RequestOptions::default(),
        )
        .await?;
    if !equal(node.get("serviceName"), "chat-bridge")
        || !["connected", "synced", "ready"]
            .iter()
            .all(|key| node.get(key).and_then(IvyJson::as_bool) == Some(true))
    {
        return Err(IvyError::new(
            "chat_main_unavailable",
            "The Main ChatBridge is unavailable.",
        ));
    }
    let workspace = call_bound(
        client,
        &bound,
        IvyJson::object(Vec::<(&str, IvyJson)>::new()),
        None,
        ToolCallOptions::default(),
    )
    .await?;
    let expected = workspace.required("expectedBridge")?;
    let channels = workspace
        .required("channels")?
        .as_array()
        .ok_or_else(IvyError::internal)?;
    if expected.get("principalId") != node.get("principalId")
        || workspace.get("nativeOwnerReady").and_then(IvyJson::as_bool) != Some(true)
        || channels.len() != 1
        || !equal(
            channels[0]
                .get("channel")
                .and_then(|value| value.get("adapter")),
            "whatsapp",
        )
    {
        return Err(IvyError::new(
            "chat_main_unavailable",
            "Main requires its single WhatsApp transport and native owner.",
        ));
    }
    Ok(MainChat {
        service_node_id: node
            .required("serviceNodeId")?
            .as_js_string()
            .cloned()
            .ok_or_else(IvyError::internal)?,
        expected_bridge: expected.clone(),
        channel: channels[0].required("channel")?.clone(),
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

/// Validate the public notification fields before projecting transport metadata.
pub fn main_notification_checked(value: &IvyJson) -> IvyResult<IvyJson> {
    for key in ["action", "operationId", "source", "text"] {
        value.required(key)?;
    }
    Ok(main_notification(value))
}
