//! Immutable reply offers and authenticated, monotone delivery confirmation.
use crate::{
    admission::{expected_bridge, same_channel},
    main_context::truthy,
    operations::{ChatOperations, contention},
    queue::{ChatQueue, main_name},
    store::{Destination, Document, Pin, Reference, mutation},
};
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString, Outcome,
    chat_schema::validate_chat,
    hash::hash_json,
    json::{canonical, object_entries},
    limits::MANAGEMENT_FRAME_BYTES,
};
use ivy_sdk::rpc::RequestOptions;
use std::{
    collections::HashSet,
    future::{Future, poll_fn},
    task::Poll,
};

const MAXIMUM_PAGE_BYTES: usize = 1024 * 1024;
const MAXIMUM_SEQUENCE: f64 = 9_007_199_254_740_991.0;
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
fn array<'a>(value: &'a Json, key: &str) -> IvyResult<&'a [Json]> {
    field(value, key)?.as_array().ok_or_else(IvyError::internal)
}
fn number(value: &Json, key: &str) -> IvyResult<f64> {
    field(value, key)?.as_f64().ok_or_else(IvyError::internal)
}
fn js<'a>(value: &'a Json, key: &str) -> IvyResult<&'a JsString> {
    field(value, key)?
        .as_js_string()
        .ok_or_else(IvyError::internal)
}
fn text(value: &Json, key: &str) -> IvyResult<String> {
    field(value, key)?.as_str()
}
fn pin(value: &Json) -> IvyResult<Pin> {
    Ok(Pin {
        object_id: text(value, "objectId")?,
        revision: number(value, "revision")? as i64,
    })
}
fn same(a: &Json, b: &Json) -> IvyResult<bool> {
    Ok(canonical(a, MANAGEMENT_FRAME_BYTES)? == canonical(b, MANAGEMENT_FRAME_BYTES)?)
}
fn bytes(value: &Json) -> IvyResult<usize> {
    Ok(canonical(value, MANAGEMENT_FRAME_BYTES)?.len())
}
fn immutable_reply(value: &Json) -> Json {
    let mut value = value.clone();
    for key in ["state", "firstOfferedAt", "confirmedAt", "immutableHash"] {
        value.remove(key);
    }
    value
}
fn view(document: &Document) -> Json {
    Json::object([
        ("object", document.pin.json()),
        ("data", document.value.clone()),
    ])
}
fn predicate(op: &str, field: &str, value: Json) -> Json {
    Json::object([("op", op.into()), ("field", field.into()), ("value", value)])
}
fn and(args: Vec<Json>) -> Json {
    Json::object([("op", "and".into()), ("args", Json::Array(args))])
}
fn order(field: &str) -> Json {
    Json::Array(vec![Json::object([
        ("field", field.into()),
        ("direction", "asc".into()),
    ])])
}
fn terminal(operation: &Document) -> bool {
    ["succeeded", "failed"]
        .iter()
        .any(|phase| operation.value.get("phase") == Some(&Json::from(*phase)))
}
struct Page {
    entries: Vec<Document>,
    has_more: bool,
}
struct Offer {
    document: Document,
    replies: Vec<Document>,
}

pub struct PendingNotices {
    pub replies: Vec<Document>,
    pub next_cursor: Option<Json>,
}

pub struct ChatOutbox<'a> {
    pub operations: &'a ChatOperations<'a>,
}
impl<'a> ChatOutbox<'a> {
    pub fn new(operations: &'a ChatOperations<'a>) -> Self {
        Self { operations }
    }
    fn root(&self) -> Json {
        self.operations
            .store
            .root_object_id
            .as_ref()
            .map(|v| predicate("eq", "object.parentId", v.clone().into()))
            .unwrap_or_else(|| {
                Json::object([("op", "isNull".into()), ("field", "object.parentId".into())])
            })
    }
    async fn read(&self, key: &str, reference: Reference<'_>) -> IvyResult<Document> {
        self.operations
            .store
            .read(key, reference)
            .await
            .map_err(|e| e.into_ivy())
    }
    async fn named(&self, key: &str, name: &str) -> IvyResult<Option<Document>> {
        self.operations
            .store
            .named(key, name)
            .await
            .map_err(|e| e.into_ivy())
    }
    async fn write(
        &self,
        key: &str,
        value: &Json,
        scope: &str,
        step: &str,
        destination: Destination<'_>,
    ) -> IvyResult<Pin> {
        self.operations
            .store
            .write(key, value, &mutation(scope, step)?, destination)
            .await
            .map_err(|e| e.into_ivy())
    }
    fn scope(&self, value: &Json, channel: &Json, definition_hash: Option<&Json>) -> IvyResult<()> {
        let admission = self.operations.admission;
        let expected = Json::from(admission.definition_hash());
        let same_scope = value.get("workspaceId") == admission.definition().get("workspaceId")
            && value.get("definitionHash") == Some(definition_hash.unwrap_or(&expected));
        let matches = if same_scope {
            let actual = if let Some(identity) = value.get("identity") {
                field(identity, "channel")?
            } else {
                field(value, "channel")?
            };
            same_channel(actual, channel)
        } else {
            false
        };
        require(
            matches,
            "chat_scope_mismatch",
            "Chat history and replies must retain their exact original workspace, definition and channel.",
        )
    }
    async fn entries(
        &self,
        key: &str,
        channel: &Json,
        after: f64,
        limit: f64,
        through: f64,
    ) -> IvyResult<Page> {
        let admission = self.operations.admission;
        require(
            array(admission.definition(), "channels")?
                .iter()
                .any(|v| v.get("channel").is_some_and(|c| same_channel(c, channel))),
            "chat_scope_mismatch",
            "Chat history requires its configured channel.",
        )?;
        let path = if key == "chat-bridge/input" {
            "identity/channel"
        } else {
            "channel"
        };
        let mut filters = vec![
            self.root(),
            predicate(
                "eq",
                "data:/workspaceId",
                field(admission.definition(), "workspaceId")?.clone(),
            ),
            predicate(
                "eq",
                "data:/definitionHash",
                admission.definition_hash().into(),
            ),
        ];
        for (name, value) in object_entries(channel) {
            filters.push(predicate(
                "eq",
                &format!("data:/{path}/{}", name.to_utf8()?),
                value.clone(),
            ));
        }
        filters.push(predicate("gt", "data:/sequence", Json::Number(after)));
        filters.push(predicate("lte", "data:/sequence", Json::Number(through)));
        let page = self
            .operations
            .store
            .client
            .request(
                "objects.query",
                Json::object([
                    ("contractKey", key.into()),
                    ("where", and(filters)),
                    ("orderBy", order("data:/sequence")),
                    ("limit", Json::Number(limit + 1.0)),
                ]),
                RequestOptions::default(),
            )
            .await?;
        let mut entries = Vec::new();
        for item in array(&page, "items")? {
            let selected = pin(item)?;
            let document = if key == "chat-bridge/reply" {
                self.reply(Reference::Pinned(&selected), channel).await?
            } else {
                self.read(key, Reference::Pinned(&selected)).await?
            };
            self.scope(&document.value, channel, None)?;
            entries.push(document);
        }
        let mut previous = after;
        for document in &entries {
            let next = number(&document.value, "sequence")?;
            require(
                next > previous,
                "chat_identity_conflict",
                "Channel history cannot contain duplicate or nonadvancing sequences.",
            )?;
            previous = next;
        }
        // The original uses !== null, not truthiness, for this cursor.
        Ok(Page {
            entries,
            has_more: page.get("nextCursor") != Some(&Json::Null),
        })
    }
    async fn boundary(&self) -> IvyResult<Option<Document>> {
        if self
            .named(
                "chat-bridge/main",
                &main_name(self.operations.admission.definition())?,
            )
            .await?
            .is_none()
        {
            return Ok(None);
        }
        Ok(Some(ChatQueue::new(self.operations, None).main().await?))
    }
    async fn visible(
        &self,
        keys: &[&str],
        channel: &Json,
        after: f64,
        limit: f64,
    ) -> IvyResult<Vec<Page>> {
        for _ in 0..8 {
            let before = self.boundary().await?;
            let pending = if let Some(action) = before
                .as_ref()
                .and_then(|d| d.value.get("pendingAction"))
                .filter(|v| truthy(v))
            {
                let document = self
                    .read("chat-bridge/operation", Reference::Pinned(&pin(action)?))
                    .await?;
                self.operations.check(&document.value)?;
                Some(document)
            } else {
                None
            };
            let mut through = MAXIMUM_SEQUENCE;
            if let Some(publication) = before
                .as_ref()
                .and_then(|d| d.value.get("publication"))
                .filter(|v| truthy(v))
            {
                through = through.min(number(publication, "firstSequence")? - 1.0);
            }
            if pending
                .as_ref()
                .and_then(|d| d.value.get("request"))
                .and_then(|r| r.get("action"))
                == Some(&Json::from("notify"))
            {
                through = through.min(
                    number(
                        &before.as_ref().ok_or_else(IvyError::internal)?.value,
                        "nextSequence",
                    )? - 1.0,
                );
            }
            // Begin every read lane before observing a rejection, as Promise.all
            // does in the original. No lane writes delivery or history state.
            let mut reads: Vec<_> = keys
                .iter()
                .map(|key| Box::pin(self.entries(key, channel, after, limit, through)))
                .collect();
            let mut completed: Vec<Option<Page>> = (0..keys.len()).map(|_| None).collect();
            let pages = poll_fn(|cx| {
                let mut failure = None;
                for (index, read) in reads.iter_mut().enumerate() {
                    if completed[index].is_some() {
                        continue;
                    }
                    match read.as_mut().poll(cx) {
                        Poll::Ready(Ok(page)) => completed[index] = Some(page),
                        Poll::Ready(Err(error)) => {
                            if failure.is_none() {
                                failure = Some(error);
                            }
                        }
                        Poll::Pending => {}
                    }
                }
                if let Some(error) = failure {
                    Poll::Ready(Err(error))
                } else if completed.iter().all(Option::is_some) {
                    Poll::Ready(Ok(completed
                        .iter_mut()
                        .map(|v| v.take().unwrap())
                        .collect::<Vec<_>>()))
                } else {
                    Poll::Pending
                }
            })
            .await?;
            let after = self.boundary().await?;
            if before.as_ref().map(|d| &d.pin) == after.as_ref().map(|d| &d.pin) {
                return Ok(pages);
            }
        }
        Err(IvyError::new(
            "chat_contention",
            "History publication changed repeatedly. Retry this same bounded query.",
        )
        .with_outcome(Outcome::Unknown))
    }
    pub async fn reply(&self, reference: Reference<'_>, channel: &Json) -> IvyResult<Document> {
        let selected = self.read("chat-bridge/reply", reference).await?;
        let value = &selected.value;
        self.scope(value, channel, value.get("definitionHash"))?;
        require(
            value.get("immutableHash") == Some(&hash_json(&immutable_reply(value))?.into()),
            "chat_identity_conflict",
            "A saved reply cannot change its original content, sequence, input, binding or result.",
        )?;
        let origin = field(value, "origin")?;
        if origin.get("kind") == Some(&Json::from("notice")) {
            require(
                !js(origin, "producerPrincipalId")?.0.is_empty()
                    && number(field(origin, "source")?, "revision")? > 0.0
                    && same_channel(channel, field(value, "channel")?)
                    && selected.metadata.get("name")
                        == Some(
                            &format!(
                                "Chat notice {}",
                                text(field(origin, "operation")?, "objectId")?
                            )
                            .into(),
                        )
                    && value.get("partIndex") == Some(&Json::Number(0.0))
                    && array(value, "artifacts")?.is_empty(),
                "chat_identity_conflict",
                "A final notice must retain its admitted producer, source, channel and immutable content without its expired delivery receipt.",
            )?;
            return Ok(selected);
        }
        let input = self
            .read(
                "chat-bridge/input",
                Reference::Current(&text(origin, "inputId")?),
            )
            .await?;
        self.scope(&input.value, channel, value.get("definitionHash"))?;
        let result = self
            .operations
            .store
            .read_in(
                "chat-bridge/result",
                Reference::Pinned(&pin(field(origin, "result")?)?),
                Some(&input.pin.object_id),
            )
            .await
            .map_err(|e| e.into_ivy())?;
        require(
            ["completed", "failed", "cancelled"]
                .iter()
                .any(|v| input.value.get("state") == Some(&Json::from(*v)))
                && same(field(&input.value, "result")?, field(origin, "result")?)?
                && same(field(origin, "binding")?, field(&input.value, "binding")?)?
                && result.value.get("inputId") == Some(&input.pin.object_id.clone().into())
                && same(field(&result.value, "binding")?, field(origin, "binding")?)?
                && result.value.get("turnId") == input.value.get("turnId")
                && number(value, "sequence")? > number(&input.value, "sequence")?,
            "chat_identity_conflict",
            "Reply delivery needs its original terminal input and immutable saved result.",
        )?;
        Ok(selected)
    }
    pub async fn pending_notices(&self, cursor: Option<&JsString>) -> IvyResult<PendingNotices> {
        let mut params = Json::object([
            ("contractKey", "chat-bridge/reply".into()),
            (
                "where",
                and(vec![
                    self.root(),
                    predicate(
                        "eq",
                        "data:/workspaceId",
                        field(self.operations.admission.definition(), "workspaceId")?.clone(),
                    ),
                    predicate("eq", "data:/partIndex", Json::Number(0.0)),
                    predicate("ne", "data:/state", "confirmed".into()),
                ]),
            ),
            ("orderBy", order("object.createdAt")),
            ("limit", Json::Number(32.0)),
        ]);
        if let Some(cursor) = cursor.filter(|v| !v.0.is_empty()) {
            params.insert("cursor", Json::String(cursor.clone()));
        }
        let page = self
            .operations
            .store
            .client
            .request("objects.query", params, RequestOptions::default())
            .await?;
        let mut replies = Vec::new();
        for item in array(&page, "items")? {
            let saved = self
                .read("chat-bridge/reply", Reference::Pinned(&pin(item)?))
                .await?;
            let origin = field(&saved.value, "origin")?;
            if origin.get("kind") != Some(&Json::from("native")) {
                continue;
            }
            let input = self
                .read(
                    "chat-bridge/input",
                    Reference::Current(&text(origin, "inputId")?),
                )
                .await?;
            let operation = self
                .operations
                .find(
                    js(field(&input.value, "identity")?, "senderPrincipalId")?,
                    js(&input.value, "operationId")?,
                )
                .await?;
            let Some(operation) = operation.filter(|d| {
                d.value.get("request").and_then(|v| v.get("action")) == Some(&Json::from("notify"))
            }) else {
                continue;
            };
            let outcome = operation.value.get("outcome").filter(|v| truthy(v));
            require(
                operation.value.get("phase") == Some(&Json::from("succeeded"))
                    && outcome.and_then(|v| v.get("action")) == Some(&Json::from("notify"))
                    && field(
                        field(field(outcome.unwrap(), "reply")?, "object")?,
                        "objectId",
                    )? == &Json::from(saved.pin.object_id.clone())
                    && operation.value.get("definitionHash") == saved.value.get("definitionHash")
                    && operation.value.get("workspaceId") == saved.value.get("workspaceId")
                    && same(
                        field(field(&operation.value, "request")?, "channel")?,
                        field(&saved.value, "channel")?,
                    )?,
                "chat_identity_conflict",
                "A service reply requires its original completed producer operation.",
            )?;
            replies.push(
                self.reply(
                    Reference::Pinned(&saved.pin),
                    field(&saved.value, "channel")?,
                )
                .await?,
            );
        }
        Ok(PendingNotices {
            replies,
            next_cursor: page.get("nextCursor").cloned(),
        })
    }
    pub async fn notice_parts(&self, first: &Document) -> IvyResult<Vec<Document>> {
        let origin = field(&first.value, "origin")?;
        require(
            origin.get("kind") == Some(&Json::from("native"))
                && first.value.get("partIndex") == Some(&Json::Number(0.0)),
            "chat_identity_conflict",
            "A notice must begin with its original first reply.",
        )?;
        let mut parts = Vec::new();
        let mut cursor = None;
        loop {
            let mut params = Json::object([
                ("contractKey", "chat-bridge/reply".into()),
                (
                    "where",
                    and(vec![
                        self.root(),
                        predicate(
                            "eq",
                            "data:/workspaceId",
                            field(&first.value, "workspaceId")?.clone(),
                        ),
                        predicate(
                            "eq",
                            "data:/origin/inputId",
                            field(origin, "inputId")?.clone(),
                        ),
                    ]),
                ),
                ("orderBy", order("data:/partIndex")),
                ("limit", Json::Number(32.0)),
            ]);
            if let Some(cursor) = cursor {
                params.insert("cursor", cursor);
            }
            let page = self
                .operations
                .store
                .client
                .request("objects.query", params, RequestOptions::default())
                .await?;
            for item in array(&page, "items")? {
                let part = self
                    .reply(
                        Reference::Pinned(&pin(item)?),
                        field(&first.value, "channel")?,
                    )
                    .await?;
                require(
                    number(&part.value, "partIndex")? == parts.len() as f64
                        && same(field(&part.value, "origin")?, origin)?
                        && part.value.get("definitionHash") == first.value.get("definitionHash")
                        && number(&part.value, "sequence")?
                            == number(&first.value, "sequence")? + parts.len() as f64,
                    "chat_identity_conflict",
                    "Notice delivery must retain every ordered part of the same Main answer.",
                )?;
                parts.push(part);
            }
            cursor = page.get("nextCursor").filter(|v| truthy(v)).cloned();
            if cursor.is_none() {
                break;
            }
        }
        require(
            parts
                .first()
                .is_some_and(|part| part.pin.object_id == first.pin.object_id),
            "chat_identity_conflict",
            "The exact first notice reply is unavailable.",
        )?;
        Ok(parts)
    }
    pub async fn confirm_notice(&self, parts: &[Document]) -> IvyResult<()> {
        require(
            parts
                .first()
                .is_some_and(|p| p.value.get("partIndex") == Some(&Json::Number(0.0))),
            "chat_identity_conflict",
            "Notice confirmation requires its ordered reply parts.",
        )?;
        // Confirm part zero last: it is the producer's complete-answer receipt.
        for reply in parts.iter().skip(1).chain(parts.first()) {
            self.mark(
                reply,
                "confirmed",
                &self.operations.store.now_iso()?,
                &format!("whatsapp-notice:{}", reply.pin.object_id),
            )
            .await?;
        }
        Ok(())
    }
    pub async fn history(&self, caller: &JsString, request: &Json) -> IvyResult<Json> {
        self.operations.admission.authorize(
            caller,
            expected_bridge(request)?,
            request.get("channel").filter(|v| truthy(v)),
        )?;
        validate_chat("HistoryQuery", request)?;
        let pages = self
            .visible(
                &["chat-bridge/input", "chat-bridge/reply"],
                field(request, "channel")?,
                number(request, "afterSequence")?,
                number(request, "limit")?,
            )
            .await?;
        let mut through = MAXIMUM_SEQUENCE;
        let mut has_more = false;
        let mut candidates = Vec::new();
        for page in pages {
            if page.has_more {
                through = through.min(number(
                    &page.entries.last().ok_or_else(IvyError::internal)?.value,
                    "sequence",
                )?);
            }
            has_more |= page.has_more;
            candidates.extend(page.entries);
        }
        candidates.sort_by(|a, b| {
            number(&a.value, "sequence")
                .unwrap_or(f64::NAN)
                .total_cmp(&number(&b.value, "sequence").unwrap_or(f64::NAN))
        });
        let mut sequences = HashSet::new();
        for candidate in &candidates {
            require(
                sequences.insert(number(&candidate.value, "sequence")?.to_bits()),
                "chat_identity_conflict",
                "Inputs and replies share one workspace sequence.",
            )?;
        }
        let mut entries = Vec::new();
        let mut cursor = field(request, "afterSequence")?.clone();
        for candidate in candidates {
            if number(&candidate.value, "sequence")? > through
                || entries.len() as f64 == number(request, "limit")?
            {
                has_more = true;
                break;
            }
            if candidate.key == "chat-bridge/input"
                && field(&candidate.value, "identity")?.get("messageId")
                    == candidate.value.get("operationId")
            {
                cursor = field(&candidate.value, "sequence")?.clone();
                continue;
            }
            let mut next = entries.clone();
            next.push(view(&candidate));
            if bytes(&Json::object([
                ("entries", Json::Array(next.clone())),
                (
                    "throughSequence",
                    field(&candidate.value, "sequence")?.clone(),
                ),
                ("hasMore", false.into()),
            ]))? > MAXIMUM_PAGE_BYTES
            {
                require(
                    !entries.is_empty(),
                    "chat_page_too_large",
                    "The next history entry exceeds the complete page byte limit.",
                )?;
                has_more = true;
                break;
            }
            entries = next;
            cursor = field(&candidate.value, "sequence")?.clone();
        }
        let page = Json::object([
            ("entries", Json::Array(entries)),
            ("throughSequence", cursor),
            ("hasMore", has_more.into()),
        ]);
        validate_chat("HistoryPage", &page)?;
        Ok(page)
    }
    async fn offer(&self, reference: Reference<'_>, caller: &JsString) -> IvyResult<Offer> {
        let document = self.read("chat-bridge/offer", reference).await?;
        let value = &document.value;
        require(
            document.pin.revision == 1
                && value.get("callerPrincipalId") == Some(&Json::String(caller.clone())),
            "chat_offer_mismatch",
            "Only the original caller can use an immutable offer.",
        )?;
        let admission = self.operations.admission;
        admission.authorize(
            caller,
            &admission.expected(caller),
            value.get("channel").filter(|v| truthy(v)),
        )?;
        let operation = self
            .operations
            .find(caller, js(value, "operationId")?)
            .await?;
        let mut matches = false;
        if let Some(operation) = &operation {
            let request = field(&operation.value, "request")?;
            if request.get("action") == Some(&Json::from("receive")) {
                let selected = array(value, "replies")?;
                let mut ids = HashSet::new();
                for pin in selected {
                    ids.insert(js(pin, "objectId")?);
                }
                matches = document.metadata.get("name")
                    == Some(&format!("Chat offer {}", operation.pin.object_id).into())
                    && same_channel(field(value, "channel")?, field(request, "channel")?)
                    && value.get("afterSequence") == request.get("afterSequence")
                    && value.get("createdAt") == operation.value.get("createdAt")
                    && selected.len() as f64 <= number(request, "limit")?
                    && ids.len() == selected.len();
            }
        }
        require(
            matches,
            "chat_offer_mismatch",
            "An offer must retain its exact receiving operation, channel and selected reply identities.",
        )?;
        let operation = operation.unwrap();
        let mut replies = Vec::new();
        for selected in array(value, "replies")? {
            let selected_pin = pin(selected)?;
            let reply = match self
                .reply(Reference::Pinned(&selected_pin), field(value, "channel")?)
                .await
            {
                Ok(reply) => reply,
                Err(error) if error.code == "revision_pruned" => {
                    let outcome = operation.value.get("outcome");
                    let snapshot =
                        if outcome.and_then(|v| v.get("action")) == Some(&Json::from("receive")) {
                            array(outcome.unwrap(), "replies")?.iter().find(|r| {
                                r.get("object")
                                    .is_some_and(|p| same(p, selected).unwrap_or(false))
                            })
                        } else {
                            None
                        };
                    require(
                        snapshot.is_some(),
                        "chat_offer_mismatch",
                        "A pruned offer reply needs its original retained receive snapshot.",
                    )?;
                    let snapshot = field(snapshot.unwrap(), "data")?;
                    let current = self
                        .reply(
                            Reference::Current(&selected_pin.object_id),
                            field(value, "channel")?,
                        )
                        .await?;
                    require(
                        current.pin.revision >= selected_pin.revision
                            && same(&immutable_reply(&current.value), &immutable_reply(snapshot))?
                            && (snapshot.get("state") != Some(&Json::from("confirmed"))
                                || current.value.get("state") == Some(&Json::from("confirmed")))
                            && (snapshot.get("state") == Some(&Json::from("pending"))
                                || current.value.get("state") != Some(&Json::from("pending"))),
                        "chat_identity_conflict",
                        "An offered reply can change its delivery state, but not its original immutable content.",
                    )?;
                    current
                }
                Err(error) => return Err(error),
            };
            replies.push(reply);
        }
        let mut previous = number(value, "afterSequence")?;
        for reply in &replies {
            let sequence = number(&reply.value, "sequence")?;
            require(
                sequence > previous,
                "chat_offer_mismatch",
                "An offer cursor cannot skip or reorder its exact selected replies.",
            )?;
            previous = sequence;
        }
        require(
            value.get("throughSequence") == Some(&Json::Number(previous)),
            "chat_offer_mismatch",
            "An offer cursor cannot skip or reorder its exact selected replies.",
        )?;
        Ok(Offer { document, replies })
    }
    async fn remember_confirmed_notice(&self, reply: &Document) -> IvyResult<()> {
        let value = &reply.value;
        if value.get("state") != Some(&Json::from("confirmed"))
            || value.get("partIndex") != Some(&Json::Number(0.0))
        {
            return Ok(());
        }
        let origin = field(value, "origin")?;
        if origin.get("kind") != Some(&Json::from("native")) {
            return Ok(());
        }
        let input = self
            .read(
                "chat-bridge/input",
                Reference::Current(&text(origin, "inputId")?),
            )
            .await?;
        let operation = self
            .operations
            .find(
                js(field(&input.value, "identity")?, "senderPrincipalId")?,
                js(&input.value, "operationId")?,
            )
            .await?;
        if operation
            .as_ref()
            .and_then(|d| d.value.get("request"))
            .and_then(|v| v.get("action"))
            == Some(&Json::from("notify"))
        {
            self.operations
                .store
                .remember_notice_delivery(&reply.pin.object_id)
                .await
                .map_err(|e| e.into_ivy())?;
        }
        Ok(())
    }
    async fn mark(&self, original: &Document, state: &str, at: &str, scope: &str) -> IvyResult<()> {
        for _ in 0..16 {
            let current = self
                .reply(
                    Reference::Current(&original.pin.object_id),
                    field(&original.value, "channel")?,
                )
                .await?;
            require(
                same(
                    &immutable_reply(&current.value),
                    &immutable_reply(&original.value),
                )? && (original.value.get("state") != Some(&Json::from("confirmed"))
                    || current.value.get("state") == Some(&Json::from("confirmed")))
                    && (original.value.get("state") == Some(&Json::from("pending"))
                        || current.value.get("state") != Some(&Json::from("pending"))),
                "chat_identity_conflict",
                "The original reply content and confirmed delivery cannot regress.",
            )?;
            if current.value.get("state") == Some(&Json::from("confirmed"))
                || current.value.get("state") == Some(&Json::from(state))
            {
                return self.remember_confirmed_notice(&current).await;
            }
            let mut next = current.value.clone();
            next.insert("state", state.into());
            next.insert(
                "firstOfferedAt",
                current
                    .value
                    .get("firstOfferedAt")
                    .filter(|v| !v.is_null())
                    .cloned()
                    .unwrap_or_else(|| at.into()),
            );
            next.insert(
                "confirmedAt",
                if state == "confirmed" {
                    at.into()
                } else {
                    Json::Null
                },
            );
            match self
                .write(
                    "chat-bridge/reply",
                    &next,
                    scope,
                    &format!(
                        "{state}:{}:{}:{}",
                        current.pin.object_id,
                        current.pin.revision,
                        hash_json(&next)?
                    ),
                    Destination::Update {
                        object_id: &current.pin.object_id,
                        expected_revision: current.pin.revision,
                    },
                )
                .await
            {
                Ok(pin) => {
                    return self
                        .remember_confirmed_notice(&Document {
                            pin,
                            value: next,
                            ..current
                        })
                        .await;
                }
                Err(error) if contention(&error) => {}
                Err(error) => return Err(error),
            }
        }
        Err(IvyError::new(
            "chat_contention",
            "Recover the original offer or acknowledgement after concurrent reply updates.",
        )
        .with_outcome(Outcome::Unknown))
    }
    pub async fn receive(&self, caller: &JsString, original: &Json) -> IvyResult<Json> {
        self.operations.admission.authorize(
            caller,
            expected_bridge(original)?,
            original.get("channel").filter(|v| truthy(v)),
        )?;
        validate_chat("ReceiveRequest", original)?;
        let request = original.clone();
        let operation = self.operations.begin(caller, &request).await?;
        if terminal(&operation) {
            return Ok(operation.value);
        }
        let scope = &operation.pin.object_id;
        let name = format!("Chat offer {scope}");
        let mut saved = self.named("chat-bridge/offer", &name).await?;
        if saved.is_none() {
            let page = self
                .visible(
                    &["chat-bridge/reply"],
                    field(&request, "channel")?,
                    number(&request, "afterSequence")?,
                    number(&request, "limit")?,
                )
                .await?
                .remove(0);
            let mut offer = Json::object([
                ("schemaVersion", Json::Number(1.0)),
                ("operationId", field(&request, "operationId")?.clone()),
                ("callerPrincipalId", Json::String(caller.clone())),
                ("channel", field(&request, "channel")?.clone()),
                ("afterSequence", field(&request, "afterSequence")?.clone()),
                ("throughSequence", field(&request, "afterSequence")?.clone()),
                ("replies", Json::Array(vec![])),
                ("createdAt", field(&operation.value, "createdAt")?.clone()),
            ]);
            let reserved = Json::object([
                ("objectId", "00000000-0000-0000-0000-000000000000".into()),
                ("revision", Json::Number(1.0)),
            ]);
            let mut selected = Vec::new();
            let mut pins = Vec::new();
            for candidate in page
                .entries
                .iter()
                .take(number(&request, "limit")? as usize)
            {
                let mut next = offer.clone();
                let mut next_pins = pins.clone();
                next_pins.push(candidate.pin.json());
                next.insert(
                    "throughSequence",
                    field(&candidate.value, "sequence")?.clone(),
                );
                next.insert("replies", Json::Array(next_pins.clone()));
                let mut next_selected = selected.clone();
                next_selected.push(view(candidate));
                let outcome = Json::object([
                    ("action", "receive".into()),
                    ("operationId", field(&request, "operationId")?.clone()),
                    (
                        "offer",
                        Json::object([("object", reserved.clone()), ("data", next.clone())]),
                    ),
                    ("replies", Json::Array(next_selected.clone())),
                ]);
                let mut envelope = operation.value.clone();
                envelope.insert("phase", "succeeded".into());
                envelope.insert("outcome", outcome);
                if bytes(&envelope)? > MAXIMUM_PAGE_BYTES {
                    require(
                        !selected.is_empty(),
                        "chat_page_too_large",
                        "The next reply exceeds the complete receive response byte limit.",
                    )?;
                    break;
                }
                selected = next_selected;
                pins = next_pins;
                offer = next;
            }
            match self
                .write(
                    "chat-bridge/offer",
                    &offer,
                    scope,
                    "publish-offer",
                    Destination::Create {
                        parent_id: self.operations.store.root_object_id.as_deref(),
                        name: &name,
                    },
                )
                .await
            {
                Ok(pin) => {
                    saved = Some(
                        self.read("chat-bridge/offer", Reference::Pinned(&pin))
                            .await?,
                    )
                }
                Err(error) if contention(&error) => {
                    saved = self.named("chat-bridge/offer", &name).await?;
                    require(
                        saved.is_some(),
                        "chat_offer_mismatch",
                        "The competing original offer must remain discoverable.",
                    )?;
                }
                Err(error) => return Err(error),
            }
        }
        let offered = self
            .offer(Reference::Pinned(&saved.unwrap().pin), caller)
            .await?;
        let outcome = Json::object([
            ("action", "receive".into()),
            ("operationId", field(&request, "operationId")?.clone()),
            ("offer", view(&offered.document)),
            (
                "replies",
                Json::Array(offered.replies.iter().map(view).collect()),
            ),
        ]);
        let mut envelope = operation.value.clone();
        envelope.insert("phase", "succeeded".into());
        envelope.insert("outcome", outcome.clone());
        require(
            bytes(&envelope)? <= MAXIMUM_PAGE_BYTES,
            "chat_page_too_large",
            "The original complete receive response exceeds its byte limit.",
        )?;
        // Preserve immutable snapshots before marking mutable reply revisions.
        let completed = self.operations.finish(scope, &outcome).await?;
        let at = text(&offered.document.value, "createdAt")?;
        for reply in &offered.replies {
            self.mark(reply, "outcome_unknown", &at, scope).await?;
        }
        Ok(completed.value)
    }
    pub async fn acknowledge(&self, caller: &JsString, original: &Json) -> IvyResult<Json> {
        self.operations
            .admission
            .authorize(caller, expected_bridge(original)?, None)?;
        validate_chat("AcknowledgeRequest", original)?;
        let request = original.clone();
        if let Some(previous) = self
            .operations
            .find(caller, js(&request, "operationId")?)
            .await?
        {
            require(
                previous.value.get("requestHash") == Some(&hash_json(&request)?.into()),
                "mutation_conflict",
                "This operation already names different ChatBridge arguments.",
            )?;
            if terminal(&previous) {
                return Ok(previous.value);
            }
        }
        let offered = self
            .offer(Reference::Current(&text(&request, "offerId")?), caller)
            .await?;
        let ids = array(&request, "replyIds")?;
        require(
            ids.iter().all(|id| {
                offered
                    .replies
                    .iter()
                    .any(|r| id == &Json::from(r.pin.object_id.clone()))
            }),
            "chat_offer_mismatch",
            "An acknowledgement may select only replies from this caller's exact original offer.",
        )?;
        let operation = self.operations.begin(caller, &request).await?;
        if terminal(&operation) {
            return Ok(operation.value);
        }
        let scope = &operation.pin.object_id;
        let name = format!("Chat acknowledgement {scope}");
        let value = Json::object([
            ("schemaVersion", Json::Number(1.0)),
            ("operationId", field(&request, "operationId")?.clone()),
            ("callerPrincipalId", Json::String(caller.clone())),
            ("offer", offered.document.pin.json()),
            ("replyIds", Json::Array(ids.to_vec())),
            ("createdAt", field(&operation.value, "createdAt")?.clone()),
        ]);
        let mut saved = self.named("chat-bridge/acknowledgement", &name).await?;
        if saved.is_none() {
            match self
                .write(
                    "chat-bridge/acknowledgement",
                    &value,
                    scope,
                    "publish-acknowledgement",
                    Destination::Create {
                        parent_id: self.operations.store.root_object_id.as_deref(),
                        name: &name,
                    },
                )
                .await
            {
                Ok(pin) => {
                    saved = Some(
                        self.read("chat-bridge/acknowledgement", Reference::Pinned(&pin))
                            .await?,
                    )
                }
                Err(error) if contention(&error) => {
                    saved = self.named("chat-bridge/acknowledgement", &name).await?;
                    require(
                        saved.is_some(),
                        "chat_offer_mismatch",
                        "The original acknowledgement must remain discoverable.",
                    )?;
                }
                Err(error) => return Err(error),
            }
        }
        let saved = saved.unwrap();
        require(
            saved.pin.revision == 1 && same(&saved.value, &value)?,
            "chat_offer_mismatch",
            "An acknowledgement cannot change its original offer, caller or selected reply identities.",
        )?;
        let at = text(&saved.value, "createdAt")?;
        for reply in offered
            .replies
            .iter()
            .filter(|r| ids.contains(&Json::from(r.pin.object_id.clone())))
        {
            self.mark(reply, "confirmed", &at, scope).await?;
        }
        let reply_ids = ids
            .iter()
            .map(Json::as_str)
            .collect::<IvyResult<Vec<_>>>()?;
        self.operations
            .store
            .confirm_offer(&offered.document.pin.object_id, &reply_ids)
            .await
            .map_err(|e| e.into_ivy())?;
        Ok(self
            .operations
            .finish(
                scope,
                &Json::object([
                    ("action", "acknowledge".into()),
                    ("operationId", field(&request, "operationId")?.clone()),
                    ("acknowledgement", view(&saved)),
                ]),
            )
            .await?
            .value)
    }
}
