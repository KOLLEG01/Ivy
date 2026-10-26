//! Owner-scoped maintenance of verified local exports. Workflow intent, user
//! comments and delivery receipts never change as a side effect of collection.
mod conversation;
mod jobs;
pub mod operator;
pub mod records;
use crate::{
    journal_store::persistent_reads::task_comments::TaskComments,
    journal_store::{Contracts, JournalStore},
    operations::Store,
};
use ivy_contracts::{IvyError, IvyJson as Json, IvyResult, JsString, hash::hash_json};
use ivy_sdk::{
    rpc::{RequestOptions, RpcClient},
    task_comments::RpcCommentSource,
};
pub use jobs::{Maintenance, MaintenanceTick};
pub use records::*;
use records::{absolute, hash, require};
use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::Arc,
};

#[derive(Clone)]
pub struct MaintenanceContext {
    pub data_root: PathBuf,
    pub host_id: JsString,
    pub service_node_id: JsString,
    pub principal_id: JsString,
}
impl MaintenanceContext {
    pub fn from_config(config: &ivy_host_runtime::config::InstanceConfig) -> IvyResult<Self> {
        let value = config.json();
        Ok(Self {
            data_root: config.data_root().to_owned(),
            host_id: text(value, "hostId")?.clone(),
            service_node_id: text(value, "serviceNodeId")?.clone(),
            principal_id: text(value.required("settings")?, "principalId")?.clone(),
        })
    }
}

#[allow(async_fn_in_trait)]
pub trait MaintenanceStore {
    fn client(&self) -> Arc<dyn RpcClient>;
    fn contracts(&self) -> &dyn Contracts;
    fn root(&self) -> Option<JsString>;
    fn publication_busy(&self, task: &str) -> IvyResult<bool>;
    /// Local native Runs retain source Task pins even though Hive cannot see
    /// those SQLite references. Defer history pruning while such a Run lives.
    fn source_in_use(&self, task: &str) -> IvyResult<bool>;
    async fn mutation(&self, scope: &str) -> IvyResult<JsString>;
}
impl<R: RpcClient + 'static, P: Contracts, C: Fn() -> String, I: Fn() -> JsString> MaintenanceStore
    for JournalStore<'_, R, P, C, I>
{
    fn client(&self) -> Arc<dyn RpcClient> {
        self.client.clone()
    }
    fn contracts(&self) -> &dyn Contracts {
        &self.contracts
    }
    fn root(&self) -> Option<JsString> {
        self.journal.root_object_id.clone()
    }
    fn publication_busy(&self, task: &str) -> IvyResult<bool> {
        self.journal.connection.query_row("SELECT EXISTS(SELECT 1 FROM documents d JOIN revisions r ON r.id=d.id AND r.revision=d.current_revision WHERE d.key='task-board/operation' AND json_extract(r.value_json,'$.request.taskId')=? AND json_extract(r.value_json,'$.phase') IN ('accepted','preparing','publishing','needs_attention'))", [task], |row| row.get(0)).map_err(ivy_store::sqlite::error)
    }
    fn source_in_use(&self, task: &str) -> IvyResult<bool> {
        self.journal.connection.query_row("SELECT EXISTS(SELECT 1 FROM documents d JOIN revisions r ON r.id=d.id AND r.revision=d.current_revision WHERE d.key='task-board/run' AND json_extract(r.value_json,'$.taskId')=? AND COALESCE(json_extract(r.value_json,'$.phase'),'') NOT IN ('completed','failed','cancelled'))", [task], |row| row.get(0)).map_err(ivy_store::sqlite::error)
    }
    async fn mutation(&self, scope: &str) -> IvyResult<JsString> {
        self.prepared_mutation_id(scope).await
    }
}

pub(super) fn text<'a>(value: &'a Json, key: &str) -> IvyResult<&'a JsString> {
    value
        .required(key)?
        .as_js_string()
        .ok_or_else(IvyError::internal)
}
pub(super) fn items<'a>(value: &'a Json, key: &str) -> IvyResult<&'a [Json]> {
    value
        .required(key)?
        .as_array()
        .ok_or_else(IvyError::internal)
}
pub(super) fn pin(value: &Json) -> IvyResult<Json> {
    Ok(Json::object([
        (
            "objectId",
            value.required("object")?.required("id")?.clone(),
        ),
        (
            "revision",
            value.required("revision")?.required("revision")?.clone(),
        ),
    ]))
}
pub(super) fn comment_error(
    error: crate::journal_store::persistent_reads::task_comments::CommentFailure<IvyError>,
) -> IvyError {
    use crate::journal_store::persistent_reads::task_comments::CommentFailure;
    match error {
        CommentFailure::Source(error) | CommentFailure::Ivy(error) => error,
    }
}

pub struct Retention<'a, S> {
    pub store: &'a S,
    comments: TaskComments<RpcCommentSource>,
}
impl<'a, S: MaintenanceStore> Retention<'a, S> {
    pub fn new(store: &'a S) -> Self {
        Self {
            store,
            comments: TaskComments::new(
                Arc::new(RpcCommentSource::new(store.client())),
                tokio::runtime::Handle::current(),
            ),
        }
    }
    pub async fn settled(&self) {
        self.comments.settled().await;
    }
    pub(super) async fn request(&self, method: &str, params: Json) -> IvyResult<Json> {
        self.store
            .client()
            .request(method, params, RequestOptions::default())
            .await
    }
    async fn task(&self, id: &str) -> IvyResult<(Json, Json)> {
        let record = self
            .request("objects.read", Json::object([("objectId", id.into())]))
            .await?;
        let object = record.required("object")?;
        require(
            object.get("contractKey") == Some(&"task-board/task".into())
                && object.get("parentId")
                    == Some(&self.store.root().map(Json::String).unwrap_or(Json::Null))
                && object.get("ownerObjectId").is_some_and(Json::is_null),
            "task_board_scope_mismatch",
            "Maintenance must select a Task owned by this workspace.",
        )?;
        let content = record.required("content")?;
        require(
            content.get("encoding") == Some(&"json".into()),
            "task_board_contract_mismatch",
            "Maintenance requires a Task document.",
        )?;
        self.store.contracts().validate_record(
            "task-board/task",
            text(record.required("revision")?, "contractVersion")?,
            content.required("value")?,
        )?;
        let owner = id.into();
        let task = match self
            .comments
            .hydrate(content.required("value")?, &owner)
            .await
            .map_err(comment_error)?
        {
            Some(task) => task,
            None => crate::journal_store::persistent_reads::read_legacy_conversation(
                self.store.client().as_ref(),
                content.required("value")?,
                &owner,
            )
            .await?
            .unwrap_or_else(|| content.required("value").unwrap().clone()),
        };
        let task = ivy_sdk::upgrade_task_record(&task)?;
        require(
            task.get("publication").is_none_or(Json::is_null)
                && !self.store.publication_busy(id)?,
            "task_board_publication_busy",
            "Finish the original publication before retiring Task data.",
        )?;
        Ok((record, task))
    }

    pub async fn retire_attachments(
        &self,
        id: &str,
        archives: &[ArchivedAttachment],
        scope: &str,
    ) -> IvyResult<usize> {
        let (previous, mut task) = self.task(id).await?;
        let mut index = HashMap::new();
        for archive in archives {
            require(
                absolute(&archive.local_path)
                    && hash(&archive.content_hash)
                    && archive.object.revision > 0
                    && index.insert(archive.object.clone(), archive).is_none(),
                "invalid_arguments",
                "The archive must contain unique exact pins, hashes and absolute paths.",
            )?;
        }
        let mut protected = HashSet::new();
        if let Some(result) = task.get("latestResult").filter(|v| !v.is_null()) {
            let saved = self.request("objects.read", result.clone()).await?;
            require(
                saved.required("object")?.get("contractKey") == Some(&"task-board/result".into())
                    && saved.required("object")?.get("parentId")
                        == previous.required("object")?.get("parentId"),
                "task_board_scope_mismatch",
                "The retained result must belong to this workspace.",
            )?;
            for (_, pin) in saved
                .required("revision")?
                .required("references")?
                .as_object()
                .ok_or_else(IvyError::internal)?
            {
                protected.insert(hash_json(pin)?);
            }
        }
        let mut retiring = HashSet::new();
        for attachment in items(&task, "attachments")?.iter().chain(
            items(&task, "comments")?
                .iter()
                .flat_map(|c| c.get("attachments").and_then(Json::as_array).unwrap_or(&[])),
        ) {
            let pin = attachment.required("object")?;
            let address = ObjectPin {
                object_id: pin.required("objectId")?.as_str()?,
                revision: pin
                    .required("revision")?
                    .as_f64()
                    .ok_or_else(IvyError::internal)? as u64,
            };
            if let Some(archive) = index.get(&address)
                && !protected.contains(&hash_json(pin)?)
            {
                require(
                    attachment.get("contentHash") == Some(&archive.content_hash.clone().into())
                        && attachment.get("byteLength")
                            == Some(&Json::Number(archive.byte_length as f64)),
                    "task_board_evidence_mismatch",
                    "The export must match the exact published attachment.",
                )?;
                retiring.insert(hash_json(pin)?);
            }
        }
        if retiring.is_empty() {
            return Ok(0);
        }
        fn filter(value: &Json, retiring: &HashSet<String>) -> IvyResult<Json> {
            Ok(Json::Array(
                items(value, "attachments")?
                    .iter()
                    .filter_map(|v| match v.required("object").and_then(hash_json) {
                        Ok(hash) if retiring.contains(&hash) => None,
                        Ok(_) => Some(Ok(v.clone())),
                        Err(error) => Some(Err(error)),
                    })
                    .collect::<IvyResult<_>>()?,
            ))
        }
        task.insert("attachments", filter(&task, &retiring)?);
        task.insert(
            "comments",
            Json::Array(
                items(&task, "comments")?
                    .iter()
                    .map(|c| {
                        let mut c = c.clone();
                        c.insert("attachments", filter(&c, &retiring)?);
                        Ok(c)
                    })
                    .collect::<IvyResult<_>>()?,
            ),
        );
        self.publish(id, &previous, &task, scope, true).await?;
        Ok(retiring.len())
    }

    pub async fn archive_comments(
        &self,
        id: &str,
        archives: &[ArchivedCommentBody],
        scope: &str,
    ) -> IvyResult<(u64, i64)> {
        let (previous, mut task) = self.task(id).await?;
        let mut selected = HashMap::new();
        for archive in archives {
            require(
                absolute(&archive.local_path)
                    && archive.local_path.chars().count() <= 1024
                    && hash(&archive.body_hash)
                    && selected
                        .insert(archive.comment_id.as_str(), archive)
                        .is_none(),
                "invalid_arguments",
                "Comment archive identities, hashes and paths must be exact and unique.",
            )?;
        }
        let (mut rewritten, mut removed) = (0, 0);
        let mut comments = Vec::new();
        for comment in items(&task, "comments")? {
            let mut next = comment.clone();
            if let Some(id) = comment
                .get("commentId")
                .and_then(Json::as_js_string)
                .and_then(|v| v.to_utf8().ok())
                && let Some(archive) = selected.remove(id.as_str())
            {
                let note = archived_comment_note(archive);
                require(
                    imported_comment(comment)
                        && (hash_json(comment.required("body")?)? == archive.body_hash
                            || comment.get("body") == Some(&note.clone().into())),
                    "task_board_evidence_mismatch",
                    "Only an exact verified generated import comment may be archived.",
                )?;
                if comment.get("body") != Some(&note.clone().into()) {
                    removed += text(comment, "body")?.utf16_len() as i64
                        - note.encode_utf16().count() as i64;
                    rewritten += 1;
                    next.insert("body", note.into());
                }
            }
            comments.push(next);
        }
        require(
            selected.is_empty(),
            "task_board_evidence_mismatch",
            "An archived comment is missing from its ticket.",
        )?;
        if rewritten > 0 {
            task.insert("comments", Json::Array(comments));
            self.publish(id, &previous, &task, scope, false).await?;
        }
        Ok((rewritten, removed))
    }
}
pub fn archived_comment_note(archive: &ArchivedCommentBody) -> String {
    format!(
        "Imported technical metadata archived locally: {}\n\nOriginal text: {}",
        archive.local_path, archive.body_hash
    )
}
fn imported_comment(value: &Json) -> bool {
    value.get("authorKind") == Some(&"system".into())
        && value.get("author").and_then(|a| a.get("principalId"))
            == Some(&"task-board-record-upgrade".into())
        && value
            .get("commentId")
            .and_then(Json::as_js_string)
            .and_then(|id| id.to_utf8().ok())
            .is_some_and(|id| {
                id.starts_with("schedule-context-") || id.starts_with("retained-findings-")
            })
}
