//! Retained Main proof and bound provider calls for Secretary notice recovery.
use crate::inbox_evidence::InboxEvidence;
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString, hash::hash_json};
use ivy_sdk::{
    main_chat::CHAT_INTERFACE_VERSION,
    rpc::{RequestOptions, RpcClient},
    tools::{InterfaceRequirement, ToolCallOptions, ToolClients, call_bound},
};

fn need(ok: bool, message: &str) -> IvyResult<()> {
    if ok {
        Ok(())
    } else {
        Err(IvyError::new("secretary_notice_mismatch", message))
    }
}
fn same(a: &IvyJson, b: &IvyJson) -> IvyResult<bool> {
    Ok(hash_json(a)? == hash_json(b)?)
}
fn supported(read: &IvyJson, key: &str) -> IvyResult<bool> {
    Ok(
        read.required("object")?.get("contractKey") == Some(&key.into())
            && read.required("object")?.get("effectivelyArchived") != Some(&IvyJson::Bool(true))
            && matches!(
                read.required("revision")?
                    .required("contractVersion")?
                    .as_str()?
                    .as_str(),
                "1.2.0" | "1.3.0"
            )
            && read.required("content")?.get("encoding") == Some(&"json".into()),
    )
}

pub async fn notice_evidence(
    client: &dyn RpcClient,
    principal: &IvyJson,
    reply: &IvyJson,
    notice: &IvyJson,
) -> IvyResult<()> {
    let request = notice.required("request")?;
    let read = client
        .request(
            "objects.read",
            reply.required("object")?.clone(),
            RequestOptions::default(),
        )
        .await?;
    need(
        supported(&read, "chat-bridge/reply")?,
        "The provider reply must be one active supported ChatBridge reply.",
    )?;
    let value = reply.required("data")?;
    need(
        same(read.required("content")?.required("value")?, value)?,
        "The provider reply view differs from its saved delivery proof.",
    )?;
    need(
        read.required("revision")?.get("contentHash") == Some(&hash_json(value)?.into()),
        "The provider reply content hash differs from its saved delivery proof.",
    )?;
    let origin = value.required("origin")?;
    need(
        origin.get("kind") == Some(&"native".into()),
        "This reply was not written by the current Main.",
    )?;
    need(
        same(value.required("channel")?, request.required("channel")?)?,
        "This reply does not target the original Secretary notice channel.",
    )?;
    let input = client
        .request(
            "objects.read",
            IvyJson::object([("objectId", origin.required("inputId")?.clone())]),
            RequestOptions::default(),
        )
        .await?;
    need(
        supported(&input, "chat-bridge/input")?,
        "The Main reply must retain its exact hidden service handoff.",
    )?;
    let handoff = input.required("content")?.required("value")?;
    need(
        input.required("revision")?.get("contentHash") == Some(&hash_json(handoff)?.into())
            && handoff.get("operationId") == request.get("operationId")
            && handoff.required("identity")?.get("senderPrincipalId") == Some(principal)
            && same(
                handoff.required("identity")?.required("channel")?,
                request.required("channel")?,
            )?
            && same(handoff.required("binding")?, origin.required("binding")?)?
            && same(handoff.required("result")?, origin.required("result")?)?,
        "The Main reply does not belong to this Secretary handoff.",
    )
}

pub enum NoticeMethod {
    Notice,
    Notify,
    Operation,
}
impl NoticeMethod {
    fn name(&self) -> JsString {
        match self {
            Self::Notice => "chat.notice",
            Self::Notify => "chat.notify",
            Self::Operation => "chat.operation",
        }
        .into()
    }
}

/// ToolClients must belong to the evidence Store's same authenticated connection.
pub struct NoticeProvider<'a, 'engine, 'journal> {
    pub evidence: &'a InboxEvidence<'engine, 'journal>,
    pub tools: &'a ToolClients,
}
impl NoticeProvider<'_, '_, '_> {
    pub async fn call(
        &self,
        method: NoticeMethod,
        target: &IvyJson,
        args: IvyJson,
        operation_id: Option<JsString>,
    ) -> IvyResult<IvyJson> {
        self.evidence.verify_owner().await?;
        let client = self.evidence.store.client.as_ref();
        let id = target
            .required("serviceNodeId")?
            .as_js_string()
            .ok_or_else(IvyError::internal)?;
        let node = client
            .request(
                "serviceNodes.get",
                IvyJson::object([("serviceNodeId", IvyJson::String(id.clone()))]),
                RequestOptions::default(),
            )
            .await?;
        if node.get("serviceName") != Some(&"chat-bridge".into())
            || node.get("principalId") != target.required("expectedBridge")?.get("principalId")
            || !["connected", "synced", "ready"]
                .iter()
                .all(|key| node.get(key).and_then(IvyJson::as_bool) == Some(true))
        {
            return Err(IvyError::new(
                "secretary_notice_unavailable",
                "The selected ChatBridge owner is not currently ready.",
            ));
        }
        let selected = self.tools.service_tools(
            id.clone(),
            &[InterfaceRequirement {
                namespace: "chat".into(),
                interface_version: CHAT_INTERFACE_VERSION.into(),
            }],
        )?;
        let bound = selected.binding(&method.name(), None).await?;
        call_bound(
            client,
            &bound,
            args,
            operation_id,
            ToolCallOptions::default(),
        )
        .await
    }
}
