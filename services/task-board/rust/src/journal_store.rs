//! Local workflow persistence and Hive publication on the same store interface.
//! The owner supplies exact contract admission and one action's read cache. This
//! layer neither upgrades records nor provides a permissive schema fallback.
use crate::{
    evidence_bytes::{BlobStore, ChunkKey},
    local_blobs::{BlobError, BlobPin, LocalBlobs},
    local_gates::LocalGates,
    local_journal::{LocalJournal, require},
    local_retention::{HistoryLimits, LocalRetention},
    local_workspaces::{WorkspaceDocument, WorkspaceOwner},
    operations::{Document, Store},
    read_evidence::ReadEvidenceStore,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString,
    hash::{digest, hash_json},
    json::{canonical, parse},
    limits::MANAGEMENT_FRAME_BYTES,
};
use ivy_sdk::{
    operation_ids::scoped_operation_id,
    rpc::{RequestOptions, RpcClient},
};
use ivy_store::sqlite::error;
use std::{
    cell::RefCell,
    collections::HashSet,
    future::Future,
    pin::Pin,
    rc::Rc,
    sync::Arc,
    task::{Context, Poll},
};

#[path = "comment_writes.rs"]
pub(crate) mod comment_writes;
#[path = "durable_admission.rs"]
pub mod durable_admission;
#[path = "mutation_ports.rs"]
pub(crate) mod mutation_ports;
#[path = "persistent_reads.rs"]
pub mod persistent_reads;
#[path = "reference_writes.rs"]
pub(crate) mod reference_writes;
#[path = "write_completion.rs"]
pub(crate) mod write_completion;

#[cfg(test)]
#[path = "../../../../tests/fixtures/task-board-execution-completion.rs"]
mod execution_completion_tests;

/// Bind this to the accepted TaskBoard contract bundle. Retained public versions
/// can be admitted as exact records, but are never rewritten during a read.
pub trait Contracts {
    fn version(&self, key: &str) -> IvyResult<String>;
    fn local(&self, key: &str) -> IvyResult<bool>;
    fn readable_version(&self, key: &str, version: &JsString) -> IvyResult<bool>;
    fn validate_record(&self, key: &str, version: &JsString, value: &Json) -> IvyResult<()>;
    fn validate_definition(&self, name: &str, value: &Json) -> IvyResult<()>;
}
impl<P: Contracts + ?Sized> Contracts for &P {
    fn version(&self, key: &str) -> IvyResult<String> {
        (**self).version(key)
    }
    fn local(&self, key: &str) -> IvyResult<bool> {
        (**self).local(key)
    }
    fn readable_version(&self, key: &str, version: &JsString) -> IvyResult<bool> {
        (**self).readable_version(key, version)
    }
    fn validate_record(&self, key: &str, version: &JsString, value: &Json) -> IvyResult<()> {
        (**self).validate_record(key, version, value)
    }
    fn validate_definition(&self, name: &str, value: &Json) -> IvyResult<()> {
        (**self).validate_definition(name, value)
    }
}

#[derive(Clone)]
struct ReadRecord {
    document: Document,
    archived: bool,
    metadata: Option<Json>,
}

#[derive(Clone)]
struct CachedRecord {
    object_id: Json,
    document: Option<ReadRecord>,
    materialized: Option<Json>,
}
type CacheEntries = Rc<RefCell<Vec<(Json, CachedRecord)>>>;
enum CacheBinding {
    Fixed(CacheEntries),
    Scoped(RefCell<Option<CacheEntries>>),
}
/// An explicit cache for one action. A service owner uses ReadScopes::cache
/// instead, so independent asynchronous actions cannot share cached pins.
pub struct ReadCache(CacheBinding);
impl Default for ReadCache {
    fn default() -> Self {
        Self(CacheBinding::Fixed(CacheEntries::default()))
    }
}
impl ReadCache {
    fn entries(&self) -> Option<CacheEntries> {
        match &self.0 {
            CacheBinding::Fixed(entries) => Some(entries.clone()),
            CacheBinding::Scoped(active) => active.borrow().clone(),
        }
    }
}
/// Async-local action caches on the journal's local owner. Every poll installs
/// its captured context and restores the preceding one, including on unwind.
pub struct ReadScopes {
    cache: ReadCache,
}
pub(crate) struct CapturedReadScope(Option<CacheEntries>);
impl Default for ReadScopes {
    fn default() -> Self {
        Self {
            cache: ReadCache(CacheBinding::Scoped(RefCell::default())),
        }
    }
}
impl ReadScopes {
    pub fn cache(&self) -> &ReadCache {
        &self.cache
    }
    /// Nested actions inherit the current map; independent actions get a new one.
    pub fn with_read_cache<F: Future>(&self, work: F) -> ReadScopeFuture<'_, F> {
        ReadScopeFuture {
            scopes: self,
            entries: Some(self.cache.entries().unwrap_or_default()),
            work: Box::pin(work),
        }
    }
    /// Capture at submission time when work is moved to the owner executor.
    /// Work submitted outside an action keeps an absent context, not a new map.
    pub fn inherit<F: Future>(&self, work: F) -> ReadScopeFuture<'_, F> {
        self.resume(self.capture(), work)
    }
    pub(crate) fn capture(&self) -> CapturedReadScope {
        CapturedReadScope(self.cache.entries())
    }
    pub(crate) fn resume<F: Future>(
        &self,
        context: CapturedReadScope,
        work: F,
    ) -> ReadScopeFuture<'_, F> {
        ReadScopeFuture {
            scopes: self,
            entries: context.0,
            work: Box::pin(work),
        }
    }
}
pub struct ReadScopeFuture<'a, F> {
    scopes: &'a ReadScopes,
    entries: Option<CacheEntries>,
    work: Pin<Box<F>>,
}
struct RestoreCache<'a> {
    active: &'a RefCell<Option<CacheEntries>>,
    previous: Option<CacheEntries>,
}
impl Drop for RestoreCache<'_> {
    fn drop(&mut self) {
        self.active.replace(self.previous.take());
    }
}
impl<F: Future> Future for ReadScopeFuture<'_, F> {
    type Output = F::Output;
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        let CacheBinding::Scoped(active) = &this.scopes.cache.0 else {
            unreachable!("ReadScopes always owns a scoped cache")
        };
        let _restore = RestoreCache {
            active,
            previous: active.replace(this.entries.clone()),
        };
        this.work.as_mut().poll(cx)
    }
}

/// Runs on the journal owner's blocking context. A transaction never spans a
/// remote await; retention rechecks its graph before committing a collection.
pub struct JournalStore<'a, R, P, C, I> {
    pub journal: LocalJournal<'a>,
    pub client: Arc<R>,
    pub contracts: P,
    pub limits: HistoryLimits,
    pub now: C,
    pub new_id: I,
    pub cache: Option<&'a ReadCache>,
}

fn string<'a>(value: &'a Json, key: &str) -> IvyResult<&'a JsString> {
    value
        .required(key)?
        .as_js_string()
        .ok_or_else(IvyError::internal)
}
fn nullable(value: Option<&JsString>) -> Json {
    value.cloned().map(Json::String).unwrap_or(Json::Null)
}
fn pin(result: &Json) -> IvyResult<Json> {
    Ok(Json::object([
        (
            "objectId",
            result.required("object")?.required("id")?.clone(),
        ),
        (
            "revision",
            result.required("revision")?.required("revision")?.clone(),
        ),
    ]))
}
fn blob_error(failure: BlobError) -> IvyError {
    match failure {
        BlobError::Domain(failure) => failure,
        BlobError::Sqlite(failure) => error(failure),
    }
}

fn attachment_content_matches(value: &Json, bytes: &[u8]) -> IvyResult<bool> {
    let value = value.as_js_string().ok_or_else(IvyError::internal)?;
    Ok(value
        .to_utf8()
        .ok()
        .and_then(|text| STANDARD.decode(text).ok())
        .is_some_and(|decoded| decoded == bytes))
}

impl<R: RpcClient + 'static, P: Contracts, C: Fn() -> String, I: Fn() -> JsString>
    JournalStore<'_, R, P, C, I>
{
    fn retention(&self) -> LocalRetention<'_, R> {
        LocalRetention {
            journal: LocalJournal {
                connection: self.journal.connection,
                root_object_id: self.journal.root_object_id.clone(),
            },
            client: self.client.as_ref(),
            limits: HistoryLimits {
                count: self.limits.count,
                bytes: self.limits.bytes,
            },
        }
    }
    pub async fn collect_local(&self) -> IvyResult<()> {
        self.retention().collect(0, 0, None, None).await
    }

    pub async fn read_record(
        &self,
        key: &str,
        address: &Json,
        primary: bool,
        include_archived: bool,
    ) -> IvyResult<Document> {
        Ok(self
            .record(key, address, primary, include_archived)
            .await?
            .document)
    }

    /// Preserve the observed Object metadata for lifecycle membership fences.
    /// Local reads obtain it only when requested; ordinary reads stay unchanged.
    pub async fn read_with_metadata(
        &self,
        key: &str,
        address: &Json,
        primary: bool,
        include_archived: bool,
    ) -> IvyResult<(Document, Json)> {
        let record = self.record(key, address, primary, include_archived).await?;
        let metadata = match record.metadata {
            Some(metadata) => metadata,
            None => self.journal.metadata(
                string(&record.document.pin, "objectId")?,
                &self.contracts.version(key)?,
            )?,
        };
        Ok((record.document, metadata))
    }

    pub async fn write_attachment(
        &self,
        task_id: &JsString,
        operation_id: &JsString,
        attachment_id: &JsString,
        bytes: &[u8],
    ) -> IvyResult<Json> {
        require(
            bytes.len() <= 8 * 1024 * 1024,
            "limit_exceeded",
            "Task attachments are limited to 8 MiB.",
        )?;
        let mutation = scoped_operation_id(
            self.client.clone(),
            &Json::Array(vec![
                "task-board-attachment".into(),
                Json::String(operation_id.clone()),
            ]),
        )
        .await?;
        let mut name = JsString::from("Task attachment ");
        name.0.extend_from_slice(&attachment_id.0);
        let version = self.contracts.version("task-board/attachment")?;
        let result = self
            .client
            .request(
                "objects.write",
                Json::object([
                    ("mutationId", Json::String(mutation)),
                    ("contractVersion", version.clone().into()),
                    ("references", Json::object(Vec::<(String, Json)>::new())),
                    (
                        "create",
                        Json::object([
                            ("parentId", Json::String(task_id.clone())),
                            ("ownerObjectId", Json::String(task_id.clone())),
                            ("contractKey", "task-board/attachment".into()),
                            ("name", Json::String(name.clone())),
                        ]),
                    ),
                    (
                        "content",
                        Json::object([
                            ("encoding", "base64".into()),
                            ("value", STANDARD.encode(bytes).into()),
                        ]),
                    ),
                ]),
                RequestOptions::default(),
            )
            .await;
        match result {
            Ok(result) => pin(&result),
            Err(error) if error.code == "revision_conflict" || error.code == "mutation_expired" => {
                let found = self
                    .client
                    .request(
                        "objects.query",
                        Json::object([
                            ("contractKey", "task-board/attachment".into()),
                            ("limit", Json::Number(2.0)),
                            (
                                "where",
                                Json::object([
                                    ("op", "and".into()),
                                    (
                                        "args",
                                        Json::Array(vec![
                                            Json::object([
                                                ("op", "eq".into()),
                                                ("field", "object.parentId".into()),
                                                ("value", Json::String(task_id.clone())),
                                            ]),
                                            Json::object([
                                                ("op", "eq".into()),
                                                ("field", "object.name".into()),
                                                ("value", Json::String(name)),
                                            ]),
                                        ]),
                                    ),
                                ]),
                            ),
                        ]),
                        RequestOptions::default(),
                    )
                    .await?;
                let items = found
                    .required("items")?
                    .as_array()
                    .ok_or_else(IvyError::internal)?;
                require(
                    items.len() == 1 && found.get("nextCursor") == Some(&Json::Null),
                    "task_board_evidence_mismatch",
                    "The original attachment identity is unavailable.",
                )?;
                let existing = self
                    .client
                    .request(
                        "objects.read",
                        Json::object([("objectId", items[0].required("objectId")?.clone())]),
                        RequestOptions::default(),
                    )
                    .await?;
                let task = Json::String(task_id.clone());
                // Check ownership and metadata before reading attachment bytes.
                require(
                    existing.required("object")?.get("parentId") == Some(&task)
                        && existing.required("object")?.get("ownerObjectId") == Some(&task)
                        && existing.required("object")?.get("effectivelyArchived")
                            == Some(&Json::Bool(false))
                        && existing.required("object")?.get("currentRevision")
                            == Some(&Json::Number(1.0))
                        && existing.required("revision")?.get("revision")
                            == Some(&Json::Number(1.0))
                        && existing.required("revision")?.get("contractVersion")
                            == Some(&Json::from(version))
                        && existing.required("revision")?.get("byteLength")
                            == Some(&Json::Number(bytes.len() as f64))
                        && existing.required("revision")?.get("contentHash")
                            == Some(&Json::from(digest(bytes)))
                        && existing.required("content")?.get("encoding")
                            == Some(&Json::from("base64"))
                        && attachment_content_matches(
                            existing.required("content")?.required("value")?,
                            bytes,
                        )?,
                    "task_board_evidence_mismatch",
                    "The retained attachment must match its exact original bytes and owner.",
                )?;
                pin(&existing)
            }
            Err(error) => Err(error),
        }
    }

    /// Local keys stay on SQLite, including when its query fails. The owner
    /// supplies both locale ordering and the exact readable remote versions.
    pub async fn page(
        &self,
        key: &str,
        options: &Json,
        readable_versions: &[JsString],
        locale: &impl crate::category_projection::CategoryLocale,
    ) -> IvyResult<Json> {
        if self.contracts.local(key)? {
            self.journal.page(key, options, locale)
        } else {
            self.remote_page(key, options, readable_versions).await
        }
    }

    /// The owner supplies the exact admitted versions from its contract bundle.
    /// An explicit null version filter queries all stored versions; an absent
    /// filter uses that bundle's readable versions, and an array stays exact.
    /// Local query policy has its own text/property bindings and is not routed
    /// through Hive as a fallback when those bindings are unavailable.
    pub async fn remote_page(
        &self,
        key: &str,
        options: &Json,
        readable_versions: &[JsString],
    ) -> IvyResult<Json> {
        let root = match &self.journal.root_object_id {
            Some(root) => Json::object([
                ("op", "eq".into()),
                ("field", "object.parentId".into()),
                ("value", Json::String(root.clone())),
            ]),
            None => Json::object([("op", "isNull".into()), ("field", "object.parentId".into())]),
        };
        let provided = |field| options.get(field).filter(|value| !value.is_null());
        let mut params = Json::object([
            ("contractKey", key.into()),
            (
                "where",
                if let Some(predicate) = provided("where") {
                    Json::object([
                        ("op", "and".into()),
                        ("args", Json::Array(vec![root, predicate.clone()])),
                    ])
                } else {
                    root
                },
            ),
            (
                "orderBy",
                provided("orderBy").cloned().unwrap_or_else(|| {
                    Json::Array(vec![Json::object([
                        ("field", "object.id".into()),
                        ("direction", "asc".into()),
                    ])])
                }),
            ),
            (
                "limit",
                provided("limit").cloned().unwrap_or(Json::Number(50.0)),
            ),
        ]);
        match options.get("contractVersions") {
            Some(Json::Null) => {}
            versions => {
                params.insert(
                    "contractVersions",
                    versions.cloned().unwrap_or_else(|| {
                        Json::Array(
                            readable_versions
                                .iter()
                                .cloned()
                                .map(Json::String)
                                .collect(),
                        )
                    }),
                );
            }
        }
        if let Some(select) = provided("select") {
            params.insert("select", select.clone());
        }
        if let Some(cursor) = provided("cursor")
            .filter(|cursor| cursor.as_js_string().is_some_and(|s| !s.0.is_empty()))
        {
            params.insert("cursor", cursor.clone());
        }
        self.client
            .request("objects.query", params, RequestOptions::default())
            .await
    }

    async fn record(
        &self,
        key: &str,
        address: &Json,
        primary: bool,
        include_archived: bool,
    ) -> IvyResult<ReadRecord> {
        let id = address
            .as_js_string()
            .or_else(|| address.get("objectId").and_then(Json::as_js_string))
            .ok_or_else(IvyError::internal)?;
        if self.contracts.local(key)? || self.journal.key(id)?.as_deref() == Some(key) {
            let document = self.journal.read(key, address, primary)?;
            self.contracts.validate_record(
                key,
                &self.contracts.version(key)?.into(),
                &document.value,
            )?;
            return Ok(ReadRecord {
                document,
                archived: false,
                metadata: None,
            });
        }
        let cache_key = address.as_js_string().is_none().then(|| {
            Json::Array(vec![
                key.into(),
                address.clone(),
                primary.into(),
                include_archived.into(),
            ])
        });
        let cache = self.cache.and_then(ReadCache::entries);
        if let (Some(cache), Some(cache_key)) = (&cache, &cache_key) {
            if let Some(document) = cache
                .borrow()
                .iter()
                .find(|(key, _)| key == cache_key)
                .and_then(|(_, entry)| entry.document.as_ref())
            {
                return Ok(document.clone());
            }
        }
        let params = if address.as_js_string().is_some() {
            Json::object([("objectId", address.clone())])
        } else {
            Json::object([
                ("objectId", address.required("objectId")?.clone()),
                ("revision", address.required("revision")?.clone()),
            ])
        };
        let result = self
            .client
            .request("objects.read", params, RequestOptions::default())
            .await?;
        let object = result.required("object")?;
        let revision = result.required("revision")?;
        let content = result.required("content")?;
        require(
            object.get("contractKey") == Some(&Json::from(key))
                && self
                    .contracts
                    .readable_version(key, string(revision, "contractVersion")?)?
                && revision.get("mediaType") == Some(&Json::from("application/json"))
                && content.get("encoding") == Some(&Json::from("json")),
            "task_board_contract_mismatch",
            "The selected Object is not an exact supported TaskBoard record.",
        )?;
        require(
            !primary
                || object.get("parentId") == Some(&nullable(self.journal.root_object_id.as_ref())),
            "task_board_scope_mismatch",
            "Prepared payloads and records from another workflow root are not primary domain Objects.",
        )?;
        let archived = object
            .required("effectivelyArchived")?
            .as_bool()
            .ok_or_else(IvyError::internal)?;
        require(
            !archived || include_archived,
            "task_board_record_archived",
            "An archived workflow record is not writable workflow state.",
        )?;
        require(
            address.as_js_string().is_some() || revision.get("revision") == address.get("revision"),
            "task_board_evidence_mismatch",
            "The selected immutable revision differs from its pin.",
        )?;
        let value = content.required("value")?;
        self.contracts
            .validate_record(key, string(revision, "contractVersion")?, value)?;
        let record = ReadRecord {
            document: Document {
                pin: pin(&result)?,
                value: value.clone(),
                parent_id: object.required("parentId")?.as_js_string().cloned(),
                content_hash: revision.required("contentHash")?.as_str()?,
            },
            archived,
            metadata: Some(object.clone()),
        };
        if let (Some(cache), Some(cache_key)) = (cache, cache_key) {
            let mut entries = cache.borrow_mut();
            if revision
                .required("byteLength")?
                .as_f64()
                .ok_or_else(IvyError::internal)?
                <= 65536.0
                && entries.len() < 256
            {
                let cached = CachedRecord {
                    object_id: record.document.pin.required("objectId")?.clone(),
                    document: Some(record.clone()),
                    materialized: None,
                };
                if let Some((_, existing)) = entries.iter_mut().find(|(key, _)| key == &cache_key) {
                    *existing = cached;
                } else {
                    entries.push((cache_key, cached));
                }
            }
        }
        Ok(record)
    }

    /// Here `None` means the explicit null parent. Store::named supplies the root
    /// when its optional parent argument is omitted.
    pub async fn named_record(
        &self,
        key: &str,
        name: &JsString,
        parent: Option<&JsString>,
    ) -> IvyResult<Option<Document>> {
        if self.contracts.local(key)?
            || parent
                .map(|id| self.journal.key(id))
                .transpose()?
                .flatten()
                .as_deref()
                == Some("task-board/operation")
        {
            let found = self.journal.named(key, name, parent)?;
            if let Some(document) = &found {
                self.contracts.validate_record(
                    key,
                    &self.contracts.version(key)?.into(),
                    &document.value,
                )?;
            }
            return Ok(found);
        }
        // Find identity across versions before checking the selected record.
        let parent_predicate = match parent {
            Some(parent) => Json::object([
                ("op", "eq".into()),
                ("field", "object.parentId".into()),
                ("value", Json::String(parent.clone())),
            ]),
            None => Json::object([("op", "isNull".into()), ("field", "object.parentId".into())]),
        };
        let page = self
            .client
            .request(
                "objects.query",
                Json::object([
                    ("contractKey", key.into()),
                    (
                        "where",
                        Json::object([
                            ("op", "and".into()),
                            (
                                "args",
                                Json::Array(vec![
                                    parent_predicate,
                                    Json::object([
                                        ("op", "eq".into()),
                                        ("field", "object.name".into()),
                                        ("value", Json::String(name.clone())),
                                    ]),
                                ]),
                            ),
                        ]),
                    ),
                    ("limit", Json::Number(2.0)),
                ]),
                RequestOptions::default(),
            )
            .await?;
        let items = page
            .required("items")?
            .as_array()
            .ok_or_else(IvyError::internal)?;
        require(
            items.len() <= 1 && page.get("nextCursor").is_some_and(Json::is_null),
            "task_board_identity_conflict",
            "A durable workflow identity must select exactly one record.",
        )?;
        match items.first() {
            Some(item) => Ok(Some(
                self.record(
                    key,
                    item.required("objectId")?,
                    parent == self.journal.root_object_id.as_ref(),
                    false,
                )
                .await?
                .document,
            )),
            None => Ok(None),
        }
    }

    fn references(&self, key: &str, value: &Json) -> IvyResult<Json> {
        let mut references = Json::object(Vec::<(String, Json)>::new());
        match key {
            "task-board/task" => {
                for (field, name) in [("latestResult", "result"), ("lastRun", "run")] {
                    if let Some(pin) = value.get(field).filter(|v| !v.is_null()) {
                        references.insert(name, pin.clone());
                    }
                }
                let mut seen = HashSet::new();
                let mut attachments = vec![];
                for attachment in value
                    .required("attachments")?
                    .as_array()
                    .ok_or_else(IvyError::internal)?
                    .iter()
                    .chain(
                        value
                            .required("comments")?
                            .as_array()
                            .ok_or_else(IvyError::internal)?
                            .iter()
                            .map(|comment| {
                                comment
                                    .required("attachments")
                                    .and_then(|v| v.as_array().ok_or_else(IvyError::internal))
                            })
                            .collect::<IvyResult<Vec<_>>>()?
                            .into_iter()
                            .flatten(),
                    )
                {
                    let pin = attachment.required("object")?;
                    if seen.insert(hash_json(pin)?) {
                        attachments.push(pin);
                    }
                }
                require(
                    attachments.len() + references.as_object().unwrap().len() <= 256,
                    "limit_exceeded",
                    "A Task can retain at most 256 exact Run, Result and attachment references.",
                )?;
                for (index, pin) in attachments.into_iter().enumerate() {
                    references.insert(format!("attachment-{index}"), pin.clone());
                }
            }
            "task-board/delivery" => {
                if let Some(request) = value.get("chatRequest").filter(|v| !v.is_null()) {
                    references.insert("source", request.required("source")?.clone());
                }
            }
            "task-board/result" => {
                let content = value.required("content")?;
                let mut artifacts = content
                    .required("artifacts")?
                    .as_array()
                    .ok_or_else(IvyError::internal)?
                    .iter()
                    .collect::<Vec<_>>();
                // Current results contain artifacts directly; older stored
                // results may also pin evidence in their retained findings.
                if let Some(checks) = content.get("checks") {
                    for check in checks.as_array().ok_or_else(IvyError::internal)? {
                        artifacts.extend(
                            check
                                .required("evidence")?
                                .as_array()
                                .ok_or_else(IvyError::internal)?,
                        );
                    }
                }
                let mut selected = vec![];
                for artifact in artifacts {
                    let pin = artifact.required("object")?;
                    if self.journal.key(string(pin, "objectId")?)?.is_none() {
                        selected.push(pin);
                    }
                }
                for (index, pin) in selected.into_iter().take(256).enumerate() {
                    references.insert(format!("artifact-{index}"), pin.clone());
                }
            }
            _ => {}
        }
        Ok(references)
    }

    async fn write_identity(
        &self,
        key: &str,
        value: &Json,
        mutation: &JsString,
        destination: &Json,
        retained: bool,
    ) -> IvyResult<Json> {
        let version = self.contracts.version(key)?;
        self.contracts
            .validate_record(key, &version.clone().into(), value)?;
        let create = destination.get("create");
        let parent = create
            .and_then(|v| v.get("parentId"))
            .and_then(Json::as_js_string);
        if self.contracts.local(key)?
            || (create.is_some()
                && parent
                    .map(|id| self.journal.key(id))
                    .transpose()?
                    .flatten()
                    .as_deref()
                    == Some("task-board/operation"))
        {
            canonical(value, MANAGEMENT_FRAME_BYTES)?;
            let now = (self.now)().into();
            // Reconciliation collects history. Workflow writes, especially gate
            // releases, must not wait for cleanup's remote receipt checks.
            return if let Some(create) = create {
                self.journal.create_with_id(
                    key,
                    string(create, "name")?,
                    parent,
                    value,
                    &now,
                    &self.new_id,
                )
            } else {
                self.journal.update(
                    key,
                    string(destination, "objectId")?,
                    destination
                        .required("expectedRevision")?
                        .as_f64()
                        .ok_or_else(IvyError::internal)?,
                    value,
                    &now,
                )
            };
        }
        let references = self.references(key, value)?;
        let identity = if retained {
            mutation.clone()
        } else {
            self.prepared_mutation_id(&mutation.to_utf8()?).await?
        };
        let mut params = Json::object([
            ("mutationId", Json::String(identity)),
            ("contractVersion", version.into()),
            (
                "content",
                Json::object([("encoding", "json".into()), ("value", value.clone())]),
            ),
            ("references", references),
        ]);
        if let Some(create) = create {
            let mut target = create.clone();
            target.insert("ownerObjectId", Json::Null);
            target.insert("contractKey", key.into());
            params.insert("create", target);
        } else {
            params.insert("objectId", destination.required("objectId")?.clone());
            params.insert(
                "expectedRevision",
                destination.required("expectedRevision")?.clone(),
            );
            if key == "task-board/task" {
                params.insert("expectedArchived", false.into());
            }
        }
        let result = match self
            .client
            .request("objects.write", params, RequestOptions::default())
            .await
        {
            Ok(result) => result,
            Err(failure) if create.is_some() && failure.code == "revision_conflict" => {
                let existing = self
                    .named_record(key, string(create.unwrap(), "name")?, parent)
                    .await?;
                require(
                    existing
                        .as_ref()
                        .is_some_and(|d| d.pin.get("revision") == Some(&Json::Number(1.0)))
                        && existing.as_ref().map(|d| hash_json(&d.value)).transpose()?
                            == Some(hash_json(value)?),
                    "revision_conflict",
                    "The durable TaskBoard identity already contains different content.",
                )?;
                return Ok(existing.unwrap().pin);
            }
            Err(failure) => return Err(failure),
        };
        if let Some(cache) = self.cache.and_then(ReadCache::entries) {
            let id = result.required("object")?.required("id")?;
            let mut entries = cache.borrow_mut();
            entries.retain(|(_, record)| &record.object_id != id);
            if canonical(value, usize::MAX)?.len() <= 65536 && entries.len() < 256 {
                let object = result.required("object")?;
                // The original write key omits includeArchived. It consumes a
                // bounded slot but pinned reads use a different key; preserve it.
                let primary =
                    object.get("parentId") == Some(&nullable(self.journal.root_object_id.as_ref()));
                entries.push((
                    Json::Array(vec![key.into(), pin(&result)?, primary.into()]),
                    CachedRecord {
                        object_id: id.clone(),
                        document: None,
                        materialized: None,
                    },
                ));
            }
        }
        pin(&result)
    }
}

impl<R: RpcClient + 'static, P: Contracts, C: Fn() -> String, I: Fn() -> JsString> Store
    for JournalStore<'_, R, P, C, I>
{
    fn root_object_id(&self) -> Option<JsString> {
        self.journal.root_object_id.clone()
    }
    fn contract_version(&self, key: &str) -> IvyResult<String> {
        self.contracts.version(key)
    }
    fn retired_operation(&self, caller: &JsString, id: &JsString) -> IvyResult<bool> {
        Ok(LocalGates::new(self.journal.connection)
            .retired_operation(caller, id)
            .map_err(error)?
            .is_some())
    }
    fn retired_operation_receipt(
        &self,
        caller: &JsString,
        id: &JsString,
    ) -> IvyResult<Option<Json>> {
        let caller = Json::String(caller.clone());
        let id = Json::String(id.clone());
        persistent_reads::local_reads::SqliteReads {
            journal: LocalJournal {
                connection: self.journal.connection,
                root_object_id: self.journal.root_object_id.clone(),
            },
            contracts: &self.contracts,
            parse: |text: &str| parse(text, MANAGEMENT_FRAME_BYTES),
            sqlite_failure: error,
        }
        .retired_operation_receipt(Some(&caller), Some(&id))
    }
    fn local_key(&self, id: &JsString) -> IvyResult<Option<String>> {
        self.journal.key(id)
    }
    fn complete_local_workflow(&self, id: &JsString, at: &JsString) -> IvyResult<()> {
        self.retention().complete_workflow(id, at, None)
    }
    async fn named(
        &self,
        key: &str,
        name: &str,
        parent: Option<&JsString>,
    ) -> IvyResult<Option<Document>> {
        self.named_record(
            key,
            &name.into(),
            parent.or(self.journal.root_object_id.as_ref()),
        )
        .await
    }
    async fn read(&self, key: &str, address: &Json, primary: bool) -> IvyResult<Document> {
        Ok(self.record(key, address, primary, false).await?.document)
    }
    async fn write(
        &self,
        key: &str,
        value: &Json,
        mutation: &str,
        destination: &Json,
    ) -> IvyResult<Json> {
        self.write_identity(key, value, &mutation.into(), destination, false)
            .await
    }
    async fn prepared_mutation_id(&self, mutation: &str) -> IvyResult<JsString> {
        scoped_operation_id(
            self.client.clone(),
            &Json::Array(vec!["task-board".into(), mutation.into()]),
        )
        .await
    }
    async fn write_prepared(
        &self,
        key: &str,
        value: &Json,
        mutation: &JsString,
        destination: &Json,
    ) -> IvyResult<Json> {
        self.write_identity(key, value, mutation, destination, true)
            .await
    }
}

impl<R, P, C, I> BlobStore for JournalStore<'_, R, P, C, I> {
    type Pin = Json;
    type Error = IvyError;
    fn save(&self, key: ChunkKey, name: &str, parent: &JsString, bytes: &[u8]) -> IvyResult<Json> {
        let pin = LocalBlobs::new(self.journal.connection)
            .save_blob(&key.as_str().into(), &name.into(), parent, bytes)
            .map_err(blob_error)?;
        Ok(Json::object([
            ("objectId", Json::String(pin.object_id)),
            ("revision", Json::Number(pin.revision as f64)),
        ]))
    }
    fn read(&self, key: ChunkKey, pin: &Json, parent: &JsString) -> IvyResult<Vec<u8>> {
        require(
            pin.get("revision") == Some(&Json::Number(1.0)),
            "task_board_evidence_mismatch",
            "Local evidence blobs have one revision.",
        )?;
        LocalBlobs::new(self.journal.connection)
            .read_blob(
                &key.as_str().into(),
                &BlobPin {
                    object_id: string(pin, "objectId")?.clone(),
                    revision: 1,
                },
                parent,
            )
            .map_err(blob_error)
    }
}
impl<R: RpcClient + 'static, P: Contracts, C: Fn() -> String, I: Fn() -> JsString> ReadEvidenceStore
    for JournalStore<'_, R, P, C, I>
{
    fn validate(&self, definition: &str, value: &Json) -> IvyResult<()> {
        self.contracts.validate_definition(definition, value)
    }
    async fn read_document(&self, key: &str, address: &Json, primary: bool) -> IvyResult<Document> {
        Store::read(self, key, address, primary).await
    }
    async fn write_document(
        &self,
        key: &str,
        value: &Json,
        mutation: &str,
        destination: &Json,
    ) -> IvyResult<Json> {
        self.write(key, value, mutation, destination).await
    }
}
impl<R: RpcClient + 'static, P: Contracts, C: Fn() -> String, I: Fn() -> JsString> WorkspaceOwner
    for JournalStore<'_, R, P, C, I>
{
    async fn read(
        &self,
        key: &str,
        address: &Json,
        primary: bool,
        include_archived: bool,
    ) -> IvyResult<WorkspaceDocument> {
        let record = self.record(key, address, primary, include_archived).await?;
        Ok(WorkspaceDocument {
            pin: record.document.pin,
            value: record.document.value,
            effectively_archived: record.archived,
        })
    }
    async fn stat(&self, id: &JsString) -> IvyResult<Json> {
        self.client
            .request(
                "objects.stat",
                Json::object([("objectId", Json::String(id.clone()))]),
                RequestOptions::default(),
            )
            .await
    }
}
