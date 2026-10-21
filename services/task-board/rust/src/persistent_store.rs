//! Current Store.read assembled from the existing value/tree/upgrade policies.
//! The source must preserve optional RPC parameters; a JSON-only client is not
//! silently substituted for that boundary.
use super::{
    legacy_reads::read_legacy_with,
    local_reads::SqliteReads,
    read_ports::{ReadPorts, TreeReadMode},
    record_upgrades::upgrade_stored_with,
    task_comments::TaskComments,
    values::{field, has, insert, same},
};
use crate::{
    coordination::next_steps::{NextStepPolicies, NextStepStore},
    engine_read::{comment_reads::CommentReadStore, condition_policy::ConditionStore},
    journal_store::{CacheEntries, CachedRecord, Contracts, ReadCache},
};
use ivy_contracts::{
    IvyError, IvyJson as Json, JsString,
    error::JavaScriptException,
    json::{canonical, js_string},
    limits::MANAGEMENT_FRAME_BYTES,
};
use ivy_sdk::task_comments::{CommentException, CommentSource};

#[allow(async_fn_in_trait)]
pub trait ObjectReads<E> {
    async fn read(&self, params: Option<Json>) -> Result<Json, E>;
}
pub struct PersistentReadStore<'a, R, C, Parse, SqlFailure, P, S: CommentSource + ?Sized, E> {
    pub local: &'a SqliteReads<'a, C, Parse, SqlFailure>,
    pub objects: &'a R,
    pub policies: &'a P,
    pub comments: &'a TaskComments<S>,
    pub source_failure: &'a dyn Fn(S::Error) -> E,
    pub cache: Option<&'a ReadCache>,
}
fn cache_key<E: From<CommentException>>(
    key: &str,
    address: &Json,
    primary: bool,
    archived: bool,
    retained: bool,
) -> Result<Option<Json>, E> {
    if matches!(address, Json::String(_)) {
        return Ok(None);
    }
    let mut key = JsString::from(format!("{key}:"));
    for (index, value) in [
        field::<E>(Some(address), "objectId")?,
        field::<E>(Some(address), "revision")?,
    ]
    .into_iter()
    .enumerate()
    {
        if index > 0 {
            key.0.push(u16::from(b':'))
        }
        key.0
            .extend(value.map(js_string).unwrap_or_else(|| "undefined".into()).0);
    }
    key.0
        .extend(format!(":{primary}:{archived}:{retained}").encode_utf16());
    Ok(Some(Json::String(key)))
}
fn cache_get(cache: &Option<CacheEntries>, key: &Option<Json>) -> Option<Json> {
    let (Some(cache), Some(key)) = (cache, key) else {
        return None;
    };
    cache
        .borrow()
        .iter()
        .find(|(k, _)| k == key)
        .and_then(|(_, v)| v.materialized.clone())
}
impl<R, C, Parse, SqlFailure, P, S, E> PersistentReadStore<'_, R, C, Parse, SqlFailure, P, S, E>
where
    R: ObjectReads<E>,
    C: Contracts,
    P: NextStepPolicies<E>,
    S: CommentSource + ?Sized,
    E: From<IvyError> + From<CommentException> + From<JavaScriptException>,
    Parse: Fn(&str) -> Result<Json, E>,
    SqlFailure: Fn(ivy_store::rusqlite::Error) -> E,
{
    fn ports(&self) -> ReadPorts<'_, Self, S, E> {
        ReadPorts {
            records: self,
            comments: self.comments,
            source_failure: self.source_failure,
        }
    }
    async fn task_value(
        &self,
        value: &Json,
        task_id: Option<&Json>,
        prepared: bool,
    ) -> Result<Json, E> {
        // Remote Object metadata and prepared DurableRequest taskId are string
        // identifiers at the existing validated transport/journal boundary.
        let tree = field::<E>(Some(value), "commentStore")?.filter(|value| super::truthy(value));
        if let Some(tree) = tree {
            if has::<E>(Some(tree), "kind")? {
                let id = task_id
                    .and_then(Json::as_js_string)
                    .ok_or_else(IvyError::internal)?;
                let mode = if prepared {
                    TreeReadMode::Prepared(id)
                } else {
                    TreeReadMode::Remote(id)
                };
                return self
                    .ports()
                    .read_tree_record(value, mode, self.policies, |params| {
                        self.objects.read(params)
                    })
                    .await?
                    .map(std::borrow::Cow::into_owned)
                    .ok_or_else(|| IvyError::internal().into());
            }
        }
        let hydrated = read_legacy_with(value, task_id, |params| self.objects.read(params))
            .await?
            .ok_or_else(IvyError::internal)?;
        if prepared {
            Ok(hydrated)
        } else {
            Ok(upgrade_stored_with("task-board/task", &hydrated, |params| {
                self.objects.read(params)
            })
            .await?
            .into_owned())
        }
    }
    pub async fn read_document(
        &self,
        key: &str,
        address: &Json,
        primary: bool,
        include_archived: bool,
        retained: bool,
    ) -> Result<Json, E> {
        let object_id = if matches!(address, Json::String(_)) {
            Some(address)
        } else {
            field::<E>(Some(address), "objectId")?
        };
        if self.local.contracts.local(key)?
            || self.local.key::<E>(object_id)?.as_deref() == Some(key)
        {
            let mut document = self.local.document::<E>(key, address, primary)?;
            if !retained
                && key == "task-board/task"
                && has::<E>(field::<E>(Some(&document), "value")?, "commentStore")?
            {
                let parent = field::<E>(field::<E>(Some(&document), "metadata")?, "parentId")?
                    .ok_or_else(IvyError::internal)?;
                let operation = self
                    .local
                    .document::<E>("task-board/operation", parent, false)?;
                let request = field::<E>(field::<E>(Some(&operation), "value")?, "request")?;
                if !has::<E>(request, "taskId")? {
                    return Err(IvyError::new(
                        "task_board_scope_mismatch",
                        "A prepared conversation must belong to its original task operation.",
                    )
                    .into());
                }
                let task_id = field::<E>(request, "taskId")?;
                let value = self
                    .task_value(document.get("value").unwrap(), task_id, true)
                    .await?;
                document.insert("value", value);
            }
            return Ok(document);
        }
        let cache = self.cache.and_then(ReadCache::entries);
        let cache_key = cache_key::<E>(key, address, primary, include_archived, retained)?;
        if let Some(document) = cache_get(&cache, &cache_key) {
            return Ok(document);
        }
        let mut params = Json::Object(vec![]);
        insert(&mut params, "objectId", object_id);
        if !matches!(address, Json::String(_)) {
            insert(
                &mut params,
                "revision",
                field::<E>(Some(address), "revision")?,
            )
        }
        let result = self.objects.read(Some(params)).await?;
        let object = field::<E>(Some(&result), "object")?;
        if field::<E>(object, "contractKey")? != Some(&Json::from(key))
            || !match field::<E>(field::<E>(Some(&result), "revision")?, "contractVersion")?
                .and_then(Json::as_js_string)
            {
                Some(version) => self.local.contracts.readable_version(key, version)?,
                None => false,
            }
            || field::<E>(field::<E>(Some(&result), "revision")?, "mediaType")?
                != Some(&Json::from("application/json"))
            || field::<E>(field::<E>(Some(&result), "content")?, "encoding")?
                != Some(&Json::from("json"))
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
        if primary && !same(field::<E>(object, "parentId")?, Some(&root)) {
            return Err(IvyError::new("task_board_scope_mismatch","Prepared payloads and records from another workflow root are not primary domain Objects.").into());
        }
        if field::<E>(object, "effectivelyArchived")?.is_some_and(super::truthy)
            && !include_archived
        {
            return Err(IvyError::new(
                "task_board_record_archived",
                "An archived workflow record is not writable workflow state.",
            )
            .into());
        }
        let revision = field::<E>(Some(&result), "revision")?;
        if !matches!(address, Json::String(_))
            && !same(
                field::<E>(revision, "revision")?,
                field::<E>(Some(address), "revision")?,
            )
        {
            return Err(IvyError::new(
                "task_board_evidence_mismatch",
                "The selected immutable revision differs from its pin.",
            )
            .into());
        }
        let mut pin = Json::Object(vec![]);
        insert(&mut pin, "objectId", field::<E>(object, "id")?);
        insert(&mut pin, "revision", field::<E>(revision, "revision")?);
        let value = field::<E>(field::<E>(Some(&result), "content")?, "value")?;
        let full = if retained {
            value.cloned()
        } else {
            let value = match value {
                Some(value) => value,
                None => {
                    field::<E>(None, "commentStore")?;
                    unreachable!()
                }
            };
            Some(if key == "task-board/task" {
                self.task_value(value, field::<E>(object, "id")?, false)
                    .await?
            } else {
                upgrade_stored_with(key, value, |params| self.objects.read(params))
                    .await?
                    .into_owned()
            })
        };
        let mut document = Json::object([("key", key.into()), ("pin", pin)]);
        insert(&mut document, "value", full.as_ref());
        insert(&mut document, "metadata", object);
        insert(&mut document, "revision", revision);
        // The captured map belongs to the action that initiated this read.
        // Canonical validation happens even when the map is already at capacity.
        if let (Some(cache), Some(cache_key)) = (cache, cache_key) {
            let value = full.as_ref().ok_or_else(|| {
                IvyError::new("invalid_arguments", "Value is outside the JSON domain.")
            })?;
            if canonical(value, MANAGEMENT_FRAME_BYTES)?.len() <= 65536 {
                let mut entries = cache.borrow_mut();
                if entries.len() < 256 {
                    let record = CachedRecord {
                        object_id: document
                            .get("pin")
                            .unwrap()
                            .get("objectId")
                            .cloned()
                            .unwrap_or(Json::Null),
                        document: None,
                        materialized: Some(document.clone()),
                    };
                    if let Some((_, existing)) =
                        entries.iter_mut().find(|(key, _)| key == &cache_key)
                    {
                        *existing = record
                    } else {
                        entries.push((cache_key, record))
                    }
                }
            }
        }
        Ok(document)
    }
}
impl<R, C, Parse, SqlFailure, P, S, E> NextStepStore<E>
    for PersistentReadStore<'_, R, C, Parse, SqlFailure, P, S, E>
where
    R: ObjectReads<E>,
    C: Contracts,
    P: NextStepPolicies<E>,
    S: CommentSource + ?Sized,
    E: From<IvyError> + From<CommentException> + From<JavaScriptException>,
    Parse: Fn(&str) -> Result<Json, E>,
    SqlFailure: Fn(ivy_store::rusqlite::Error) -> E,
{
    async fn read(&self, key: &str, address: &Json, header_only: bool) -> Result<Json, E> {
        self.read_document(key, address, true, true, header_only)
            .await?
            .get("value")
            .cloned()
            .ok_or_else(|| IvyError::internal().into())
    }
}
impl<R, C, Parse, SqlFailure, P, S, E> CommentReadStore<E>
    for PersistentReadStore<'_, R, C, Parse, SqlFailure, P, S, E>
where
    R: ObjectReads<E>,
    C: Contracts,
    P: NextStepPolicies<E>,
    S: CommentSource + ?Sized,
    E: From<IvyError> + From<CommentException> + From<JavaScriptException>,
    Parse: Fn(&str) -> Result<Json, E>,
    SqlFailure: Fn(ivy_store::rusqlite::Error) -> E,
{
    fn comment_summary(
        &self,
        task: &Json,
        tree: &Json,
        task_id: &JsString,
    ) -> Result<Option<Json>, E> {
        self.ports().comment_summary(task, tree, task_id)
    }
    async fn comment_range(
        &self,
        tree: &Json,
        task_id: &JsString,
        start: usize,
        end: usize,
    ) -> Result<Vec<Json>, E> {
        self.ports().comment_range(tree, task_id, start, end).await
    }
}
impl<R, C, Parse, SqlFailure, P, S, E> ConditionStore<E>
    for PersistentReadStore<'_, R, C, Parse, SqlFailure, P, S, E>
where
    C: Contracts,
    S: CommentSource + ?Sized,
    E: From<IvyError> + From<CommentException>,
    Parse: Fn(&str) -> Result<Json, E>,
    SqlFailure: Fn(ivy_store::rusqlite::Error) -> E,
{
    fn failed_handoff(&self, run_id: &JsString) -> Result<Option<Json>, E> {
        self.local.failed_handoff(run_id)
    }
}
