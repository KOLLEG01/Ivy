//! Local workflow persistence and Hive publication on the same store interface.
//! The owner supplies exact contract admission and one action's read cache. This
//! layer neither upgrades records nor provides a permissive schema fallback.
use crate::{
    evidence_bytes::{BlobStore, ChunkKey},
    local_blobs::{BlobError, BlobPin, LocalBlobs},
    local_gates::LocalGates,
    local_journal::{LocalJournal, require},
    local_retention::HistoryLimits,
    local_workspaces::{WorkspaceDocument, WorkspaceOwner},
    operations::{Document, PREPARED_PAYLOAD, PreparedPayload, Store, StoredRecord, TaskChange},
    read_evidence::ReadEvidenceStore,
    work_budget::{Reservation, WorkBudget},
};
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString,
    hash::hash_json,
    json::{canonical_size, parse},
    limits::MANAGEMENT_FRAME_BYTES,
};
use ivy_host_runtime::base64_content::Base64Content;
use ivy_sdk::{
    operation_ids::scoped_operation_id,
    rpc::{RequestOptions, RpcClient},
};
use ivy_store::sqlite::error;
use std::{
    cell::{OnceCell, RefCell},
    collections::HashSet,
    future::Future,
    pin::Pin,
    rc::Rc,
    sync::Arc,
    task::{Context, Poll},
};

#[path = "comment_index.rs"]
pub(crate) mod comment_index;
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
#[path = "task_changes.rs"]
mod task_changes;
#[path = "task_publication.rs"]
mod task_publication;
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

struct ReadRecord {
    document: Document,
    archived: bool,
    metadata: Option<Json>,
    references: Option<Json>,
}

impl ReadRecord {
    fn retained_bytes(&self) -> usize {
        self.document.retained_bytes()
            + self.metadata.as_ref().map_or(0, Json::retained_bytes)
            + self.references.as_ref().map_or(0, Json::retained_bytes)
    }
}

enum CachedValue {
    Workflow(ReadRecord),
    Materialized(StoredRecord),
}

struct CachedRecord {
    value: CachedValue,
    retained_bytes: usize,
    _reservation: Arc<Reservation>,
}
impl CachedRecord {
    fn object_id(&self) -> &JsString {
        match &self.value {
            CachedValue::Workflow(record) => &record.document.pin.object_id,
            CachedValue::Materialized(record) => &record.pin.object_id,
        }
    }
}
fn remember_record(
    entries: &mut CacheRecords,
    key: Json,
    value_bytes: usize,
    take: impl FnOnce() -> CachedValue,
) -> Option<&CachedValue> {
    let bytes = std::mem::size_of::<CachedRecord>() + key.retained_bytes() + value_bytes;
    if bytes > 64 * 1024 || entries.len() >= 256 {
        return None;
    }
    let retained: usize = entries
        .iter()
        .filter(|(saved, _)| saved != &key)
        .map(|(_, record)| record.retained_bytes)
        .sum();
    if retained.saturating_add(bytes) > 4 * 1024 * 1024 {
        return None;
    }
    let Ok(reservation) = entries.budget.cache(bytes) else {
        return None;
    };
    if !entries.iter().any(|(saved, _)| saved == &key) && !entries.grow() {
        return None;
    }
    // Admission precedes taking the original record. Callers project only the
    // fields they need from the admitted record, or consume it on a cache miss.
    let record = CachedRecord {
        value: take(),
        retained_bytes: bytes,
        _reservation: reservation,
    };
    let index = if let Some(index) = entries.iter().position(|(saved, _)| saved == &key) {
        entries[index].1 = record;
        index
    } else {
        entries.push((key, record));
        entries.len() - 1
    };
    Some(&entries[index].1.value)
}
struct CacheRecords {
    values: Vec<(Json, CachedRecord)>,
    budget: WorkBudget,
    allocation: Arc<Reservation>,
}
impl CacheRecords {
    fn new(budget: WorkBudget) -> Self {
        Self {
            values: Vec::new(),
            allocation: budget.cache(0).expect("An empty cache has no byte charge."),
            budget,
        }
    }
    fn grow(&mut self) -> bool {
        if self.values.len() < self.values.capacity() {
            return true;
        }
        let old = self.allocation.bytes();
        let capacity = (self.values.capacity().saturating_mul(2)).clamp(4, 256);
        let slot = std::mem::size_of::<(Json, CachedRecord)>();
        if self.allocation.resize(capacity * slot).is_err() {
            return false;
        }
        if self
            .values
            .try_reserve_exact(capacity - self.values.len())
            .is_err()
        {
            self.allocation
                .resize(old)
                .expect("Releasing a cache charge cannot exceed its budget.");
            return false;
        }
        // Include unused Vec slots after allocator rounding. A cache is only
        // an optimization: loss of capacity never rejects the original read.
        if self
            .allocation
            .resize(self.values.capacity() * slot)
            .is_err()
        {
            self.values = Vec::new();
            self.allocation
                .resize(0)
                .expect("Releasing an empty cache cannot exceed its budget.");
            return false;
        }
        true
    }
}
impl Default for CacheRecords {
    fn default() -> Self {
        Self::new(WorkBudget::default())
    }
}
impl std::ops::Deref for CacheRecords {
    type Target = Vec<(Json, CachedRecord)>;
    fn deref(&self) -> &Self::Target {
        &self.values
    }
}
impl std::ops::DerefMut for CacheRecords {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.values
    }
}
type CacheEntries = Rc<RefCell<CacheRecords>>;
enum CacheBinding {
    Fixed(CacheEntries),
    Scoped(RefCell<Option<CacheEntries>>),
}
/// An explicit cache for one action. A service owner uses ReadScopes::cache
/// instead, so independent asynchronous actions cannot share cached pins.
pub struct ReadCache(
    CacheBinding,
    OnceCell<
        persistent_reads::task_comments::TaskComments<ivy_sdk::task_comments::RpcCommentSource>,
    >,
    persistent_reads::local_history::HistoryCollections,
    WorkBudget,
);
impl Default for ReadCache {
    fn default() -> Self {
        let budget = WorkBudget::default();
        Self(
            CacheBinding::Fixed(Rc::new(RefCell::new(CacheRecords::new(budget.clone())))),
            OnceCell::new(),
            persistent_reads::local_history::HistoryCollections::default(),
            budget,
        )
    }
}
impl ReadCache {
    pub(crate) fn history_collections(
        &self,
    ) -> &persistent_reads::local_history::HistoryCollections {
        &self.2
    }
    pub(crate) fn comments(
        &self,
        client: Arc<dyn RpcClient>,
    ) -> &persistent_reads::task_comments::TaskComments<ivy_sdk::task_comments::RpcCommentSource>
    {
        self.1.get_or_init(|| {
            persistent_reads::task_comments::TaskComments::new(
                Arc::new(ivy_sdk::task_comments::RpcCommentSource::new(client)),
                tokio::runtime::Handle::current(),
            )
        })
    }
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
        Self::with_budget(WorkBudget::default())
    }
}
impl ReadScopes {
    pub fn with_budget(budget: WorkBudget) -> Self {
        Self {
            cache: ReadCache(
                CacheBinding::Scoped(RefCell::default()),
                OnceCell::new(),
                persistent_reads::local_history::HistoryCollections::default(),
                budget,
            ),
        }
    }
    pub fn cache(&self) -> &ReadCache {
        &self.cache
    }
    /// Nested actions inherit the current map; independent actions get a new one.
    pub fn with_read_cache<F: Future>(&self, work: F) -> ReadScopeFuture<'_, F> {
        ReadScopeFuture {
            scopes: self,
            entries: Some(
                self.cache.entries().unwrap_or_else(|| {
                    Rc::new(RefCell::new(CacheRecords::new(self.cache.3.clone())))
                }),
            ),
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

/// Retained workflow and remote publication share the current generation owner.
/// A transaction never spans a remote await; collection rechecks its graph.
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

fn attachment_content_matches(value: &Json, content: &Base64Content<'_>) -> IvyResult<bool> {
    let value = value.as_js_string().ok_or_else(IvyError::internal)?;
    // Canonical base64 has one representation. Exact equality also checks every byte,
    // after the existing owner/revision/hash guards, without decoding another body.
    Ok(value == content.encoded())
}

impl<R: RpcClient + 'static, P: Contracts, C: Fn() -> String, I: Fn() -> JsString>
    JournalStore<'_, R, P, C, I>
{
    pub async fn collect_local(&self) -> IvyResult<()>
    where
        R: ivy_sdk::rpc::OptionalRpcClient,
    {
        use crate::engine_read::read_failure::{ReadFailure, provider_read_error};
        use persistent_reads::{
            local_history::{HistoryCollections, LocalHistory},
            local_reads::SqliteReads,
            store::RpcObjectReads,
        };
        let local = SqliteReads {
            journal: LocalJournal {
                connection: self.journal.connection,
                root_object_id: self.journal.root_object_id.clone(),
            },
            contracts: &self.contracts,
            parse: |raw: &str| parse(raw, MANAGEMENT_FRAME_BYTES).map_err(ReadFailure::from),
            sqlite_failure: |failure| ReadFailure::from(error(failure)),
        };
        let objects = RpcObjectReads {
            client: self.client.as_ref(),
        };
        let fallback = HistoryCollections::default();
        let collections = self.cache.map_or(&fallback, ReadCache::history_collections);
        LocalHistory::new(
            &local,
            self,
            &objects,
            collections,
            HistoryLimits {
                count: self.limits.count,
                bytes: self.limits.bytes,
            },
        )
        .collect::<std::convert::Infallible>(0, 0, None, None)
        .await
        .map_err(provider_read_error)
    }

    pub async fn read_record(
        &self,
        key: &str,
        address: &Json,
        primary: bool,
        include_archived: bool,
    ) -> IvyResult<Document> {
        self.record_with(
            key,
            address,
            primary,
            include_archived,
            |record| Ok(record.document),
            |record| Ok(record.document.clone()),
        )
        .await
    }

    pub(crate) async fn materialize_document(
        &self,
        key: &str,
        mut document: Document,
    ) -> IvyResult<Document> {
        let mut value = document.value;
        if key == "task-board/task" {
            let mut owner = Json::String(document.pin.object_id.clone());
            if value.get("commentStore").is_some_and(|v| !v.is_null())
                && let Some(parent) = document.parent_id.as_ref()
                && Some(parent) != self.journal.root_object_id.as_ref()
                && self.journal.key(parent)?.as_deref() == Some("task-board/operation")
            {
                let operation = self.journal.read(
                    "task-board/operation",
                    &Json::String(parent.clone()),
                    true,
                )?;
                owner = persistent_reads::local_reads::prepared_task_owner(
                    &operation.value,
                    &document.pin.to_json(),
                )?
                .clone()
                .into();
            }
            let owner = owner.as_js_string().ok_or_else(IvyError::internal)?;
            value = if value
                .get("commentStore")
                .and_then(|tree| tree.get("kind"))
                .is_some()
            {
                let uncached = ReadCache::default();
                let cache = self.cache.unwrap_or(&uncached);
                let comments = cache.comments(self.client.clone());
                let full = comments.hydrate_owned(value, owner).await;
                if self.cache.is_none() {
                    comments.settled().await;
                }
                full.map_err(|failure| match failure {
                    persistent_reads::task_comments::CommentFailure::Source(error)
                    | persistent_reads::task_comments::CommentFailure::Ivy(error) => error,
                })?
                .ok_or_else(IvyError::internal)?
            } else {
                persistent_reads::read_legacy_conversation_owned(self.client.as_ref(), value, owner)
                    .await?
                    .ok_or_else(IvyError::internal)?
            };
        }
        document.value = persistent_reads::record_upgrades::upgrade_stored_owned(
            self.client.as_ref(),
            key,
            value,
        )
        .await?;
        Ok(document)
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
        let (document, metadata) = self
            .record_with(
                key,
                address,
                primary,
                include_archived,
                |record| Ok((record.document, record.metadata)),
                |record| Ok((record.document.clone(), record.metadata.clone())),
            )
            .await?;
        let metadata = match metadata {
            Some(metadata) => metadata,
            None => self
                .journal
                .metadata(&document.pin.object_id, &self.contracts.version(key)?)?,
        };
        Ok((document, metadata))
    }

    pub async fn write_attachment(
        &self,
        task_id: &JsString,
        operation_id: &JsString,
        attachment_id: &JsString,
        content: &Base64Content<'_>,
    ) -> IvyResult<Json> {
        require(
            content.byte_length() <= 8 * 1024 * 1024,
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
                            ("value", Json::String(content.encoded().clone())),
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
                            == Some(&Json::Number(content.byte_length() as f64))
                        && existing.required("revision")?.get("contentHash")
                            == Some(&Json::from(content.content_hash()))
                        && existing.required("content")?.get("encoding")
                            == Some(&Json::from("base64"))
                        && attachment_content_matches(
                            existing.required("content")?.required("value")?,
                            content,
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
    /// supplies the exact readable remote versions.
    pub async fn page(
        &self,
        key: &str,
        options: &Json,
        readable_versions: &[JsString],
    ) -> IvyResult<Json> {
        if self.contracts.local(key)? {
            self.journal.page(key, options)
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
        for field in ["includeArchived", "includeContent", "observe"] {
            if let Some(value) = provided(field) {
                params.insert(field, value.clone());
            }
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

    fn validate_local_record(&self, key: &str, document: &Document) -> IvyResult<String> {
        let mut version = self.contracts.version(key)?;
        if let Some(parent) = document.parent_id.as_ref()
            && Some(parent) != self.journal.root_object_id.as_ref()
            && self.journal.key(parent)?.as_deref() == Some("task-board/operation")
        {
            let operation =
                self.journal
                    .read("task-board/operation", &Json::String(parent.clone()), true)?;
            if let Some(captured) = persistent_reads::local_reads::prepared_version(
                key,
                &document.pin.to_json(),
                &document.content_hash,
                &operation.value,
            )? {
                if key == PREPARED_PAYLOAD
                    || key == "task-board/task"
                        && document
                            .value
                            .get("commentStore")
                            .is_some_and(|tree| !tree.is_null())
                {
                    persistent_reads::local_reads::prepared_task_owner(
                        &operation.value,
                        &document.pin.to_json(),
                    )?;
                }
                version = captured;
            }
        }
        self.contracts
            .validate_record(key, &version.clone().into(), &document.value)?;
        Ok(version)
    }

    async fn record_with<T>(
        &self,
        key: &str,
        address: &Json,
        primary: bool,
        include_archived: bool,
        take: impl FnOnce(ReadRecord) -> IvyResult<T>,
        copy: impl FnOnce(&ReadRecord) -> IvyResult<T>,
    ) -> IvyResult<T> {
        let id = address
            .as_js_string()
            .or_else(|| address.get("objectId").and_then(Json::as_js_string))
            .ok_or_else(IvyError::internal)?;
        let local = self.contracts.local(key)?;
        let local_key = if local { None } else { self.journal.key(id)? };
        let stored_key =
            if key == "task-board/task" && local_key.as_deref() == Some(PREPARED_PAYLOAD) {
                PREPARED_PAYLOAD
            } else {
                key
            };
        if local || local_key.as_deref() == Some(stored_key) {
            let mut document = self.journal.read(stored_key, address, primary)?;
            let version = self.validate_local_record(stored_key, &document)?;
            let metadata = if version != self.contracts.version(stored_key)? {
                Some(self.journal.metadata(id, &version)?)
            } else {
                None
            };
            if stored_key != key {
                document.value =
                    persistent_reads::local_reads::prepared_task(&self.contracts, document.value)?;
            }
            return take(ReadRecord {
                document,
                archived: false,
                metadata,
                references: None,
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
                .and_then(|(_, entry)| match &entry.value {
                    CachedValue::Workflow(record) => Some(record),
                    CachedValue::Materialized(_) => None,
                })
            {
                return copy(document);
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
        let mut result = self
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
        let document = Document {
            pin: crate::operations::ObjectPin {
                object_id: string(object, "id")?.clone(),
                revision: revision
                    .required("revision")?
                    .as_f64()
                    .ok_or_else(IvyError::internal)?,
            },
            value: Json::Null,
            parent_id: object.required("parentId")?.as_js_string().cloned(),
            content_hash: revision.required("contentHash")?.as_str()?,
        };
        // The transport has relinquished this payload and metadata. Preserve
        // their original buffers after all admission checks have succeeded.
        let value = result.get_mut("content").unwrap().remove("value").unwrap();
        let metadata = result.remove("object").unwrap();
        let references = result.get_mut("revision").unwrap().remove("references");
        let mut record = Some(ReadRecord {
            document: Document { value, ..document },
            archived,
            metadata: Some(metadata),
            references,
        });
        let cacheable = if cache.is_some() && cache_key.is_some() {
            result
                .required("revision")?
                .required("byteLength")?
                .as_f64()
                .ok_or_else(IvyError::internal)?
                <= 65536.0
        } else {
            false
        };
        // Preserve references for publication and release the unused transport
        // fields before copying only the selected output.
        drop(result);
        if let (Some(cache), Some(cache_key)) = (cache, cache_key) {
            let mut entries = cache.borrow_mut();
            if cacheable && entries.len() < 256 {
                if let Some(CachedValue::Workflow(cached)) = remember_record(
                    &mut entries,
                    cache_key,
                    record.as_ref().unwrap().retained_bytes(),
                    || CachedValue::Workflow(record.take().unwrap()),
                ) {
                    return copy(cached);
                }
            }
        }
        take(record.unwrap())
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
                self.validate_local_record(key, document)?;
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
                self.read_record(
                    key,
                    item.required("objectId")?,
                    parent == self.journal.root_object_id.as_ref(),
                    false,
                )
                .await?,
            )),
            None => Ok(None),
        }
    }

    async fn references(&self, key: &str, value: &Json, destination: &Json) -> IvyResult<Json> {
        let mut references = Json::object(Vec::<(String, Json)>::new());
        match key {
            "task-board/task" => {
                for (field, name) in [
                    ("latestResult", "result"),
                    ("lastRun", "run"),
                    ("contextRelease", "context-release"),
                ] {
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
            "task-board/run" => {
                if let Some(pin) = reference_writes::run_source_for_write(
                    &nullable(self.journal.root_object_id.as_ref()),
                    value,
                    destination,
                    |params| {
                        self.client
                            .request("objects.read", params, RequestOptions::default())
                    },
                    |error: &IvyError| {
                        error.code == "revision_pruned"
                            && error.outcome_json() == Json::from("not_executed")
                    },
                )
                .await?
                {
                    references.insert("source-task", pin);
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
        selected_version: Option<&JsString>,
        selected_references: Option<&Json>,
    ) -> IvyResult<Json> {
        let version = match selected_version {
            Some(version) => version.to_utf8()?,
            None => self.contracts.version(key)?,
        };
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
            canonical_size(value, MANAGEMENT_FRAME_BYTES)?;
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
        // Domain reads hydrate the entire conversation. Only its bounded header
        // and immutable nodes belong in Hive, including prepared-write replay.
        let published;
        let (value, references) = if let Some(references) = selected_references {
            (value, references.clone())
        } else if key == "task-board/task"
            && create.is_none()
            && persistent_reads::task_comments::comment_node_version(&version).is_some()
        {
            published = self
                .publish_task_content(
                    value,
                    destination,
                    &version,
                    reference_writes::ReferencePages::Retained,
                )
                .await?;
            (&published.0, published.1.clone())
        } else {
            (value, self.references(key, value, destination).await?)
        };
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
                params.insert(
                    "expectedArchived",
                    destination
                        .get("expectedArchived")
                        .cloned()
                        .unwrap_or(false.into()),
                );
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
                    existing.as_ref().is_some_and(|d| d.pin.revision == 1.0)
                        && existing.as_ref().map(|d| hash_json(&d.value)).transpose()?
                            == Some(hash_json(value)?),
                    "revision_conflict",
                    "The durable TaskBoard identity already contains different content.",
                )?;
                return Ok(existing.unwrap().pin.to_json());
            }
            Err(failure) => return Err(failure),
        };
        if let Some(cache) = self.cache.and_then(ReadCache::entries) {
            let id = result.required("object")?.required("id")?;
            let mut entries = cache.borrow_mut();
            entries.retain(|(_, record)| Some(record.object_id()) != id.as_js_string());
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
        self.journal.complete_workflow(id, at, None)
    }
    fn task_operations_pending(
        &self,
        task: &JsString,
        except: Option<&JsString>,
    ) -> IvyResult<bool> {
        self.journal.task_operations_pending(task, except)
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
        let document = self.read_record(key, address, primary, false).await?;
        self.materialize_document(key, document).await
    }
    async fn read_task_header(&self, address: &Json, primary: bool) -> IvyResult<Document> {
        Ok(self
            .lifecycle_task_header(address, primary, false)
            .await?
            .document)
    }
    async fn read_task_comments(
        &self,
        task: &Json,
        task_id: &JsString,
        ids: &[Json],
    ) -> IvyResult<Vec<Json>> {
        self.selected_comments(task, task_id, ids).await
    }
    async fn has_task_comment(
        &self,
        task: &Json,
        task_id: &JsString,
        id: &JsString,
    ) -> IvyResult<bool> {
        if let Some(tree) = task
            .get("commentStore")
            .filter(|v| v.get("kind").is_some_and(|k| k.is_str("tree")))
        {
            return Ok(self
                .comment_positions(tree, task_id, &[(0, id.clone())])
                .await?[0]
                .is_some());
        }
        Ok(task
            .required("comments")?
            .as_array()
            .ok_or_else(IvyError::internal)?
            .iter()
            .any(|comment| comment.get("commentId").and_then(Json::as_js_string) == Some(id)))
    }
    async fn read_completion_comments(
        &self,
        task: &Json,
        task_id: &JsString,
        run: &JsString,
        inputs: &[Json],
    ) -> IvyResult<crate::operations::CompletionComments> {
        self.completion_comment_projection(task, task_id, run, inputs)
            .await
    }
    async fn read_prepared(&self, key: &str, address: &Json) -> IvyResult<Document> {
        self.read_record(key, address, false, false).await
    }
    async fn prepare_payload(
        &self,
        key: &str,
        value: Json,
        destination: &Json,
    ) -> IvyResult<PreparedPayload> {
        let version = self.contracts.version(key)?;
        if key == "task-board/task"
            && destination.get("create").is_none()
            && !self.contracts.local(key)?
            && persistent_reads::task_comments::comment_node_version(&version).is_some()
        {
            let (content, references) = self
                .publish_task_content(
                    &value,
                    destination,
                    &version,
                    reference_writes::ReferencePages::Immutable,
                )
                .await?;
            Ok(PreparedPayload {
                key: PREPARED_PAYLOAD.into(),
                value: Json::object([
                    ("schemaVersion", Json::Number(1.0)),
                    ("contractKey", key.into()),
                    ("contractVersion", version.clone().into()),
                    ("content", content),
                    ("references", references),
                ]),
                version: Some(version.into()),
            })
        } else {
            Ok(PreparedPayload {
                key: key.into(),
                value,
                version: None,
            })
        }
    }
    async fn prepare_task_change(
        &self,
        value: &Json,
        destination: &Json,
        change: TaskChange,
    ) -> IvyResult<PreparedPayload> {
        let version = self.contracts.version("task-board/task")?;
        let (content, references) = self
            .publish_task_change(value, destination, &version, change)
            .await?;
        Ok(PreparedPayload {
            key: PREPARED_PAYLOAD.into(),
            value: Json::object([
                ("schemaVersion", Json::Number(1.0)),
                ("contractKey", "task-board/task".into()),
                ("contractVersion", version.clone().into()),
                ("content", content),
                ("references", references),
            ]),
            version: Some(version.into()),
        })
    }
    async fn write(
        &self,
        key: &str,
        value: &Json,
        mutation: &str,
        destination: &Json,
    ) -> IvyResult<Json> {
        self.write_identity(key, value, &mutation.into(), destination, false, None, None)
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
        version: Option<&JsString>,
        references: Option<&Json>,
    ) -> IvyResult<Json> {
        self.write_identity(key, value, mutation, destination, true, version, references)
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
    async fn read_task_routing(&self, address: &Json) -> IvyResult<Json> {
        let mut task = self
            .read_record("task-board/task", address, true, true)
            .await?;
        let routes = task
            .value
            .remove("nativeRoutes")
            .unwrap_or_else(|| Json::Array(vec![]));
        Ok(Json::object([("nativeRoutes", routes)]))
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
        self.record_with(
            key,
            address,
            primary,
            include_archived,
            |record| {
                Ok(WorkspaceDocument {
                    pin: record.document.pin.into_json(),
                    value: record.document.value,
                    effectively_archived: record.archived,
                })
            },
            |record| {
                Ok(WorkspaceDocument {
                    pin: record.document.pin.to_json(),
                    value: record.document.value.clone(),
                    effectively_archived: record.archived,
                })
            },
        )
        .await
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

impl<
    R: RpcClient + 'static,
    P: Contracts,
    C: Fn() -> String,
    I: Fn() -> JsString,
    E: From<IvyError>,
> persistent_reads::local_history::HistoryDocuments<E> for JournalStore<'_, R, P, C, I>
{
    async fn history_document(
        &self,
        key: &str,
        address: Option<&Json>,
        primary: bool,
        include_archived: bool,
    ) -> Result<Json, E> {
        let mut document = self
            .read_record(
                key,
                address.ok_or_else(IvyError::internal)?,
                primary,
                include_archived,
            )
            .await?;
        if key != "task-board/task" {
            document = self.materialize_document(key, document).await?;
        }
        Ok(Json::object([
            ("pin", document.pin.into_json()),
            ("value", document.value),
        ]))
    }
}
