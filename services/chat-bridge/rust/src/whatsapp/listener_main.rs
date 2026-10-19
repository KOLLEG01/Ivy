//! Borrow the existing outbox/store owners for one immutable connection lease.
use crate::{
    generation::ChatGenerationLease,
    outbox::ChatOutbox,
    store::{Document, Pin, Reference},
};
use ivy_contracts::{IvyError, IvyJson as Json, IvyResult, JsString};
use ivy_sdk::{operation_ids::scoped_operation_id, rpc::RequestFuture};

pub trait ListenerMain: Send + Sync {
    fn definition_hash(&self) -> &str;
    fn expected(&self, sender: &JsString) -> Json;
    fn is_notice_input<'a>(&'a self, id: &'a JsString) -> RequestFuture<'a>;
    fn operation_id(&self, scope: Json) -> RequestFuture<'_>;
    fn operation<'a>(&'a self, sender: &'a JsString, id: &'a Json) -> RequestFuture<'a>;
    fn history<'a>(&'a self, sender: &'a JsString, request: &'a Json) -> RequestFuture<'a>;
    fn receive<'a>(&'a self, sender: &'a JsString, request: &'a Json) -> RequestFuture<'a>;
    fn acknowledge<'a>(&'a self, sender: &'a JsString, request: &'a Json) -> RequestFuture<'a>;
    fn result<'a>(&'a self, reference: &'a Json, input: &'a Json) -> RequestFuture<'a>;
    fn reply<'a>(&'a self, id: &'a Json, channel: &'a Json) -> RequestFuture<'a>;
    fn pending_notices(&self, cursor: Option<JsString>) -> RequestFuture<'_>;
    fn notice_parts<'a>(&'a self, first: &'a Json) -> RequestFuture<'a>;
    fn confirm_notice<'a>(&'a self, parts: &'a [Json]) -> RequestFuture<'a>;
}
fn pin(value: &Json) -> IvyResult<Pin> {
    Ok(Pin {
        object_id: value.required("objectId")?.as_str()?,
        revision: value
            .required("revision")?
            .as_f64()
            .ok_or_else(IvyError::internal)? as i64,
    })
}
fn document(value: &Json) -> IvyResult<Document> {
    Ok(Document {
        key: value.required("key")?.as_str()?,
        pin: pin(value.required("pin")?)?,
        value: value.required("value")?.clone(),
        metadata: value.required("metadata")?.clone(),
        revision: value.required("revision")?.clone(),
    })
}
fn view(value: Document) -> Json {
    Json::object([
        ("key", value.key.into()),
        ("pin", value.pin.json()),
        ("value", value.value),
        ("metadata", value.metadata),
        ("revision", value.revision),
    ])
}
impl ListenerMain for ChatGenerationLease {
    fn definition_hash(&self) -> &str {
        self.config().admission.definition_hash()
    }
    fn expected(&self, sender: &JsString) -> Json {
        self.config().admission.expected(sender)
    }
    fn is_notice_input<'a>(&'a self, id: &'a JsString) -> RequestFuture<'a> {
        Box::pin(async move {
            Ok(self
                .store()
                .is_notice_input(&String::from_utf16_lossy(&id.0))
                .await
                .map_err(|e| e.into_ivy())?
                .into())
        })
    }
    fn operation_id(&self, scope: Json) -> RequestFuture<'_> {
        Box::pin(async move {
            Ok(Json::String(
                scoped_operation_id(self.store().client.clone(), &scope).await?,
            ))
        })
    }
    fn operation<'a>(&'a self, sender: &'a JsString, id: &'a Json) -> RequestFuture<'a> {
        Box::pin(async move {
            let id = id.as_js_string().ok_or_else(IvyError::internal)?;
            Ok(self
                .operations()?
                .find(sender, id)
                .await?
                .map(view)
                .unwrap_or(Json::Null))
        })
    }
    fn history<'a>(&'a self, sender: &'a JsString, request: &'a Json) -> RequestFuture<'a> {
        Box::pin(async move {
            ChatOutbox::new(&self.operations()?)
                .history(sender, request)
                .await
        })
    }
    fn receive<'a>(&'a self, sender: &'a JsString, request: &'a Json) -> RequestFuture<'a> {
        Box::pin(async move {
            ChatOutbox::new(&self.operations()?)
                .receive(sender, request)
                .await
        })
    }
    fn acknowledge<'a>(&'a self, sender: &'a JsString, request: &'a Json) -> RequestFuture<'a> {
        Box::pin(async move {
            ChatOutbox::new(&self.operations()?)
                .acknowledge(sender, request)
                .await
        })
    }
    fn result<'a>(&'a self, reference: &'a Json, input: &'a Json) -> RequestFuture<'a> {
        Box::pin(async move {
            let reference = pin(reference)?;
            Ok(view(
                self.store()
                    .read_in(
                        "chat-bridge/result",
                        Reference::Pinned(&reference),
                        Some(&input.as_str()?),
                    )
                    .await
                    .map_err(|e| e.into_ivy())?,
            ))
        })
    }
    fn reply<'a>(&'a self, id: &'a Json, channel: &'a Json) -> RequestFuture<'a> {
        Box::pin(async move {
            Ok(view(
                ChatOutbox::new(&self.operations()?)
                    .reply(Reference::Current(&id.as_str()?), channel)
                    .await?,
            ))
        })
    }
    fn pending_notices(&self, cursor: Option<JsString>) -> RequestFuture<'_> {
        Box::pin(async move {
            let page = ChatOutbox::new(&self.operations()?)
                .pending_notices(cursor.as_ref())
                .await?;
            let mut result = Json::object([(
                "replies",
                Json::Array(page.replies.into_iter().map(view).collect()),
            )]);
            if let Some(cursor) = page.next_cursor {
                result.insert("nextCursor", cursor);
            }
            Ok(result)
        })
    }
    fn notice_parts<'a>(&'a self, first: &'a Json) -> RequestFuture<'a> {
        Box::pin(async move {
            Ok(Json::Array(
                ChatOutbox::new(&self.operations()?)
                    .notice_parts(&document(first)?)
                    .await?
                    .into_iter()
                    .map(view)
                    .collect(),
            ))
        })
    }
    fn confirm_notice<'a>(&'a self, parts: &'a [Json]) -> RequestFuture<'a> {
        Box::pin(async move {
            let parts = parts.iter().map(document).collect::<IvyResult<Vec<_>>>()?;
            ChatOutbox::new(&self.operations()?)
                .confirm_notice(&parts)
                .await?;
            Ok(Json::Null)
        })
    }
}
