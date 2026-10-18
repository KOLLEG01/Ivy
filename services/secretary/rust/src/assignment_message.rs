//! Producer-independent message-channel admission using shared Date.parse semantics.
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString};
use ivy_sdk::clock::parse_date;

pub const MESSAGE_WINDOW_KEY: &str = "secretary/channel-window";
pub const MESSAGE_BATCH_KIND: &str = "secretary.channel.batch";
pub const MESSAGE_WINDOW_MS: i64 = 5 * 60_000;

pub fn message_channel(payload: &IvyJson) -> Option<IvyJson> {
    let value = payload.get("messageChannel")?;
    let key = value.get("key")?.as_js_string()?;
    let occurred_at = value.get("occurredAt")?.as_js_string()?;
    if key.0.is_empty() || !parse_date(occurred_at).is_finite() {
        return None;
    }
    let reference = value.get("reference")?;
    reference.as_object()?;
    let absent = IvyJson::Null;
    let item_id = value.get("itemId").unwrap_or(&absent);
    if !item_id.is_null() && item_id.as_js_string().is_none() {
        return None;
    }
    Some(IvyJson::object([
        ("key", IvyJson::String(key.clone())),
        ("reference", reference.clone()),
        ("occurredAt", IvyJson::String(occurred_at.clone())),
        ("itemId", item_id.clone()),
    ]))
}

pub fn message_batch(payload: &IvyJson) -> Option<IvyJson> {
    let channel = payload.get("channel")?;
    message_channel(&IvyJson::object([("messageChannel", channel.clone())]))?;
    if payload.get("kind")?.as_js_string()? != &JsString::from(MESSAGE_BATCH_KIND)
        || payload.get("dueAt")?.as_js_string().is_none()
        || payload.get("serviceNodeId")?.as_js_string().is_none()
    {
        return None;
    }
    for name in ["itemIds", "eventIds"] {
        if !payload
            .get(name)?
            .as_array()?
            .iter()
            .all(|id| id.as_js_string().is_some())
        {
            return None;
        }
    }
    // Validation uses a normalized channel, but the original returns the supplied
    // batch unchanged, including extra fields and omitted channel.itemId.
    Some(payload.clone())
}

pub fn message_unread_snapshot(observation: &IvyJson, due_at: &JsString) -> IvyResult<bool> {
    let pending = || {
        IvyError::new(
            "secretary_message_unread_pending",
            "The channel unread state is incomplete or stale.",
        )
    };
    if observation.get("complete").and_then(IvyJson::as_bool) != Some(true) {
        return Err(pending());
    }
    let unread = observation
        .get("unread")
        .and_then(IvyJson::as_bool)
        .ok_or_else(pending)?;
    let observed_at = observation
        .get("observedAt")
        .and_then(IvyJson::as_js_string)
        .ok_or_else(pending)?;
    if !parse_date(observed_at).is_finite() || parse_date(observed_at) < parse_date(due_at) {
        return Err(pending());
    }
    Ok(unread)
}

/// Verify unread evidence from the exact batch producer. The ToolClients cache
/// must belong to this same RPC connection. All failures retain the source's
/// pending result, without inventing a read or advancing the event window.
pub async fn message_conversation_unread(
    client: &dyn ivy_sdk::rpc::RpcClient,
    tools: &ivy_sdk::tools::ToolClients,
    root_object_id: &JsString,
    batch: &IvyJson,
) -> IvyResult<bool> {
    use ivy_sdk::{
        rpc::RequestOptions,
        tools::{InterfaceRequirement, ToolCallOptions},
    };
    let attempt = async {
        let service_node_id = batch
            .required("serviceNodeId")?
            .as_js_string()
            .ok_or_else(IvyError::internal)?;
        let node = client
            .request(
                "serviceNodes.get",
                IvyJson::object([("serviceNodeId", IvyJson::String(service_node_id.clone()))]),
                RequestOptions::default(),
            )
            .await?;
        if ["connected", "synced", "ready"]
            .iter()
            .any(|key| node.get(key).and_then(IvyJson::as_bool) != Some(true))
            || node.get("stale").and_then(IvyJson::as_bool) == Some(true)
        {
            return Err(IvyError::new(
                "secretary_message_unread_pending",
                "The message source is unavailable.",
            ));
        }
        let source = tools.service_tools(
            service_node_id.clone(),
            &[InterfaceRequirement {
                namespace: "message-channel".into(),
                interface_version: "1.0.0".into(),
            }],
        )?;
        let observation = source
            .call(
                &"message-channel.unread".into(),
                IvyJson::object([
                    ("rootObjectId", IvyJson::String(root_object_id.clone())),
                    (
                        "reference",
                        batch.required("channel")?.required("reference")?.clone(),
                    ),
                    ("notBefore", batch.required("dueAt")?.clone()),
                ]),
                None,
                ToolCallOptions::default(),
            )
            .await?;
        message_unread_snapshot(
            &observation,
            batch
                .required("dueAt")?
                .as_js_string()
                .ok_or_else(IvyError::internal)?,
        )
    }
    .await;
    attempt.map_err(|_| {
        IvyError::new(
            "secretary_message_unread_pending",
            "The channel unread state cannot be verified.",
        )
    })
}
