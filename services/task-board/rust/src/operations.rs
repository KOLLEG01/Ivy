//! Retained prepare/apply/receipt protocol. Store adapters validate exact domain
//! schemas; this layer preserves original JSON values and destination identities.
use crate::journal_store::persistent_reads::json_values::Value;
use ivy_contracts::{IvyError, IvyJson as Json, IvyResult, JsString};

#[path = "decoded_mutations.rs"]
pub(crate) mod decoded;

#[path = "local_operations.rs"]
pub(crate) mod local;
pub(crate) use local::apply_graph;

#[path = "upload_request.rs"]
mod upload_request;
pub use upload_request::original_upload_request;

#[derive(Debug, Clone)]
pub struct Document {
    pub pin: ObjectPin,
    pub value: Json,
    pub parent_id: Option<JsString>,
    pub content_hash: String,
}

/// An immutable object address inside the workflow. JSON is produced only for
/// persisted values, evidence hashes and transport ports.
#[derive(Debug, Clone, PartialEq)]
pub struct ObjectPin {
    pub object_id: JsString,
    pub revision: f64,
}

impl ObjectPin {
    pub fn from_json(mut value: Json) -> IvyResult<Self> {
        value
            .required("objectId")?
            .as_js_string()
            .ok_or_else(IvyError::internal)?;
        let revision = value
            .required("revision")?
            .as_f64()
            .ok_or_else(IvyError::internal)?;
        let object_id = value.remove("objectId").unwrap().into_js_string().unwrap();
        Ok(Self {
            object_id,
            revision,
        })
    }

    pub fn to_json(&self) -> Json {
        self.clone().into_json()
    }

    pub fn into_json(self) -> Json {
        Json::object([
            ("objectId", Json::String(self.object_id)),
            ("revision", Json::Number(self.revision)),
        ])
    }
}

impl Document {
    pub(crate) fn retained_bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            + self.pin.object_id.0.capacity() * 2
            + self.value.retained_bytes()
            + self.content_hash.capacity()
            + self.parent_id.as_ref().map_or(0, |id| id.0.capacity() * 2)
    }
}

/// The read owner's record, before a caller selects the domain document or
/// the complete transport envelope. Metadata and revision retain their exact
/// admitted fields, including data that the workflow does not interpret.
#[derive(Debug, Clone)]
pub(crate) struct StoredRecord {
    pub key: String,
    pub pin: ObjectPin,
    pub value: Json,
    pub metadata: Json,
    pub revision: Json,
}

impl StoredRecord {
    pub(crate) fn document(&self) -> IvyResult<Document> {
        Ok(Document {
            pin: self.pin.clone(),
            value: self.value.clone(),
            parent_id: self
                .metadata
                .get("parentId")
                .and_then(Json::as_js_string)
                .cloned(),
            content_hash: self.revision.required("contentHash")?.as_str()?,
        })
    }

    pub(crate) fn into_json(self) -> Json {
        Json::object([
            ("key", self.key.into()),
            ("pin", self.pin.into_json()),
            ("value", self.value),
            ("metadata", self.metadata),
            ("revision", self.revision),
        ])
    }

    pub(crate) fn into_document(self) -> IvyResult<Document> {
        self.into_document_with_metadata()
            .map(|(document, _)| document)
    }

    pub(crate) fn into_document_with_metadata(self) -> IvyResult<(Document, Json)> {
        let document = Document {
            pin: self.pin,
            value: self.value,
            parent_id: self
                .metadata
                .get("parentId")
                .and_then(Json::as_js_string)
                .cloned(),
            content_hash: self.revision.required("contentHash")?.as_str()?,
        };
        Ok((document, self.metadata))
    }

    pub(crate) fn retained_bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            + self.key.capacity()
            + self.pin.object_id.0.capacity() * 2
            + self.value.retained_bytes()
            + self.metadata.retained_bytes()
            + self.revision.retained_bytes()
    }
}

pub(crate) const PREPARED_PAYLOAD: &str = "task-board/prepared-payload";

/// A snapshot's storage contract can differ from its publication contract.
/// The publication version and exact references travel with compact payloads.
pub struct PreparedPayload {
    pub key: String,
    pub value: Json,
    pub version: Option<JsString>,
}

/// Explicit changes over an already stored, immutable conversation root.
#[derive(Clone, Copy)]
pub enum TaskChange {
    Header,
    AppendSystem,
    AppendComment,
    Receipts,
    NativeCompletion,
    QuestionRecipient,
}

/// The latest intent and existence of a reply in one immutable conversation.
pub struct CompletionComments {
    pub handoff: Option<Json>,
    pub answered: bool,
}
pub(crate) fn completion_comments(
    task: &Json,
    run: &JsString,
    inputs: &[Json],
) -> IvyResult<CompletionComments> {
    let comments = task
        .required("comments")?
        .as_array()
        .ok_or_else(IvyError::internal)?;
    Ok(CompletionComments {
        handoff: crate::engine_read::conversation::run_handoff(task, run),
        answered: !inputs.is_empty()
            && comments.iter().any(|comment| {
                comment.get("authorKind").is_some_and(|v| v.is_str("agent"))
                    && comment
                        .get("run")
                        .and_then(|v| v.get("objectId"))
                        .and_then(Json::as_js_string)
                        == Some(run)
                    && comment
                        .get("replyTo")
                        .is_some_and(|v| !v.is_null() && inputs.contains(v))
            }),
    })
}

pub(crate) fn selected_task_comments(task: &Json, ids: &[Json]) -> IvyResult<Vec<Json>> {
    Ok(task
        .required("comments")?
        .as_array()
        .ok_or_else(IvyError::internal)?
        .iter()
        .filter(|comment| comment.get("commentId").is_some_and(|id| ids.contains(id)))
        .cloned()
        .collect())
}

/// The journal implementation owns CAS, schema validation, mutation receipts and
/// local/remote routing. No network effect may be inferred from a failed call.
#[allow(async_fn_in_trait)]
pub trait Store {
    fn root_object_id(&self) -> Option<JsString>;
    fn contract_version(&self, key: &str) -> IvyResult<String>;
    fn retired_operation(&self, caller: &JsString, id: &JsString) -> IvyResult<bool>;
    fn retired_operation_receipt(&self, _: &JsString, _: &JsString) -> IvyResult<Option<Json>> {
        Ok(None)
    }
    fn local_key(&self, id: &JsString) -> IvyResult<Option<String>>;
    fn complete_local_workflow(&self, id: &JsString, at: &JsString) -> IvyResult<()>;
    /// One current-head query; incomplete local work cannot prove release safety.
    fn task_operations_pending(
        &self,
        _task: &JsString,
        _except: Option<&JsString>,
    ) -> IvyResult<bool> {
        Err(IvyError::new(
            "task_board_pending_work_unavailable",
            "The owning journal must check accepted ticket work before release.",
        ))
    }
    async fn named(
        &self,
        key: &str,
        name: &str,
        parent: Option<&JsString>,
    ) -> IvyResult<Option<Document>>;
    /// Address is either an object ID string or an exact object/revision pin.
    async fn read(&self, key: &str, address: &Json, upgrade: bool) -> IvyResult<Document>;
    /// The same primary/archive policy as read, without an indexed conversation.
    async fn read_task_header(&self, address: &Json, primary: bool) -> IvyResult<Document> {
        self.read("task-board/task", address, primary).await
    }
    /// Select immutable comment records without replacing the Task's conversation.
    async fn read_task_comments(
        &self,
        task: &Json,
        _task_id: &JsString,
        ids: &[Json],
    ) -> IvyResult<Vec<Json>> {
        selected_task_comments(task, ids)
    }
    async fn read_completion_comments(
        &self,
        task: &Json,
        _task_id: &JsString,
        run: &JsString,
        inputs: &[Json],
    ) -> IvyResult<CompletionComments> {
        completion_comments(task, run, inputs)
    }
    async fn has_task_comment(
        &self,
        task: &Json,
        task_id: &JsString,
        id: &JsString,
    ) -> IvyResult<bool> {
        Ok(!self
            .read_task_comments(task, task_id, &[Json::String(id.clone())])
            .await?
            .is_empty())
    }
    async fn read_prepared(&self, key: &str, address: &Json) -> IvyResult<Document> {
        self.read(key, address, false).await
    }
    async fn prepare_payload(
        &self,
        key: &str,
        value: Json,
        _destination: &Json,
    ) -> IvyResult<PreparedPayload> {
        Ok(PreparedPayload {
            key: key.into(),
            value,
            version: None,
        })
    }
    async fn prepare_task_change(
        &self,
        _value: &Json,
        _destination: &Json,
        _change: TaskChange,
    ) -> IvyResult<PreparedPayload> {
        Err(IvyError::internal())
    }
    async fn write(
        &self,
        key: &str,
        value: &Json,
        mutation: &str,
        destination: &Json,
    ) -> IvyResult<Json>;
    async fn prepared_mutation_id(&self, mutation: &str) -> IvyResult<JsString>;
    async fn write_prepared(
        &self,
        key: &str,
        value: &Json,
        mutation: &JsString,
        destination: &Json,
        version: Option<&JsString>,
        references: Option<&Json>,
    ) -> IvyResult<Json>;
}

pub struct Operations<S, C> {
    pub store: S,
    /// Inject the owner's ISO UTC clock; called at the same persisted transitions as TS.
    pub now: C,
}

pub fn identity_hash(caller: &JsString, id: &JsString) -> IvyResult<String> {
    local::identity_hash(&Value::String(caller.clone()), &Value::String(id.clone()))
}
pub fn mutation(scope: &JsString, step: &str) -> IvyResult<String> {
    let Value::String(ref value) =
        local::mutation(&Value::String(scope.clone()), local::text(step))?
    else {
        unreachable!()
    };
    value.to_utf8()
}
impl<S: Store, C: Fn() -> String> Operations<S, C> {
    pub(crate) fn decoded(&self) -> Operations<decoded::DecodedStore<'_, S>, &C> {
        Operations {
            store: decoded::DecodedStore(&self.store),
            now: &self.now,
        }
    }
    pub async fn find(&self, caller: &JsString, id: &JsString) -> IvyResult<Option<Document>> {
        local::find_graph(
            &self.decoded(),
            &Value::String(caller.clone()),
            &Value::String(id.clone()),
        )
        .await
        .map_err(decoded::failure)
    }
    pub async fn begin(&self, request: &Json, actor: &Json) -> IvyResult<Document> {
        local::begin_graph(&self.decoded(), request, actor)
            .await
            .map_err(decoded::failure)
    }
    /// Transfer an owned public request into its immutable acceptance record.
    pub async fn begin_owned(&self, request: Json, actor: &Json) -> IvyResult<Document> {
        local::begin_graph_with(
            &self.decoded(),
            std::borrow::Cow::Owned(request),
            actor,
            || std::future::ready(Ok(Json::Null)),
            &Json::Null,
        )
        .await
        .map_err(decoded::failure)
    }
    /// Bind completion identity only on first admission. Replays keep the
    /// original request and binding without repeating validation or task reads.
    pub async fn begin_with_handoff<F: std::future::Future<Output = IvyResult<Json>>>(
        &self,
        request: &Json,
        actor: &Json,
        handoff: impl FnOnce() -> F,
    ) -> IvyResult<Document> {
        local::begin_graph_with(
            &self.decoded(),
            std::borrow::Cow::Borrowed(request),
            actor,
            || async { handoff().await.map_err(Into::into) },
            &Value::Null,
        )
        .await
        .map_err(decoded::failure)
    }
    pub async fn prepare_native(&self, id: &JsString, proposed: &Json) -> IvyResult<Json> {
        decoded::json_result(
            local::prepare_native_graph(&self.decoded(), &Value::String(id.clone()), proposed)
                .await,
        )
    }
    pub async fn observe_native(
        &self,
        id: &JsString,
        key: &JsString,
        observation: &Json,
    ) -> IvyResult<Json> {
        decoded::json_result(
            local::observe_native_graph(
                &self.decoded(),
                &Value::String(id.clone()),
                &Value::String(key.clone()),
                observation,
            )
            .await,
        )
    }
    pub async fn archive_identity(&self, id: &JsString) -> IvyResult<JsString> {
        decoded::string_result(
            local::archive_identity_graph(&self.decoded(), &Value::String(id.clone())).await,
        )
    }
    pub async fn stage(
        &self,
        id: &JsString,
        index: f64,
        key: &str,
        destination: &Json,
        proposed: &Json,
    ) -> IvyResult<Json> {
        decoded::json_result(
            local::stage_graph_with(
                &self.decoded(),
                &Value::String(id.clone()),
                index,
                key,
                destination,
                || std::future::ready(Ok((proposed).clone())),
            )
            .await,
        )
    }
    pub async fn stage_task_change(
        &self,
        id: &JsString,
        index: f64,
        destination: &Json,
        proposed: &Json,
        change: TaskChange,
    ) -> IvyResult<Json> {
        decoded::json_result(
            local::stage_prepared_graph_with(
                &self.decoded(),
                &Value::String(id.clone()),
                index,
                "task-board/task",
                destination,
                || async {
                    self.store
                        .prepare_task_change(proposed, destination, change)
                        .await
                        .map_err(Into::into)
                },
            )
            .await,
        )
    }
    pub async fn apply(&self, id: &JsString, index: usize) -> IvyResult<Json> {
        decoded::json_result(apply_graph(&self.decoded(), &Value::String(id.clone()), index).await)
    }
    pub async fn finish(&self, id: &JsString, outcome: &Json) -> IvyResult<Json> {
        decoded::json_result(
            local::finish_graph(&self.decoded(), &Value::String(id.clone()), outcome).await,
        )
    }
    pub async fn failure(
        &self,
        id: &JsString,
        error: &IvyError,
        needs_attention: bool,
    ) -> IvyResult<Document> {
        local::failure_graph(
            &self.decoded(),
            &Value::String(id.clone()),
            error,
            needs_attention,
        )
        .await
        .map_err(decoded::failure)
    }
}
