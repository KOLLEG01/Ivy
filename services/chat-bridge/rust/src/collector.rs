//! Durable, read-only collection of the original Main turn. Native dispatch and
//! live queue ownership stay with the runtime; retained evidence drives Results.
use crate::{
    collection_reads::{ChatCollectionReads, Phase},
    limits::{COLLECTION_BYTES, COLLECTION_READS, EVIDENCE_CHUNK_BYTES, MAXIMUM_EVIDENCE_BYTES},
    main_context::object_options,
    native_evidence::ChatNativeEvidence,
    operations::{ChatOperations, contention},
    publisher::{PublicationFuture, PublicationSource},
    result_text::read_chat_result_text,
    store::{ChatStore, Destination, Document, Pin, Reference, mutation},
};
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString,
    checked_native_contract::CheckedNativeContracts,
    hash::{digest, hash_json},
    json::canonical,
    limits::MANAGEMENT_FRAME_BYTES,
    operation::derive_operation_id_js,
};
use ivy_sdk::{clock::parse_date, operation_ids::scoped_operation_id, rpc::RequestOptions};
use std::{
    collections::HashSet,
    sync::{Arc, Mutex},
};

const NAME: &str = "Chat original turn collection";
pub struct CollectionHead {
    pub input: Document,
    pub binding: Document,
}
/// Main/delivery and native transport are mandatory runtime adapters. The
/// collector itself verifies their original receipt and every read's evidence.
pub trait CollectionRuntime: Send + Sync {
    fn head<'a>(
        &'a self,
        input_id: Option<&'a str>,
    ) -> PublicationFuture<'a, Option<CollectionHead>>;
    fn call<'a>(&'a self, input_id: &'a str, pin: &'a Pin) -> PublicationFuture<'a, Document>;
    fn ready(&self) -> PublicationFuture<'_, Json>;
    fn read(&self, request: Json) -> PublicationFuture<'_, Json>;
    fn verify_owner(&self) -> PublicationFuture<'_, ()>;
    fn recover_pending(&self) -> PublicationFuture<'_, ()>;
}
fn require(ok: bool, code: &str, message: &str) -> IvyResult<()> {
    if ok {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn field<'a>(v: &'a Json, key: &str) -> IvyResult<&'a Json> {
    v.get(key).ok_or_else(IvyError::internal)
}
fn string<'a>(v: &'a Json, key: &str) -> IvyResult<&'a JsString> {
    field(v, key)?.as_js_string().ok_or_else(IvyError::internal)
}
fn number(v: &Json, key: &str) -> IvyResult<f64> {
    field(v, key)?.as_f64().ok_or_else(IvyError::internal)
}
fn array<'a>(v: &'a Json, key: &str) -> IvyResult<&'a [Json]> {
    field(v, key)?.as_array().ok_or_else(IvyError::internal)
}
fn pin(v: &Json) -> IvyResult<Pin> {
    Ok(Pin {
        object_id: field(v, "objectId")?.as_str()?,
        revision: number(v, "revision")? as i64,
    })
}
fn same(a: &Json, b: &Json) -> IvyResult<bool> {
    Ok(canonical(a, MANAGEMENT_FRAME_BYTES)? == canonical(b, MANAGEMENT_FRAME_BYTES)?)
}
fn failure(error: &IvyError) -> Json {
    Json::object([
        ("code", Json::String(error.code_js())),
        ("outcome", "unknown".into()),
        (
            "detail",
            error.message.chars().take(2048).collect::<String>().into(),
        ),
    ])
}
fn joined(state: &ChatCollectionReads) -> IvyResult<String> {
    Ok(state
        .texts
        .iter()
        .map(JsString::to_utf8)
        .collect::<IvyResult<Vec<_>>>()?
        .join("\n\n"))
}
struct Source {
    selected: CollectionHead,
    call: Document,
}
struct Cached {
    pin: Pin,
    state: Arc<ChatCollectionReads>,
}
pub struct ChatCollector<'a> {
    pub operations: &'a ChatOperations<'a>,
    pub(crate) evidence: &'a ChatNativeEvidence<'a>,
    pub(crate) runtime: &'a dyn CollectionRuntime,
    catalogs: &'a CheckedNativeContracts,
    cache: Mutex<Option<Cached>>,
}
impl<'a> ChatCollector<'a> {
    pub fn new(
        operations: &'a ChatOperations<'a>,
        evidence: &'a ChatNativeEvidence<'a>,
        runtime: &'a dyn CollectionRuntime,
        catalogs: &'a CheckedNativeContracts,
    ) -> Self {
        Self {
            operations,
            evidence,
            runtime,
            catalogs,
            cache: Mutex::new(None),
        }
    }
    fn store(&self) -> &ChatStore {
        self.operations.store
    }
    /// Provider retries use the original request-local collector, independently
    /// of the worker's retained collection cache.
    pub(crate) fn fresh(&self) -> Self {
        Self::new(self.operations, self.evidence, self.runtime, self.catalogs)
    }
    fn clear(&self) {
        *self.cache.lock().unwrap() = None;
    }
    pub async fn find(&self, input_id: &str) -> IvyResult<Option<Document>> {
        self.store()
            .named_in("chat-bridge/collection", NAME, Some(input_id))
            .await
            .map_err(|e| e.into_ivy())
    }
    async fn source(&self, input_id: &str) -> IvyResult<Source> {
        let selected = self.runtime.head(Some(input_id)).await?;
        require(
            selected.as_ref().is_some_and(|s| {
                s.input
                    .value
                    .get("turnId")
                    .is_some_and(|value| !value.is_null())
                    && s.input
                        .value
                        .get("epoch")
                        .is_some_and(|value| !value.is_null())
                    && s.input
                        .value
                        .get("nativeCalls")
                        .and_then(|n| n.get("turn"))
                        .is_some_and(|value| !value.is_null())
            }),
            "chat_collection_unstarted",
            "Collection requires the original delivered queue head and native turn.",
        )?;
        let selected = selected.unwrap();
        let call = self
            .runtime
            .call(
                input_id,
                &pin(field(field(&selected.input.value, "nativeCalls")?, "turn")?)?,
            )
            .await?;
        require(
            call.value.get("operationId")
                == Some(&Json::String(derive_operation_id_js(
                    string(&selected.input.value, "operationId")?,
                    &"chat:native:turn/start".into(),
                )?))
                && call.value.get("predecessor") == Some(&Json::Null)
                && call.value.get("preparedEpoch") == Some(&Json::Null)
                && call.value.get("method") == Some(&"turn/start".into())
                && call.value.get("state") == Some(&"succeeded".into())
                && call
                    .value
                    .get("evidence")
                    .is_some_and(|value| !value.is_null())
                && call.value.get("epoch") == selected.input.value.get("epoch"),
            "chat_native_mismatch",
            "Collection requires its original succeeded native turn receipt.",
        )?;
        let operation = self
            .evidence
            .operation(
                input_id,
                &call.value,
                &pin(field(&call.value, "evidence")?)?,
            )
            .await?;
        let reply = operation.get("reply");
        require(
            reply.is_some_and(|value| !value.is_null())
                && reply.unwrap().get("result").is_some()
                && field(&operation, "params")?.get("threadId")
                    == field(&selected.binding.value, "primary")?.get("nativeId")
                && reply
                    .unwrap()
                    .get("result")
                    .and_then(|r| r.get("turn"))
                    .and_then(|t| t.get("id"))
                    == selected.input.value.get("turnId"),
            "chat_native_mismatch",
            "Only the turn from the original Main call may supply the result.",
        )?;
        Ok(Source { selected, call })
    }
    fn check(&self, collection: &Document, source: &Source) -> IvyResult<()> {
        let v = &collection.value;
        require(
            collection.metadata.get("name") == Some(&NAME.into())
                && same(
                    field(v, "input")?,
                    &Json::object([
                        (
                            "objectId",
                            source.selected.input.pin.object_id.clone().into(),
                        ),
                        ("revision", Json::Number(1.0)),
                    ]),
                )?
                && same(
                    field(v, "binding")?,
                    field(&source.selected.input.value, "binding")?,
                )?
                && same(field(v, "turnCall")?, &source.call.pin.json())?
                && v.get("turnId") == source.selected.input.value.get("turnId"),
            "chat_identity_conflict",
            "Collection must retain its original input, binding and succeeded turn call.",
        )?;
        let attempt = number(v, "attempt")?;
        require(
            (1.0..=32.0).contains(&attempt),
            "chat_identity_conflict",
            "Collection attempt is outside its bounded workflow state.",
        )?;
        if v.get("previousAttempt")
            .is_some_and(|value| !value.is_null())
        {
            let previous = field(v, "previousAttempt")?;
            require(
                previous.get("objectId") == Some(&collection.pin.object_id.clone().into())
                    && number(previous, "revision")? < collection.pin.revision as f64,
                "chat_identity_conflict",
                "A new read epoch must retain its earlier original collection revision.",
            )?;
            let retry = v.get("retryAuthorization").filter(|v| !(v).is_null());
            require(
                attempt > 1.0
                    && match retry {
                        None => true,
                        Some(retry) => {
                            let failed = field(retry, "failed")?;
                            failed.get("objectId") == Some(&collection.pin.object_id.clone().into())
                                && failed.get("revision") == previous.get("revision")
                        }
                    },
                "chat_identity_conflict",
                "A new read attempt must identify its immediately preceding workflow state.",
            )?;
        } else {
            require(
                attempt == 1.0,
                "chat_identity_conflict",
                "A later read attempt must identify its preceding workflow state.",
            )?;
        }
        Ok(())
    }
    fn expected(&self, value: &Json, state: &ChatCollectionReads) -> IvyResult<Json> {
        let definition = self.operations.admission.definition();
        let mut expected = Json::object([
            (
                "serviceNodeId",
                field(field(definition, "project")?, "serviceNodeId")?.clone(),
            ),
            (
                "callerPrincipalId",
                field(definition, "principalId")?.clone(),
            ),
            (
                "nativeVersion",
                field(field(definition, "nativePlan")?, "nativeVersion")?.clone(),
            ),
            ("epoch", field(value, "epoch")?.clone()),
        ]);
        for (key, value) in state.next()?.as_object().ok_or_else(IvyError::internal)? {
            expected.insert(key.clone(), value.clone());
        }
        Ok(expected)
    }
    async fn replay(
        &self,
        collection: &Document,
        thread_id: &JsString,
    ) -> IvyResult<Arc<ChatCollectionReads>> {
        if let Some(cached) = &*self.cache.lock().unwrap() {
            if cached.pin == collection.pin {
                return Ok(cached.state.clone());
            }
        }
        let mut state = ChatCollectionReads::new(
            thread_id.clone(),
            string(&collection.value, "turnId")?.clone(),
        );
        let mut pins = HashSet::new();
        for reference in array(&collection.value, "reads")? {
            require(
                reference.get("revision") == Some(&Json::Number(1.0))
                    && pins.insert(field(reference, "objectId")?.as_str()?),
                "chat_collection_gap",
                "A collection must pin distinct immutable original evidence.",
            )?;
            let original = self
                .evidence
                .read(
                    &field(field(&collection.value, "input")?, "objectId")?.as_str()?,
                    &self.expected(&collection.value, &state)?,
                    &pin(reference)?,
                )
                .await?;
            require(
                state.append(&original, self.catalogs)?,
                "chat_collection_gap",
                "A saved collection must not use an in-progress poll as result evidence.",
            )?;
        }
        require(
            state.native_bytes as f64 == number(&collection.value, "nativeBytes")?
                && if collection.value.get("state") == Some(&"collecting".into()) {
                    state.phase != Phase::Done
                } else {
                    state.phase == Phase::Done
                },
            "chat_collection_gap",
            "The saved collection state and byte total must agree with its full original evidence.",
        )?;
        let state = Arc::new(state);
        *self.cache.lock().unwrap() = Some(Cached {
            pin: collection.pin.clone(),
            state: state.clone(),
        });
        Ok(state)
    }
    async fn update(&self, current: &Document, next: &Json) -> IvyResult<Document> {
        let reference = self
            .store()
            .write(
                "chat-bridge/collection",
                next,
                &mutation(
                    &current.pin.object_id,
                    &format!("collection:{}:{}", current.pin.revision, hash_json(next)?),
                )?,
                Destination::Update {
                    object_id: &current.pin.object_id,
                    expected_revision: current.pin.revision,
                },
            )
            .await
            .map_err(|e| e.into_ivy())?;
        self.store()
            .read_in(
                "chat-bridge/collection",
                Reference::Pinned(&reference),
                Some(&field(field(&current.value, "input")?, "objectId")?.as_str()?),
            )
            .await
            .map_err(|e| e.into_ivy())
    }
    async fn expose_failure(&self, current: Document) -> IvyResult<Document> {
        if current.value.get("state") != Some(&"failed".into()) {
            return Ok(current);
        }
        let head = self
            .runtime
            .head(Some(
                &field(field(&current.value, "input")?, "objectId")?.as_str()?,
            ))
            .await?;
        let Some(head) = head else {
            return Ok(current);
        };
        let input = head.input;
        if input
            .value
            .get("finishedAt")
            .is_some_and(|value| !value.is_null())
            || input
                .value
                .get("result")
                .is_some_and(|value| !value.is_null())
        {
            return Ok(current);
        }
        let state = if input
            .value
            .get("cancellation")
            .is_some_and(|value| !value.is_null())
        {
            "cancel_requested"
        } else {
            "collecting"
        };
        require(
            input.value.get("turnId") == current.value.get("turnId")
                && same(
                    field(&input.value, "binding")?,
                    field(&current.value, "binding")?,
                )?,
            "chat_collection_mismatch",
            "Collection failure belongs to its original input and turn.",
        )?;
        if input.value.get("state") == Some(&state.into())
            && same(
                field(&input.value, "error")?,
                field(&current.value, "error")?,
            )?
        {
            return Ok(current);
        }
        let mut next = input.value.clone();
        next.insert("state", state.into());
        next.insert("error", field(&current.value, "error")?.clone());
        next.insert("updatedAt", field(&current.value, "updatedAt")?.clone());
        self.store()
            .write(
                "chat-bridge/input",
                &next,
                &mutation(
                    &current.pin.object_id,
                    &format!(
                        "expose-failure:{}:{}",
                        input.pin.revision,
                        hash_json(&next)?
                    ),
                )?,
                Destination::Update {
                    object_id: &input.pin.object_id,
                    expected_revision: input.pin.revision,
                },
            )
            .await
            .map_err(|e| e.into_ivy())?;
        Ok(current)
    }
    async fn fail(&self, current: &Document, error: &IvyError) -> IvyResult<Document> {
        let mut next = current.value.clone();
        next.insert("state", "failed".into());
        next.insert("result", Json::Null);
        next.insert("error", failure(error));
        next.insert("updatedAt", self.store().now_iso()?.into());
        self.expose_failure(self.update(current, &next).await?)
            .await
    }
    async fn publish(&self, current: Document, state: &ChatCollectionReads) -> IvyResult<Document> {
        require(
            state.phase == Phase::Done
                && !(&state.native_state).is_null()
                && state
                    .last_observed_at
                    .as_ref()
                    .is_some_and(|value| !value.is_null()),
            "chat_collection_incomplete",
            "A Result needs all original native evidence and final verification.",
        )?;
        let parent = field(field(&current.value, "input")?, "objectId")?.as_str()?;
        let metadata = Json::object([
            ("schemaVersion", Json::Number(1.0)),
            ("inputId", parent.clone().into()),
            ("binding", field(&current.value, "binding")?.clone()),
            ("turnId", field(&current.value, "turnId")?.clone()),
            ("nativeState", state.native_state.clone()),
            ("evidence", field(&current.value, "reads")?.clone()),
            ("createdAt", state.last_observed_at.clone().unwrap()),
        ]);
        if current.value.get("state") == Some(&"complete".into()) {
            let result = self
                .store()
                .read_in(
                    "chat-bridge/result",
                    Reference::Pinned(&pin(field(&current.value, "result")?)?),
                    Some(&parent),
                )
                .await
                .map_err(|e| e.into_ivy())?;
            let mut retained = object_options(&result.value);
            retained.remove("textParts");
            require(
                result.pin.revision == 1
                    && same(&retained, &metadata)?
                    && read_chat_result_text(self.store(), &parent.clone().into(), &result.value)
                        .await?
                        == JsString::from(joined(state)?),
                "chat_evidence_mismatch",
                "The completed collection must retain its exact original Result and complete text.",
            )?;
            return Ok(current);
        }
        let bytes = joined(state)?.into_bytes();
        require(
            bytes.len() == state.text_bytes && bytes.len() <= COLLECTION_BYTES,
            "chat_collection_limit",
            "The complete UTF-8 text must fit its declared bound.",
        )?;
        let mut parts = vec![];
        let mut offset = 0;
        while offset < bytes.len() {
            require(
                parts.len() < 64,
                "chat_collection_limit",
                "Complete UTF-8 text exceeds 64 bounded immutable parts.",
            )?;
            let mut end = bytes.len().min(offset + EVIDENCE_CHUNK_BYTES);
            while end < bytes.len() && bytes[end] & 0xc0 == 0x80 {
                end -= 1;
            }
            let part = &bytes[offset..end];
            let index = parts.len();
            let hash = digest(part);
            let id = scoped_operation_id(
                self.store().client.clone(),
                &Json::Array(vec![
                    "chat".into(),
                    mutation(&parent, &format!("result-text:{index}:{hash}"))?.into(),
                ]),
            )
            .await?;
            let saved = self
                .store()
                .client
                .request(
                    "objects.write",
                    Json::object([
                        ("mutationId", Json::String(id)),
                        ("contractVersion", "1.0.0".into()),
                        ("references", Json::Object(vec![])),
                        (
                            "create",
                            Json::object([
                                ("parentId", parent.clone().into()),
                                ("ownerObjectId", parent.clone().into()),
                                ("contractKey", "chat-bridge/result-text".into()),
                                (
                                    "name",
                                    format!("Chat result text {index} {}", &hash[7..]).into(),
                                ),
                            ]),
                        ),
                        (
                            "content",
                            Json::object([
                                ("encoding", "text".into()),
                                (
                                    "value",
                                    String::from_utf8(part.to_vec())
                                        .map_err(|_| IvyError::internal())?
                                        .into(),
                                ),
                            ]),
                        ),
                    ]),
                    RequestOptions::default(),
                )
                .await?;
            let saved_pin = Pin {
                object_id: field(field(&saved, "object")?, "id")?.as_str()?,
                revision: number(field(&saved, "revision")?, "revision")? as i64,
            };
            parts.push(Json::object([
                ("object", saved_pin.json()),
                ("contentHash", hash.into()),
                ("byteLength", Json::Number(part.len() as f64)),
                ("mediaType", "text/plain".into()),
                ("label", format!("Reply text part {}", index + 1).into()),
            ]));
            offset = end;
        }
        let mut value = metadata;
        value.insert("textParts", Json::Array(parts));
        let attempt = number(&current.value, "attempt")? as usize;
        let result = self
            .store()
            .write(
                "chat-bridge/result",
                &value,
                &mutation(&current.pin.object_id, &format!("result:{attempt}"))?,
                Destination::Create {
                    parent_id: Some(&parent),
                    name: &format!(
                        "Chat complete result {} attempt {attempt}",
                        current.pin.object_id
                    ),
                },
            )
            .await
            .map_err(|e| e.into_ivy())?;
        let mut next = current.value.clone();
        next.insert("state", "complete".into());
        next.insert("result", result.json());
        next.insert("error", Json::Null);
        next.insert("updatedAt", state.last_observed_at.clone().unwrap());
        self.update(&current, &next).await
    }
    async fn restart(
        &self,
        current: &Document,
        epoch: &Json,
        authorization: Option<&Json>,
    ) -> IvyResult<Document> {
        require(
            current.value.get("state") == Some(&"failed".into())
                && number(&current.value, "attempt")? < 32.0,
            "chat_collection_retry_refused",
            "Only a bounded failed original read may be retried.",
        )?;
        self.clear();
        let mut next = current.value.clone();
        next.insert("epoch", epoch.clone());
        next.insert(
            "attempt",
            Json::Number(number(&current.value, "attempt")? + 1.0),
        );
        next.insert("previousAttempt", current.pin.json());
        next.insert("state", "collecting".into());
        next.insert("result", Json::Null);
        next.insert("error", Json::Null);
        next.insert("reads", Json::Array(vec![]));
        next.insert("nativeBytes", Json::Number(0.0));
        next.insert(
            "retryAuthorization",
            authorization.cloned().unwrap_or(Json::Null),
        );
        next.insert("updatedAt", self.store().now_iso()?.into());
        self.update(current, &next).await
    }
    /// At most one native read; every persisted checkpoint can be reopened.
    pub async fn step(&self, input_id: &str) -> IvyResult<Option<Document>> {
        let source = self.source(input_id).await?;
        let thread = string(
            field(&source.selected.binding.value, "primary")?,
            "nativeId",
        )?;
        let mut current = self.find(input_id).await?;
        let outcome: IvyResult<Option<Document>> = async {
            if let Some(selected) = current.as_ref() {
                self.check(selected, &source)?;
                if selected.value.get("state") == Some(&"failed".into()) {
                    let code = selected
                        .value
                        .get("error")
                        .and_then(|e| e.get("code"))
                        .and_then(Json::as_js_string);
                    let gap = code == Some(&"chat_collection_gap".into());
                    let attempt = number(&selected.value, "attempt")?;
                    let delay = if gap {
                        5000.0
                    } else {
                        1000.0 * 2.0_f64.powf(attempt)
                    };
                    if !(gap || code == Some(&"chat_collection_native_error".into()))
                        || attempt >= if gap { 32.0 } else { 3.0 }
                        || (self.store().now_millis() as f64)
                            < parse_date(string(&selected.value, "updatedAt")?) + delay
                    {
                        let selected = current.take().unwrap();
                        return Ok(Some(self.expose_failure(selected).await?));
                    }
                    let status = self.runtime.ready().await?;
                    current = Some(
                        self.restart(selected, field(&status, "epoch")?, None)
                            .await?,
                    );
                }
                let selected = current.as_ref().unwrap();
                if ["ready", "complete"]
                    .iter()
                    .any(|s| selected.value.get("state") == Some(&Json::from(*s)))
                {
                    let state = self.replay(selected, thread).await?;
                    // Keep the checkpoint available to the error handler if publication fails.
                    let result = self
                        .publish(
                            self.store()
                                .read_in(
                                    "chat-bridge/collection",
                                    Reference::Pinned(&selected.pin),
                                    Some(input_id),
                                )
                                .await
                                .map_err(|e| e.into_ivy())?,
                            &state,
                        )
                        .await?;
                    return Ok(Some(result));
                }
            }
            let status = self.runtime.ready().await?;
            if current.is_none() {
                let at = self.store().now_iso()?;
                let value = Json::object([
                    ("schemaVersion", Json::Number(1.0)),
                    (
                        "input",
                        Json::object([
                            ("objectId", input_id.into()),
                            ("revision", Json::Number(1.0)),
                        ]),
                    ),
                    (
                        "binding",
                        field(&source.selected.input.value, "binding")?.clone(),
                    ),
                    ("turnCall", source.call.pin.json()),
                    (
                        "turnId",
                        field(&source.selected.input.value, "turnId")?.clone(),
                    ),
                    ("epoch", field(&status, "epoch")?.clone()),
                    ("attempt", Json::Number(1.0)),
                    ("previousAttempt", Json::Null),
                    ("reads", Json::Array(vec![])),
                    ("nativeBytes", Json::Number(0.0)),
                    ("state", "collecting".into()),
                    ("result", Json::Null),
                    ("error", Json::Null),
                    ("createdAt", at.clone().into()),
                    ("updatedAt", at.into()),
                ]);
                let reference = self
                    .store()
                    .write(
                        "chat-bridge/collection",
                        &value,
                        &mutation(input_id, "collection")?,
                        Destination::Create {
                            parent_id: Some(input_id),
                            name: NAME,
                        },
                    )
                    .await
                    .map_err(|e| e.into_ivy())?;
                current = Some(
                    self.store()
                        .read_in(
                            "chat-bridge/collection",
                            Reference::Pinned(&reference),
                            Some(input_id),
                        )
                        .await
                        .map_err(|e| e.into_ivy())?,
                );
            } else if status.get("epoch") != current.as_ref().unwrap().value.get("epoch") {
                let selected = current.as_ref().unwrap();
                require(
                    number(&selected.value, "attempt")? < 32.0,
                    "chat_collection_limit",
                    "Explicit operator investigation is required after 32 read attempts.",
                )?;
                let mut next = selected.value.clone();
                next.insert("epoch", field(&status, "epoch")?.clone());
                next.insert(
                    "attempt",
                    Json::Number(number(&selected.value, "attempt")? + 1.0),
                );
                next.insert("previousAttempt", selected.pin.json());
                next.insert("reads", Json::Array(vec![]));
                next.insert("nativeBytes", Json::Number(0.0));
                next.insert("updatedAt", field(&status, "observedAt")?.clone());
                current = Some(self.update(selected, &next).await?);
                self.clear();
            }
            let selected = current.as_ref().unwrap();
            let mut state = self.replay(selected, thread).await?.fork();
            require(
                state.count < COLLECTION_READS,
                "chat_collection_limit",
                "Native collection exhausted its complete read bound.",
            )?;
            let expected = self.expected(&selected.value, &state)?;
            let observation = self
                .runtime
                .read(Json::object([
                    ("nativeVersion", field(&expected, "nativeVersion")?.clone()),
                    ("method", field(&expected, "method")?.clone()),
                    ("params", field(&expected, "params")?.clone()),
                ]))
                .await?;
            self.evidence.verify_read(&observation, &expected)?;
            let bytes = canonical(&observation, MAXIMUM_EVIDENCE_BYTES)?.len();
            require(
                number(&selected.value, "nativeBytes")? + bytes as f64 <= COLLECTION_BYTES as f64,
                "chat_collection_limit",
                "Native collection exhausted its complete byte bound.",
            )?;
            self.clear();
            let error = match state.append(&observation, self.catalogs) {
                Ok(false) => return Ok(current.take()),
                Ok(true) => None,
                Err(error) => Some(error),
            };
            let evidence = self
                .evidence
                .save_read(input_id, &expected, &observation)
                .await?;
            let mut reads = array(&selected.value, "reads")?.to_vec();
            reads.push(evidence.json());
            let mut next = selected.value.clone();
            next.insert("reads", Json::Array(reads));
            next.insert(
                "nativeBytes",
                Json::Number(number(&selected.value, "nativeBytes")? + bytes as f64),
            );
            next.insert(
                "state",
                if error.is_some() {
                    "failed"
                } else if state.phase == Phase::Done {
                    "ready"
                } else {
                    "collecting"
                }
                .into(),
            );
            next.insert("error", error.as_ref().map(failure).unwrap_or(Json::Null));
            next.insert("updatedAt", field(&observation, "observedAt")?.clone());
            current = Some(self.update(selected, &next).await?);
            if error.is_none() {
                *self.cache.lock().unwrap() = Some(Cached {
                    pin: current.as_ref().unwrap().pin.clone(),
                    state: Arc::new(state),
                });
            }
            // On exposure failure, the new durable collection remains the checkpoint.
            let selected = current.as_ref().unwrap();
            self.expose_failure(
                self.store()
                    .read_in(
                        "chat-bridge/collection",
                        Reference::Pinned(&selected.pin),
                        Some(input_id),
                    )
                    .await
                    .map_err(|e| e.into_ivy())?,
            )
            .await
            .map(Some)
        }
        .await;
        match outcome {
            Ok(value) => Ok(value),
            Err(error) => {
                self.clear();
                if contention(&error) {
                    return self.find(input_id).await;
                }
                if let Some(current) = &current {
                    if error.code.starts_with("chat_collection_")
                        || error.code == "chat_result_text_invalid"
                    {
                        return self.fail(current, &error).await.map(Some);
                    }
                }
                Err(error)
            }
        }
    }
    pub async fn observed_terminal(&self, input_id: &str) -> IvyResult<bool> {
        let source = self.source(input_id).await?;
        let Some(current) = self.find(input_id).await? else {
            return Ok(false);
        };
        self.check(&current, &source)?;
        let mut attempt = Some(current);
        for _ in 0..32 {
            let Some(selected) = attempt else {
                break;
            };
            let mut state = ChatCollectionReads::new(
                string(
                    field(&source.selected.binding.value, "primary")?,
                    "nativeId",
                )?
                .clone(),
                string(&selected.value, "turnId")?.clone(),
            );
            for reference in array(&selected.value, "reads")? {
                let original = self
                    .evidence
                    .read(
                        input_id,
                        &self.expected(&selected.value, &state)?,
                        &pin(reference)?,
                    )
                    .await?;
                if state.append(&original, self.catalogs).is_err() {
                    break;
                }
                if state.native_state != Json::Null {
                    return Ok(true);
                }
            }
            attempt = if selected
                .value
                .get("previousAttempt")
                .is_some_and(|value| !value.is_null())
            {
                Some(
                    self.store()
                        .read_in(
                            "chat-bridge/collection",
                            Reference::Pinned(&pin(field(&selected.value, "previousAttempt")?)?),
                            Some(input_id),
                        )
                        .await
                        .map_err(|e| e.into_ivy())?,
                )
            } else {
                None
            };
        }
        Ok(false)
    }
    pub async fn retry(&self, caller: &JsString, request: &Json) -> IvyResult<Json> {
        let expected = field(request, "expectedBridge")?;
        self.operations
            .admission
            .authorize(caller, expected, None)?;
        let input_id = field(request, "inputId")?.as_str()?;
        let input = self
            .store()
            .read("chat-bridge/input", Reference::Current(&input_id))
            .await
            .map_err(|e| e.into_ivy())?;
        self.operations.admission.authorize(
            caller,
            expected,
            Some(field(field(&input.value, "identity")?, "channel")?),
        )?;
        self.runtime.verify_owner().await?;
        let current = self.find(&input_id).await?;
        require(
            current.is_some(),
            "not_found",
            "The original collection is unavailable.",
        )?;
        let current = current.unwrap();
        let authorization = Json::object([
            ("callerPrincipalId", Json::String(caller.clone())),
            ("operationId", field(request, "operationId")?.clone()),
            ("reason", field(request, "reason")?.clone()),
            ("failed", field(request, "expectedCollection")?.clone()),
        ]);
        if same(
            current
                .value
                .get("retryAuthorization")
                .unwrap_or(&Json::Null),
            &authorization,
        )? {
            return Ok(current.value);
        }
        let source = self.source(&input_id).await?;
        self.check(&current, &source)?;
        require(
            same(&current.pin.json(), field(request, "expectedCollection")?)?,
            "revision_conflict",
            "Retry requires the exact displayed failed collection.",
        )?;
        let status = self.runtime.ready().await?;
        Ok(self
            .restart(&current, field(&status, "epoch")?, Some(&authorization))
            .await?
            .value)
    }
}
impl PublicationSource for ChatCollector<'_> {
    fn recover_pending(&self) -> PublicationFuture<'_, ()> {
        self.runtime.recover_pending()
    }
    fn verify_owner(&self) -> PublicationFuture<'_, ()> {
        self.runtime.verify_owner()
    }
    fn head(&self) -> PublicationFuture<'_, Option<Document>> {
        Box::pin(async move { Ok(self.runtime.head(None).await?.map(|s| s.input)) })
    }
    fn find<'a>(&'a self, input_id: &'a str) -> PublicationFuture<'a, Option<Document>> {
        Box::pin(self.find(input_id))
    }
    fn collect<'a>(&'a self, input_id: &'a str) -> PublicationFuture<'a, Option<Document>> {
        Box::pin(self.step(input_id))
    }
}
