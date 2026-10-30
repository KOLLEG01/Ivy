//! Local and remote document reads over stored JSON.
use super::{
    json_values::{self as v, Value},
    legacy_reads::read_legacy_with,
    local_reads::SqliteReads,
    record_upgrades::upgrade_stored_with,
    store::ObjectReads,
};
use crate::{
    engine_read::read_policy::TaskReadStore,
    engine_read::{comment_reads::CommentReadStore, condition_policy::ConditionStore},
    journal_store::{CacheEntries, CachedRecord, Contracts, ReadCache, remember_record},
};
use ivy_contracts::{
    IvyError, IvyJson as Json, JsString, json::canonical, limits::MANAGEMENT_FRAME_BYTES,
};

#[allow(async_fn_in_trait)]
pub(crate) trait TreeReads<E> {
    async fn hydrate(&self, task: &Json, owner: &Json) -> Result<Option<Json>, E>;
    fn summary(&self, task: &Json, tree: &Json, owner: &Json) -> Result<Option<Json>, E>;
    async fn range(
        &self,
        tree: &Json,
        owner: &Json,
        start: usize,
        end: usize,
    ) -> Result<Vec<Json>, E>;
}
pub(crate) struct DocumentReads<'a, R, C, Parse, SqlFailure, M> {
    pub(crate) local: &'a SqliteReads<'a, C, Parse, SqlFailure>,
    pub(crate) objects: &'a R,
    pub(crate) comments: &'a M,
    pub(crate) cache: Option<&'a ReadCache>,
}

/// Decode the remaining mutation ports before native SQLite binding.
pub(super) fn sql_argument(value: &Value) -> Result<Option<Json>, IvyError> {
    Ok(Some(value.clone()))
}
fn cache_key(
    key: &str,
    address: &Json,
    primary: bool,
    archived: bool,
    retained: bool,
) -> Result<Option<Json>, IvyError> {
    if address.as_js_string().is_some() {
        return Ok(None);
    }
    let id = address
        .required("objectId")?
        .as_js_string()
        .ok_or_else(IvyError::internal)?;
    let revision = address
        .required("revision")?
        .as_f64()
        .ok_or_else(IvyError::internal)?;
    let mut text = JsString::from(format!("{key}:"));
    text.0.extend_from_slice(&id.0);
    text.0
        .extend(format!(":{revision}:{primary}:{archived}:{retained}").encode_utf16());
    Ok(Some(Json::String(text)))
}
fn cache_get(cache: &Option<CacheEntries>, key: &Option<Json>) -> Option<Json> {
    let (Some(cache), Some(key)) = (cache, key) else {
        return None;
    };
    cache
        .borrow()
        .iter()
        .find(|(saved, _)| saved == key)
        .and_then(|(_, entry)| entry.materialized.clone())
}
impl<R, C, Parse, SqlFailure, M> DocumentReads<'_, R, C, Parse, SqlFailure, M> {
    async fn task_value<E>(&self, task: &Json, owner: &Json, prepared: bool) -> Result<Json, E>
    where
        R: ObjectReads<E>,
        M: TreeReads<E>,
        E: From<IvyError>,
    {
        let full = if task
            .get("commentStore")
            .and_then(|tree| tree.get("kind"))
            .is_some()
        {
            self.comments.hydrate(task, owner).await?
        } else {
            read_legacy_with(task, Some(owner), |params| self.objects.read(params)).await?
        }
        .ok_or_else(IvyError::internal)?;
        if prepared {
            return Ok(full);
        }
        Ok(
            upgrade_stored_with("task-board/task", &full, |params| self.objects.read(params))
                .await?
                .into_owned(),
        )
    }
    pub(crate) async fn read_document<E>(
        &self,
        key: &str,
        address: &Json,
        primary: bool,
        include_archived: bool,
        retained: bool,
    ) -> Result<Json, E>
    where
        R: ObjectReads<E>,
        C: Contracts,
        M: TreeReads<E>,
        E: From<IvyError>,
        Parse: Fn(&str) -> Result<Json, E>,
        SqlFailure: Fn(ivy_store::rusqlite::Error) -> E,
    {
        let object_id = if address.as_js_string().is_some() {
            address
        } else {
            address.required("objectId")?
        };
        if self.local.contracts.local(key)?
            || self.local.key::<E>(Some(object_id))?.as_deref() == Some(key)
        {
            let mut document = self.local.document::<E>(key, address, primary)?;
            if !retained
                && key == "task-board/task"
                && document.required("value")?.get("commentStore").is_some()
            {
                let parent = document.required("metadata")?.required("parentId")?;
                let operation = self
                    .local
                    .document::<E>("task-board/operation", parent, false)?;
                let request = operation.required("value")?.required("request")?;
                let owner = request.get("taskId").ok_or_else(|| {
                    IvyError::new(
                        "task_board_scope_mismatch",
                        "A prepared conversation must belong to its original task operation.",
                    )
                })?;
                let full = self
                    .task_value(document.required("value")?, owner, true)
                    .await?;
                document.insert("value", full);
            }
            return Ok(document);
        }
        let cache = self.cache.and_then(ReadCache::entries);
        let cache_key = cache_key(key, address, primary, include_archived, retained)?;
        if let Some(document) = cache_get(&cache, &cache_key) {
            return Ok(document);
        }
        let mut params = Json::object([("objectId", object_id.clone())]);
        if address.as_js_string().is_none() {
            params.insert("revision", address.required("revision")?.clone());
        }
        let result = self.objects.read(Some(params)).await?;
        let object = result.required("object")?;
        let revision = result.required("revision")?;
        let content = result.required("content")?;
        if object.get("contractKey") != Some(&Json::from(key))
            || !revision
                .get("contractVersion")
                .and_then(Json::as_js_string)
                .map(|version| self.local.contracts.readable_version(key, version))
                .transpose()?
                .unwrap_or(false)
            || revision.get("mediaType") != Some(&Json::from("application/json"))
            || content.get("encoding") != Some(&Json::from("json"))
        {
            return Err(IvyError::new(
                "task_board_contract_mismatch",
                "The selected Object is not an exact supported TaskBoard record.",
            )
            .into());
        }
        let root = self
            .local
            .journal
            .root_object_id
            .clone()
            .map(Json::String)
            .unwrap_or(Json::Null);
        if primary && object.get("parentId") != Some(&root) {
            return Err(IvyError::new("task_board_scope_mismatch", "Prepared payloads and records from another workflow root are not primary domain Objects.").into());
        }
        if object.get("effectivelyArchived").and_then(Json::as_bool) == Some(true)
            && !include_archived
        {
            return Err(IvyError::new(
                "task_board_record_archived",
                "An archived workflow record is not writable workflow state.",
            )
            .into());
        }
        if address.as_js_string().is_none() && revision.get("revision") != address.get("revision") {
            return Err(IvyError::new(
                "task_board_evidence_mismatch",
                "The selected immutable revision differs from its pin.",
            )
            .into());
        }
        let pin = Json::object([
            ("objectId", object.required("id")?.clone()),
            ("revision", revision.required("revision")?.clone()),
        ]);
        let value = content.required("value")?;
        let full = if retained {
            value.clone()
        } else if key == "task-board/task" {
            self.task_value(value, object.required("id")?, false)
                .await?
        } else {
            upgrade_stored_with(key, value, |params| self.objects.read(params))
                .await?
                .into_owned()
        };
        let document = Json::object([
            ("key", key.into()),
            ("pin", pin.clone()),
            ("value", full.clone()),
            ("metadata", object.clone()),
            ("revision", revision.clone()),
        ]);
        if let (Some(cache), Some(cache_key)) = (cache, cache_key) {
            if canonical(&full, MANAGEMENT_FRAME_BYTES)?.len() <= 65536
                && cache.borrow().len() < 256
            {
                let record = CachedRecord {
                    object_id: pin.required("objectId")?.clone(),
                    document: None,
                    materialized: Some(document.clone()),
                    retained_bytes: 0,
                };
                let mut entries = cache.borrow_mut();
                remember_record(&mut entries, cache_key, record);
            }
        }
        Ok(document)
    }
    // Remaining mutation programs adapt here; the read policy above owns JSON.
    pub(crate) async fn read_graph<E>(
        &self,
        key: &str,
        address: &Value,
        primary: bool,
        include_archived: bool,
        retained: bool,
    ) -> Result<Value, E>
    where
        R: ObjectReads<E>,
        C: Contracts,
        M: TreeReads<E>,
        E: From<IvyError>,
        Parse: Fn(&str) -> Result<Json, E>,
        SqlFailure: Fn(ivy_store::rusqlite::Error) -> E,
    {
        let address = v::decoded_comment::<E>(address)?;
        self.read_document(key, &address, primary, include_archived, retained)
            .await
            .map(|value| (&value).clone())
    }
}
impl<R, C, Parse, SqlFailure, M, E> TaskReadStore<E>
    for DocumentReads<'_, R, C, Parse, SqlFailure, M>
where
    R: ObjectReads<E>,
    C: Contracts,
    M: TreeReads<E>,
    E: From<IvyError>,
    Parse: Fn(&str) -> Result<Json, E>,
    SqlFailure: Fn(ivy_store::rusqlite::Error) -> E,
{
    async fn read(&self, key: &str, address: &Json, header_only: bool) -> Result<Json, E> {
        Ok(self
            .read_document(key, address, true, true, header_only)
            .await?
            .required("value")?
            .clone())
    }
}
impl<R, C, Parse, SqlFailure, M, E> crate::wake::WakeStore<E>
    for DocumentReads<'_, R, C, Parse, SqlFailure, M>
where
    R: ObjectReads<E>,
    C: Contracts,
    M: TreeReads<E>,
    E: From<IvyError> + crate::wake::WakeFailure,
    Parse: Fn(&str) -> Result<Json, E>,
    SqlFailure: Fn(ivy_store::rusqlite::Error) -> E,
{
    async fn watched_task(&self, id: &JsString) -> Result<Json, E> {
        self.read_document(
            "task-board/task",
            &Json::String(id.clone()),
            true,
            true,
            true,
        )
        .await
    }
    fn deleted(&self, error: &E) -> bool {
        crate::wake::WakeFailure::deleted(error)
    }
}
impl<R, C, Parse, SqlFailure, M: TreeReads<E>, E> CommentReadStore<E>
    for DocumentReads<'_, R, C, Parse, SqlFailure, M>
where
    Self: TaskReadStore<E>,
{
    fn comment_summary(
        &self,
        task: &Json,
        tree: &Json,
        task_id: &JsString,
    ) -> Result<Option<Json>, E> {
        self.comments
            .summary(task, tree, &Json::String(task_id.clone()))
    }
    async fn comment_range(
        &self,
        tree: &Json,
        task_id: &JsString,
        start: usize,
        end: usize,
    ) -> Result<Vec<Json>, E> {
        self.comments
            .range(tree, &Json::String(task_id.clone()), start, end)
            .await
    }
}
impl<R, C: Contracts, Parse, SqlFailure, M, E> ConditionStore<E>
    for DocumentReads<'_, R, C, Parse, SqlFailure, M>
where
    E: From<IvyError>,
    Parse: Fn(&str) -> Result<Json, E>,
    SqlFailure: Fn(ivy_store::rusqlite::Error) -> E,
{
    fn failed_handoff(&self, run_id: &JsString) -> Result<Option<Json>, E> {
        self.local.failed_handoff(run_id)
    }
}
impl<R, C, Parse, SqlFailure, M, E> super::local_history::HistoryDocuments<E>
    for DocumentReads<'_, R, C, Parse, SqlFailure, M>
where
    R: ObjectReads<E>,
    C: Contracts,
    M: TreeReads<E>,
    E: From<IvyError>,
    Parse: Fn(&str) -> Result<Json, E>,
    SqlFailure: Fn(ivy_store::rusqlite::Error) -> E,
{
    async fn history_document(
        &self,
        key: &str,
        address: Option<&Json>,
        primary: bool,
        include_archived: bool,
    ) -> Result<Json, E> {
        self.read_document(
            key,
            address.ok_or_else(IvyError::internal)?,
            primary,
            include_archived,
            false,
        )
        .await
    }
}
