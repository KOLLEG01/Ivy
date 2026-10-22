//! Original inbox identities and retained-record checks used before recovery effects.
use crate::store::{Record, Store};
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString, hash::hash_json};
use ivy_sdk::{connection_owner::connection_owner, rpc::RequestOptions};

/// Check the live Secretary identity without borrowing the local journal. This
/// also serves the native dispatch guard, which must be Send + Sync.
pub async fn verify_owner(
    client: &dyn ivy_sdk::rpc::RpcClient,
    service_node_id: &JsString,
    settings: &IvyJson,
    cancellation: &RequestOptions,
) -> IvyResult<()> {
    if cancellation.cancellation.is_cancelled() {
        return Err(cancellation.cancel_error.clone());
    }
    let owner = connection_owner(client, service_node_id).await?;
    let identity = settings.required("identity")?;
    need(
        owner
            .get("connected")
            .is_some_and(|value| value == &IvyJson::Bool(true))
            && owner
                .get("synced")
                .is_some_and(|value| value == &IvyJson::Bool(true))
            && owner.get("principalId") == identity.get("principalId")
            && owner.get("hostId") == identity.get("hostId")
            && owner.get("serviceName") == Some(&IvyJson::from("secretary")),
        "secretary_owner_mismatch",
        "Secretary work requires its original authenticated live owner.",
    )
}

fn need(ok: bool, code: &str, message: &str) -> IvyResult<()> {
    if ok {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn same(a: &IvyJson, b: &IvyJson) -> IvyResult<bool> {
    Ok(hash_json(a)? == hash_json(b)?)
}
fn without(value: &IvyJson, keys: &[&str]) -> IvyJson {
    let mut result = value.clone();
    for key in keys {
        result.remove(key);
    }
    result
}
pub fn operation_name(caller: &IvyJson, id: &IvyJson) -> IvyResult<JsString> {
    Ok(format!(
        "operation-{}",
        &hash_json(&IvyJson::Array(vec![caller.clone(), id.clone()]))?[7..]
    )
    .into())
}
pub fn source_name(id: &IvyJson) -> IvyResult<JsString> {
    Ok(format!("source-{}", &hash_json(id)?[7..]).into())
}
pub fn message_hash(message: &IvyJson) -> IvyResult<String> {
    hash_json(&without(message, &["observedAt"]))
}
pub fn identity_hash(source_id: &IvyJson, message: &IvyJson) -> IvyResult<String> {
    hash_json(&IvyJson::Array(vec![
        source_id.clone(),
        message.required("accountId")?.clone(),
        message.required("conversationId")?.clone(),
        message.required("messageId")?.clone(),
        message.required("nativeRevision")?.clone(),
    ]))
}
pub fn source_identity(source: &IvyJson) -> IvyResult<IvyJson> {
    Ok(IvyJson::object([
        ("sourceId", source.required("sourceId")?.clone()),
        ("accountId", source.required("accountId")?.clone()),
        ("kind", source.required("kind")?.clone()),
    ]))
}
pub(crate) fn ignored_reason(
    source: &IvyJson,
    message: &IvyJson,
) -> IvyResult<Option<&'static str>> {
    if message
        .get("outgoing")
        .is_some_and(|value| value == &IvyJson::Bool(true))
    {
        return Ok(Some("outgoing"));
    }
    let sender = message.required("senderId")?;
    if source
        .required("ownSenderIds")?
        .as_array()
        .ok_or_else(IvyError::internal)?
        .contains(sender)
    {
        return Ok(Some("own_sender"));
    }
    if source
        .get("allowedSenderIds")
        .filter(|v| !(v).is_null())
        .and_then(IvyJson::as_array)
        .is_some_and(|allowed| !allowed.contains(sender))
    {
        return Ok(Some("sender_not_allowed"));
    }
    Ok(None)
}
pub fn notice_text(
    assessment: &IvyJson,
    message: &IvyJson,
    source: &IvyJson,
) -> IvyResult<JsString> {
    let sender_id = message.required("senderId")?;
    let title = message.required("title")?;
    let email = source.get("kind") == Some(&IvyJson::from("email"));
    let internal = sender_id.as_js_string().is_some_and(|s| {
        s.0.starts_with(&"user:".encode_utf16().collect::<Vec<_>>())
            || s.0
                .starts_with(&"application:".encode_utf16().collect::<Vec<_>>())
    });
    let fallback = IvyJson::from("Microsoft Teams");
    let sender = if email {
        sender_id
    } else if internal {
        if (title)
            .as_js_string()
            .is_some_and(|text| !text.0.is_empty())
        {
            title
        } else {
            &fallback
        }
    } else if (sender_id)
        .as_js_string()
        .is_some_and(|text| !text.0.is_empty())
    {
        sender_id
    } else {
        title
    };
    let body = if email
        || !(message.required("text")?)
            .as_js_string()
            .is_some_and(|text| !text.0.is_empty())
    {
        assessment.required("summary")?
    } else {
        message.required("text")?
    };
    let mut text = format!(
        "[{}] {}",
        message.required("accountId")?.as_str()?,
        sender.as_str()?
    );
    if email
        && (title)
            .as_js_string()
            .is_some_and(|text| !text.0.is_empty())
    {
        text.push_str(" – ");
        text.push_str(&title.as_str()?);
    }
    text.push_str(": ");
    text.push_str(&body.as_str()?);
    Ok(JsString(text.encode_utf16().take(8192).collect()))
}

/// Borrows the engine's admitted settings, journal and generation cancellation.
/// This component checks evidence; it does not admit requests or dispatch effects.
pub struct InboxEvidence<'a, 'journal> {
    pub store: &'a Store<'journal>,
    pub settings: &'a IvyJson,
    pub service_node_id: &'a JsString,
    pub cancellation: RequestOptions,
}
impl InboxEvidence<'_, '_> {
    pub fn source_definition(&self, id: &IvyJson) -> IvyResult<&IvyJson> {
        self.settings
            .required("sources")?
            .as_array()
            .ok_or_else(IvyError::internal)?
            .iter()
            .find(|source| source.get("sourceId") == Some(id))
            .ok_or_else(|| {
                IvyError::new(
                    "secretary_source_unknown",
                    "The selected source is not configured.",
                )
            })
    }
    pub fn authorize(
        &self,
        caller: &IvyJson,
        scope: &IvyJson,
        action: &str,
        source_id: Option<&IvyJson>,
    ) -> IvyResult<()> {
        need(
            same(
                scope,
                self.settings.required("identity")?.required("scope")?,
            )?,
            "secretary_scope_conflict",
            "Select this exact Secretary scope.",
        )?;
        if matches!(action, "capture" | "checkpoint" | "attach") {
            let allowed = match source_id.filter(|id| !(id).is_null()) {
                Some(id) => self
                    .source_definition(id)?
                    .required("producerPrincipalIds")?
                    .as_array()
                    .ok_or_else(IvyError::internal)?
                    .contains(caller),
                None => false,
            };
            need(
                allowed,
                "forbidden",
                "Only this source’s admitted producer may capture or advance it.",
            )?;
        }
        Ok(())
    }
    pub async fn verify_owner(&self) -> IvyResult<()> {
        verify_owner(
            self.store.client.as_ref(),
            self.service_node_id,
            self.settings,
            &self.cancellation,
        )
        .await
    }
    pub async fn source(&self, id: &IvyJson) -> IvyResult<IvyJson> {
        let source = self
            .store
            .named(Record::Source, &source_name(id)?)
            .await?
            .ok_or_else(|| {
                IvyError::new(
                    "secretary_evidence_missing",
                    "The original source checkpoint is missing.",
                )
            })?;
        let value = source.required("value")?;
        let saved = value.required("definition")?;
        let configured = self
            .settings
            .required("sources")?
            .as_array()
            .ok_or_else(IvyError::internal)?
            .iter()
            .find(|source| source.get("sourceId") == Some(id));
        need(
            saved.get("sourceId") == Some(id)
                && value.get("definitionHash") == Some(&hash_json(saved)?.into())
                && match configured {
                    None => true,
                    Some(current) => same(&source_identity(current)?, &source_identity(saved)?)?,
                },
            "secretary_identity_conflict",
            "A source checkpoint cannot change its original source/account definition.",
        )?;
        Ok(source)
    }
    pub async fn item(&self, selected: &IvyJson) -> IvyResult<IvyJson> {
        let item = self.store.read(Record::Item, selected).await?;
        let value = item.required("value")?;
        let message = value.required("message")?;
        let source = self.source(value.required("sourceId")?).await?;
        let definition = source.required("value")?.required("definition")?;
        need(
            value.get("identityHash")
                == Some(&identity_hash(value.required("sourceId")?, message)?.into())
                && value.get("messageHash") == Some(&message_hash(message)?.into())
                && message.get("accountId") == definition.get("accountId"),
            "secretary_evidence_conflict",
            "The item must retain its original source, message and content identity.",
        )?;
        if let Some(decision) = value.get("decision").filter(|v| !(v).is_null()) {
            let valid = match value.get("notice").filter(|v| !(v).is_null()) {
                Some(notice) => {
                    notice.get("text")
                        == Some(&IvyJson::String(notice_text(
                            decision.required("assessment")?,
                            message,
                            definition,
                        )?))
                        && notice.required("source")?.get("objectId")
                            == item.required("pin")?.get("objectId")
                }
                None => false,
            };
            need(
                valid,
                "secretary_evidence_conflict",
                "The assessment and original notification source cannot change.",
            )?;
        } else {
            need(
                !value.get("notice").is_some_and(|value| !value.is_null()),
                "secretary_evidence_conflict",
                "An unassessed item has no notice.",
            )?;
        }
        Ok(item)
    }
    pub async fn checked(&self, operation: &IvyJson) -> IvyResult<IvyJson> {
        let original = self.store.original(Record::Operation, operation).await?;
        let value = operation.required("value")?;
        let first = original.required("value")?;
        let immutable = |v: &IvyJson| without(v, &["phase", "effect", "errorCode"]);
        let valid = first.get("phase") == Some(&"accepted".into())
            && first.get("effect") == Some(&IvyJson::Null)
            && first.get("errorCode") == Some(&IvyJson::Null)
            && same(&immutable(first), &immutable(value))?;
        let request_valid = if !valid {
            false
        } else if let Some(request) = value.get("request").filter(|v| !(v).is_null()) {
            request.get("action") == value.get("action")
                && request.get("operationId") == value.get("operationId")
                && value.get("requestHash") == Some(&hash_json(request)?.into())
                && same(
                    request.required("expectedScope")?,
                    self.settings.required("identity")?.required("scope")?,
                )?
        } else {
            value.get("action") == Some(&"capture".into())
                && value
                    .get("ignoredReason")
                    .and_then(IvyJson::as_js_string)
                    .is_some_and(|text| !text.0.is_empty())
                && !value.get("prepared").is_some_and(|value| !value.is_null())
        };
        need(
            valid && request_valid,
            "secretary_evidence_conflict",
            "The original accepted action and prepared bytes cannot be rewritten.",
        )?;
        let source = self.source(value.required("sourceId")?).await?;
        let accepted = value.required("source")?;
        let definition = accepted.required("definition")?;
        need(
            accepted.get("definitionHash") == Some(&hash_json(definition)?.into())
                && same(
                    &source_identity(definition)?,
                    &source_identity(source.required("value")?.required("definition")?)?,
                )?,
            "secretary_evidence_conflict",
            "The accepted source configuration must retain its original identity.",
        )?;
        Ok(operation.clone())
    }
    pub async fn find(&self, caller: &IvyJson, id: &IvyJson) -> IvyResult<Option<IvyJson>> {
        let Some(operation) = self
            .store
            .named(Record::Operation, &operation_name(caller, id)?)
            .await?
        else {
            return Ok(None);
        };
        let value = operation.required("value")?;
        need(
            value.get("callerPrincipalId") == Some(caller) && value.get("operationId") == Some(id),
            "secretary_identity_conflict",
            "This operation belongs to another caller or ID.",
        )?;
        Ok(Some(self.checked(&operation).await?))
    }
    pub async fn operation(
        &self,
        caller: &IvyJson,
        scope: &IvyJson,
        id: &IvyJson,
    ) -> IvyResult<IvyJson> {
        self.authorize(caller, scope, "operation", None)?;
        self.verify_owner().await?;
        let operation = self.find(caller, id).await?.ok_or_else(|| {
            IvyError::new(
                "not_found",
                "The caller has no retained operation with this ID.",
            )
        })?;
        Ok(operation.required("value")?.clone())
    }
}
