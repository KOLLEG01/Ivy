//! Durable publication of a separately verified complete collection. Dispatch
//! and collection authority remain with the Main/collector source.
use crate::{
    main_context::{spread, truthy},
    notice_input::attributed_notice_reply,
    operations::{ChatOperations, contention},
    queue::ChatQueue,
    result_text::{chat_reply_parts, read_chat_result_text},
    store::{Destination, Document, Pin, Reference, mutation},
};
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString,
    hash::hash_json,
    json::{canonical, try_js_string},
    limits::MANAGEMENT_FRAME_BYTES,
};
use std::{future::Future, pin::Pin as FuturePin, sync::Arc};

pub type PublicationFuture<'a, T> = FuturePin<Box<dyn Future<Output = IvyResult<T>> + Send + 'a>>;
/// Main recovers its pending action, delivery supplies the live head, and the
/// collector verifies the unchanged complete collection before publication.
pub trait PublicationSource: Send + Sync {
    fn recover_pending(&self) -> PublicationFuture<'_, ()>;
    fn verify_owner(&self) -> PublicationFuture<'_, ()>;
    fn head(&self) -> PublicationFuture<'_, Option<Document>>;
    fn find<'a>(&'a self, input_id: &'a str) -> PublicationFuture<'a, Option<Document>>;
    fn collect<'a>(&'a self, input_id: &'a str) -> PublicationFuture<'a, Option<Document>>;
}
fn require(ok: bool, code: &str, message: &str) -> IvyResult<()> {
    if ok {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn field<'a>(value: &'a Json, key: &str) -> IvyResult<&'a Json> {
    value.get(key).ok_or_else(IvyError::internal)
}
fn string<'a>(value: &'a Json, key: &str) -> IvyResult<&'a JsString> {
    field(value, key)?
        .as_js_string()
        .ok_or_else(IvyError::internal)
}
fn number(value: &Json, key: &str) -> IvyResult<f64> {
    field(value, key)?.as_f64().ok_or_else(IvyError::internal)
}
fn pin(value: &Json) -> IvyResult<Pin> {
    Ok(Pin {
        object_id: field(value, "objectId")?.as_str()?,
        revision: number(value, "revision")? as i64,
    })
}
fn same(a: &Json, b: &Json) -> IvyResult<bool> {
    Ok(canonical(a, MANAGEMENT_FRAME_BYTES)? == canonical(b, MANAGEMENT_FRAME_BYTES)?)
}
fn safe_integer(value: f64) -> bool {
    value.is_finite() && value.fract() == 0.0 && value.abs() <= 9_007_199_254_740_991.0
}
fn head_id(main: &Json) -> Option<&Json> {
    main.get("queue")
        .and_then(Json::as_array)
        .and_then(|q| q.first())
        .and_then(|t| t.get("inputId"))
}
fn original_claim(claim: &Json) -> Json {
    let mut value = spread(claim);
    value.remove("publishedParts");
    value
}
struct Material {
    collection: Pin,
    result: Document,
    parts: Vec<Json>,
}
pub struct ChatPublisher<'a> {
    pub operations: &'a ChatOperations<'a>,
    source: &'a dyn PublicationSource,
    cache: Option<Arc<Material>>,
}
impl<'a> ChatPublisher<'a> {
    pub fn new(operations: &'a ChatOperations<'a>, source: &'a dyn PublicationSource) -> Self {
        Self {
            operations,
            source,
            cache: None,
        }
    }
    async fn main(&self) -> IvyResult<Document> {
        ChatQueue::new(self.operations, None).main().await
    }
    async fn material(&mut self, input_id: &str) -> IvyResult<Option<Arc<Material>>> {
        let Some(selected) = self.source.find(input_id).await? else {
            return Ok(None);
        };
        if selected.value.get("state") != Some(&"complete".into()) {
            return Ok(None);
        }
        if let Some(cache) = &self.cache {
            if cache.result.value.get("inputId") == Some(&input_id.into())
                && same(&selected.pin.json(), &cache.collection.json())?
                && same(field(&selected.value, "result")?, &cache.result.pin.json())?
            {
                return Ok(Some(cache.clone()));
            }
        }
        let verified = self.source.collect(input_id).await?;
        require(
            verified
                .as_ref()
                .is_some_and(|v| v.value.get("state") == Some(&"complete".into()))
                && verified.as_ref().is_some_and(|v| v.pin == selected.pin),
            "revision_conflict",
            "Publication must pin the unchanged complete original collection.",
        )?;
        let result_pin = pin(field(&verified.unwrap().value, "result")?)?;
        let result = self
            .operations
            .store
            .read_in(
                "chat-bridge/result",
                Reference::Pinned(&result_pin),
                Some(input_id),
            )
            .await
            .map_err(|e| e.into_ivy())?;
        let input = self
            .operations
            .store
            .read("chat-bridge/input", Reference::Current(input_id))
            .await
            .map_err(|e| e.into_ivy())?;
        let mut text =
            read_chat_result_text(self.operations.store, &input_id.into(), &result.value).await?;
        let operation = self
            .operations
            .find(
                string(field(&input.value, "identity")?, "senderPrincipalId")?,
                string(&input.value, "operationId")?,
            )
            .await?;
        require(
            operation.is_some(),
            "chat_identity_conflict",
            "Publication requires the input's original operation.",
        )?;
        let operation = operation.unwrap();
        if field(&operation.value, "request")?.get("action") == Some(&"notify".into()) {
            text = attributed_notice_reply(string(&operation.value, "callerPrincipalId")?, &text)?;
        }
        let parts = chat_reply_parts(&result.value, &text)?;
        let material = Arc::new(Material {
            collection: selected.pin.clone(),
            result,
            parts,
        });
        self.cache = Some(material.clone());
        Ok(Some(material))
    }
    async fn write_main(&self, current: &Document, next: &Json) -> IvyResult<()> {
        self.source.verify_owner().await?;
        self.operations
            .store
            .write(
                "chat-bridge/main",
                next,
                &mutation(
                    &current.pin.object_id,
                    &format!("publish:{}:{}", current.pin.revision, hash_json(next)?),
                )?,
                Destination::Update {
                    object_id: &current.pin.object_id,
                    expected_revision: current.pin.revision,
                },
            )
            .await
            .map_err(|e| e.into_ivy())?;
        Ok(())
    }
    async fn claimed(&self, claim: &Json) -> IvyResult<Document> {
        let current = self.main().await?;
        require(
            !current.value.get("pendingAction").is_some_and(truthy)
                && head_id(&current.value) == claim.get("inputId")
                && current.value.get("publication").is_some_and(truthy)
                && same(
                    &original_claim(field(&current.value, "publication")?),
                    &original_claim(claim),
                )?,
            "revision_conflict",
            "Only the current original publication may finish its queue head.",
        )?;
        Ok(current)
    }
    fn terminal(input: &Json, result: &Json) -> IvyResult<Json> {
        let completed = result.get("nativeState") == Some(&"completed".into());
        let cancelled = result.get("nativeState") == Some(&"interrupted".into())
            && input.get("cancellation") != Some(&Json::Null);
        let state = if completed {
            "completed"
        } else if cancelled {
            "cancelled"
        } else {
            "failed"
        };
        let error = if completed || cancelled {
            Json::Null
        } else {
            let native = try_js_string(field(result, "nativeState")?)?.to_utf8()?;
            Json::object([
                ("code", format!("chat_native_turn_{native}").into()),
                ("outcome", "unknown".into()),
                (
                    "detail",
                    format!("The original native turn {native}. Its complete result is retained.")
                        .into(),
                ),
            ])
        };
        Ok(Json::object([
            ("state", state.into()),
            ("finishedAt", field(result, "createdAt")?.clone()),
            ("updatedAt", field(result, "createdAt")?.clone()),
            ("error", error),
        ]))
    }
    async fn finish_notice(&self, input: &Document, claim: &Json) -> IvyResult<()> {
        let operation = self
            .operations
            .find(
                string(field(&input.value, "identity")?, "senderPrincipalId")?,
                string(&input.value, "operationId")?,
            )
            .await?;
        require(
            operation.is_some(),
            "chat_identity_conflict",
            "Publication requires the input's original operation.",
        )?;
        let operation = operation.unwrap();
        let request = field(&operation.value, "request")?;
        if request.get("action") != Some(&"notify".into()) {
            return Ok(());
        }
        let reply = self
            .operations
            .store
            .named(
                "chat-bridge/reply",
                &format!("Chat reply {} part 0", input.pin.object_id),
            )
            .await
            .map_err(|e| e.into_ivy())?;
        let valid = if let Some(reply) = &reply {
            let origin = field(&reply.value, "origin")?;
            origin.get("kind") == Some(&"native".into())
                && origin.get("inputId") == Some(&input.pin.object_id.clone().into())
                && same(field(origin, "result")?, field(claim, "result")?)?
                && same(field(&reply.value, "channel")?, field(request, "channel")?)?
        } else {
            false
        };
        require(
            valid,
            "chat_identity_conflict",
            "A completed service handoff needs Main's exact first published reply.",
        )?;
        let reply = reply.unwrap();
        self.operations
            .finish(
                &operation.pin.object_id,
                &Json::object([
                    ("action", "notify".into()),
                    (
                        "operationId",
                        field(&operation.value, "operationId")?.clone(),
                    ),
                    (
                        "reply",
                        Json::object([("object", reply.pin.json()), ("data", reply.value)]),
                    ),
                ]),
            )
            .await?;
        Ok(())
    }
    pub async fn step(&mut self) -> IvyResult<()> {
        match self.step_inner().await {
            Err(error) if contention(&error) => Ok(()),
            result => result,
        }
    }
    async fn step_inner(&mut self) -> IvyResult<()> {
        self.source.recover_pending().await?;
        let Some(input) = self.source.head().await? else {
            return Ok(());
        };
        if !input.value.get("turnId").is_some_and(truthy) {
            return Ok(());
        }
        let Some(material) = self.material(&input.pin.object_id).await? else {
            return Ok(());
        };
        let mut current = self.main().await?;
        require(
            !current.value.get("pendingAction").is_some_and(truthy)
                && head_id(&current.value) == Some(&input.pin.object_id.clone().into()),
            "revision_conflict",
            "Publication requires its unchanged current queue head.",
        )?;
        let result = &material.result;
        let parts = &material.parts;
        if !current.value.get("publication").is_some_and(truthy) {
            let first = number(&current.value, "nextSequence")?;
            let end = first + parts.len() as f64;
            require(
                safe_integer(end),
                "chat_sequence_exhausted",
                "The complete reply range cannot exceed the workspace sequence bound.",
            )?;
            let claim = Json::object([
                ("inputId", input.pin.object_id.clone().into()),
                ("collection", material.collection.json()),
                ("result", result.pin.json()),
                ("firstSequence", Json::Number(first)),
                ("partCount", Json::Number(parts.len() as f64)),
                ("publishedParts", Json::Number(0.0)),
                ("createdAt", field(&result.value, "createdAt")?.clone()),
            ]);
            let mut next = current.value.clone();
            next.insert("publication", claim.clone());
            next.insert("nextSequence", Json::Number(end));
            next.insert("updatedAt", field(&claim, "createdAt")?.clone());
            return self.write_main(&current, &next).await;
        }
        let claim = field(&current.value, "publication")?.clone();
        require(
            same(field(&claim, "collection")?, &material.collection.json())?
                && same(field(&claim, "result")?, &result.pin.json())?
                && claim.get("partCount") == Some(&Json::Number(parts.len() as f64))
                && claim.get("createdAt") == result.value.get("createdAt")
                && same(
                    field(&result.value, "binding")?,
                    field(&input.value, "binding")?,
                )?
                && result.value.get("turnId") == input.value.get("turnId"),
            "chat_identity_conflict",
            "Publication must keep its original complete result, binding, turn and reply parts.",
        )?;
        let terminal = Self::terminal(&input.value, &result.value)?;
        if !input.value.get("result").is_some_and(truthy) {
            require(
                !input.value.get("finishedAt").is_some_and(truthy),
                "chat_identity_conflict",
                "A different terminal input cannot be replaced by a collected result.",
            )?;
            self.source.verify_owner().await?;
            self.claimed(&claim).await?;
            let mut next = input.value.clone();
            for (key, value) in terminal.as_object().unwrap() {
                next.insert(key.clone(), value.clone());
            }
            next.insert("result", result.pin.json());
            self.operations
                .store
                .write(
                    "chat-bridge/input",
                    &next,
                    &mutation(
                        &input.pin.object_id,
                        &format!(
                            "complete:{}:{}",
                            input.pin.revision,
                            hash_json(&Json::object([
                                ("terminal", terminal),
                                ("result", result.pin.json())
                            ]))?
                        ),
                    )?,
                    Destination::Update {
                        object_id: &input.pin.object_id,
                        expected_revision: input.pin.revision,
                    },
                )
                .await
                .map_err(|e| e.into_ivy())?;
            return Ok(());
        }
        let mut terminal_matches = same(field(&input.value, "result")?, &result.pin.json())?;
        for (key, value) in terminal.as_object().unwrap() {
            if !terminal_matches {
                break;
            }
            terminal_matches = same(
                input.value.get_key(key).ok_or_else(IvyError::internal)?,
                value,
            )?;
        }
        require(
            terminal_matches,
            "chat_identity_conflict",
            "All replies require the original input terminal state and complete Result.",
        )?;
        let published = number(&claim, "publishedParts")?;
        if published == number(&claim, "partCount")? {
            current = self.claimed(&claim).await?;
            self.finish_notice(&input, &claim).await?;
            let mut next = current.value.clone();
            next.insert("publication", Json::Null);
            next.insert(
                "queue",
                Json::Array(
                    field(&current.value, "queue")?
                        .as_array()
                        .ok_or_else(IvyError::internal)?[1..]
                        .to_vec(),
                ),
            );
            next.insert("updatedAt", field(&claim, "createdAt")?.clone());
            self.write_main(&current, &next).await?;
            self.cache = None;
            return Ok(());
        }
        let end = number(&claim, "partCount")?.min(published + 8.0);
        for index in published as usize..end as usize {
            self.source.verify_owner().await?;
            self.claimed(&claim).await?;
            let mut immutable = Json::object([
                ("schemaVersion", Json::Number(1.0)),
                ("workspaceId", field(&input.value, "workspaceId")?.clone()),
                (
                    "definitionHash",
                    field(&input.value, "definitionHash")?.clone(),
                ),
                (
                    "channel",
                    field(field(&input.value, "identity")?, "channel")?.clone(),
                ),
                (
                    "origin",
                    Json::object([
                        ("kind", "native".into()),
                        ("inputId", field(&claim, "inputId")?.clone()),
                        ("binding", field(&input.value, "binding")?.clone()),
                        ("result", result.pin.json()),
                    ]),
                ),
                (
                    "sequence",
                    Json::Number(number(&claim, "firstSequence")? + index as f64),
                ),
                ("partIndex", Json::Number(index as f64)),
            ]);
            for (key, value) in parts
                .get(index)
                .ok_or_else(IvyError::internal)?
                .as_object()
                .ok_or_else(IvyError::internal)?
            {
                immutable.insert(key.clone(), value.clone());
            }
            immutable.insert("createdAt", field(&claim, "createdAt")?.clone());
            let hash = hash_json(&immutable)?;
            immutable.insert("immutableHash", hash.into());
            immutable.insert("state", "pending".into());
            immutable.insert("firstOfferedAt", Json::Null);
            immutable.insert("confirmedAt", Json::Null);
            self.operations
                .store
                .write(
                    "chat-bridge/reply",
                    &immutable,
                    &mutation(
                        &input.pin.object_id,
                        &format!("reply:{}:{index}", result.pin.object_id),
                    )?,
                    Destination::Create {
                        parent_id: self.operations.store.root_object_id.as_deref(),
                        name: &format!("Chat reply {} part {index}", input.pin.object_id),
                    },
                )
                .await
                .map_err(|e| e.into_ivy())?;
        }
        current = self.claimed(&claim).await?;
        if field(&current.value, "publication")?.get("publishedParts")
            != claim.get("publishedParts")
        {
            return Ok(());
        }
        let mut next = current.value.clone();
        let mut progressed = claim.clone();
        progressed.insert("publishedParts", Json::Number(end));
        next.insert("publication", progressed);
        next.insert("updatedAt", field(&claim, "createdAt")?.clone());
        self.write_main(&current, &next).await
    }
}
