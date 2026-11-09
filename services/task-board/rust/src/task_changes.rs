//! Small Task edits preserve the original conversation and evidence references.
use super::comment_writes::tree::{CommentWriteJobs, TreeCommentWriter};
use super::persistent_reads::{
    comment_port::CommentPort,
    task_comments::{TaskComments, validate_task_content},
};
use super::reference_writes::{ReferencePages, task_attachment_pins, task_references};
use super::{Contracts, JournalStore, mutation_ports::WriteFailure, task_publication::Requests};
use crate::{
    local_executor::LocalExecutor,
    operations::{Store, TaskChange, decoded::failure},
};
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString, hash::hash_json, json::canonical,
    limits::JSON_OBJECT_CONTENT_BYTES,
};
use ivy_sdk::{rpc::RpcClient, task_comments::RpcCommentSource};
use std::sync::Arc;

impl<R: RpcClient + 'static, P: Contracts, C: Fn() -> String, I: Fn() -> JsString>
    JournalStore<'_, R, P, C, I>
{
    pub(super) async fn publish_task_change(
        &self,
        value: &Json,
        destination: &Json,
        version: &str,
        change: TaskChange,
    ) -> IvyResult<(Json, Json)> {
        let owner = destination.required("objectId")?;
        let address = Json::object([
            ("objectId", owner.clone()),
            (
                "revision",
                destination.required("expectedRevision")?.clone(),
            ),
        ]);
        let (base, base_references) = self
            .record_with(
                "task-board/task",
                &address,
                true,
                destination.get("expectedArchived").and_then(Json::as_bool) == Some(true),
                |record| Ok((record.document, record.references)),
                |record| Ok((record.document.clone(), record.references.clone())),
            )
            .await?;
        let tree = base.value.required("commentStore")?;
        super::require(
            tree.get("kind").is_some_and(|v| v.is_str("tree"))
                && tree.get("summary").is_some_and(|v| !v.is_null())
                && value.get("commentStore").map(hash_json).transpose()? == Some(hash_json(tree)?)
                && ["comments", "commentDeliveries"].iter().all(|key| {
                    base.value
                        .get(*key)
                        .and_then(Json::as_array)
                        .is_some_and(|v| v.is_empty())
                }),
            "task_board_comment_mismatch",
            "A small Task edit must preserve its original conversation root.",
        )?;
        validate_task_content(value, owner.as_js_string().ok_or_else(IvyError::internal)?)?;
        let source = Requests(self.client.as_ref());
        let identity = |scope: Json| async move {
            self.prepared_mutation_id(&scope.as_str()?)
                .await
                .map(Json::String)
                .map_err(WriteFailure::<()>::from)
        };
        let mut replacement_entries = None;
        let mut stored = match change {
            TaskChange::Header => {
                super::require(
                    ["comments", "commentDeliveries"].iter().all(|key| {
                        value
                            .get(*key)
                            .and_then(Json::as_array)
                            .is_some_and(|v| v.is_empty())
                    }),
                    "task_board_comment_mismatch",
                    "A header edit cannot change conversation entries.",
                )?;
                value.clone()
            }
            TaskChange::AppendSystem
            | TaskChange::AppendComment
            | TaskChange::Receipts
            | TaskChange::NativeCompletion
            | TaskChange::QuestionRecipient => {
                let system_only = matches!(change, TaskChange::AppendSystem);
                let summary = if matches!(change, TaskChange::QuestionRecipient) {
                    let (summary, entries) = self
                        .retargeted_comment_summary(
                            &base.value,
                            owner.as_js_string().ok_or_else(IvyError::internal)?,
                            value
                                .required("comments")?
                                .as_array()
                                .ok_or_else(IvyError::internal)?,
                        )
                        .await?;
                    replacement_entries = Some(entries);
                    Some(summary)
                } else if !matches!(
                    change,
                    TaskChange::AppendComment | TaskChange::NativeCompletion
                ) {
                    None
                } else {
                    let added = value
                        .required("comments")?
                        .as_array()
                        .ok_or_else(IvyError::internal)?;
                    let added = added
                        .iter()
                        .filter(|comment| {
                            !matches!(change, TaskChange::NativeCompletion)
                                || comment.get("sequence").and_then(Json::as_f64)
                                    > tree.get("length").and_then(Json::as_f64)
                        })
                        .collect::<Vec<_>>();
                    super::require(
                        added.len() <= 1
                            && (matches!(change, TaskChange::NativeCompletion) || added.len() == 1),
                        "task_board_comment_mismatch",
                        "A comment append needs exactly one new entry.",
                    )?;
                    if let Some(comment) = added.first() {
                        let context = self
                            .comment_context(
                                &base.value,
                                owner.as_js_string().ok_or_else(IvyError::internal)?,
                                comment,
                            )
                            .await?;
                        context.validate_identities(comment)?;
                        Some(
                            self.appended_comment_summary(
                                tree,
                                owner.as_js_string().ok_or_else(IvyError::internal)?,
                                comment,
                            )
                            .await?,
                        )
                    } else {
                        None
                    }
                };
                let comments = TaskComments::new(
                    Arc::new(RpcCommentSource::new(self.client.clone())),
                    tokio::runtime::Handle::current(),
                );
                let port = CommentPort {
                    comments: &comments,
                    source_failure: &WriteFailure::<()>::from,
                };
                let jobs = CommentWriteJobs::default();
                let executor = LocalExecutor::default();
                let writer = TreeCommentWriter {
                    jobs: &jobs,
                    executor: &executor,
                    source: &source,
                    identity: &identity,
                    comments: &port,
                };
                let result = if matches!(change, TaskChange::QuestionRecipient) {
                    executor
                        .drive(writer.retarget(
                            value,
                            owner,
                            version,
                            summary.ok_or_else(IvyError::internal)?,
                        ))
                        .await
                } else if matches!(change, TaskChange::Receipts) {
                    executor.drive(writer.receipts(value, owner, version)).await
                } else if matches!(change, TaskChange::NativeCompletion) {
                    executor
                        .drive(writer.completion(value, owner, version, summary))
                        .await
                } else {
                    executor
                        .drive(writer.append(value, owner, version, system_only, summary))
                        .await
                }
                .map_err(failure);
                executor.drive(jobs.settled()).await;
                comments.settled().await;
                result?
            }
        };
        if matches!(
            change,
            TaskChange::AppendComment
                | TaskChange::NativeCompletion
                | TaskChange::QuestionRecipient
        ) {
            let questions = stored
                .required("commentStore")?
                .required("summary")?
                .required("questions")?
                .as_array()
                .ok_or_else(IvyError::internal)?;
            let count = questions
                .iter()
                .filter(|question| {
                    question.get("open").and_then(Json::as_bool) == Some(true)
                        && question
                            .get("recipient")
                            .and_then(|v| v.get("kind"))
                            .is_none_or(|v| !v.is_str("task"))
                })
                .count();
            stored.insert("openQuestionCount", Json::Number(count as f64));
        }
        self.contracts
            .validate_record("task-board/task", &version.into(), &stored)?;
        canonical(&stored, JSON_OBJECT_CONTENT_BYTES)?;
        if !matches!(change, TaskChange::Header) {
            let entries = if let Some(entries) = replacement_entries {
                entries
            } else {
                let comments = value
                    .required("comments")?
                    .as_array()
                    .ok_or_else(IvyError::internal)?;
                let receipts = value
                    .required("commentDeliveries")?
                    .as_array()
                    .ok_or_else(IvyError::internal)?;
                comments
                    .iter()
                    .enumerate()
                    .filter_map(|(at, comment)| {
                        let receipt = if matches!(
                            change,
                            TaskChange::Receipts | TaskChange::NativeCompletion
                        ) {
                            receipts.get(at)
                        } else {
                            receipts.iter().find(|receipt| {
                                receipt.get("commentId") == comment.get("commentId")
                            })
                        };
                        if matches!(change, TaskChange::NativeCompletion)
                            && receipt.is_none()
                            && comment.get("sequence").and_then(Json::as_f64)
                                <= tree.get("length").and_then(Json::as_f64)
                        {
                            return None;
                        }
                        Some(Json::object([
                            ("comment", comment.clone()),
                            ("receipt", receipt.cloned().unwrap_or(Json::Null)),
                        ]))
                    })
                    .collect::<Vec<_>>()
            };
            self.extend_comment_index(
                tree,
                stored.required("commentStore")?,
                owner.as_js_string().ok_or_else(IvyError::internal)?,
                &entries,
            )?;
        }
        let mut named = Json::Object(vec![]);
        for (field, name) in [("latestResult", "result"), ("lastRun", "run")] {
            if let Some(pin) = stored.get(field).filter(|pin| !pin.is_null()) {
                named.insert(name, pin.clone());
            }
        }
        if let Some(root) = stored
            .required("commentStore")?
            .get("root")
            .filter(|root| !root.is_null())
        {
            named.insert("conversation", root.required("object")?.clone());
        }
        // At most 256 existing direct pins. Opaque reference pages retain their
        // own original pins, including attachments in older comment snapshots.
        let mut attachments = base_references
            .ok_or_else(IvyError::internal)?
            .as_object()
            .ok_or_else(IvyError::internal)?
            .iter()
            .filter(|(name, _)| {
                !["conversation", "result", "run"]
                    .iter()
                    .any(|key| name == &JsString::from(*key))
            })
            .map(|(_, pin)| pin.clone())
            .collect::<Vec<_>>();
        let added = task_attachment_pins::<()>(value).map_err(failure)?;
        attachments.extend(
            added
                .as_array()
                .ok_or_else(IvyError::internal)?
                .iter()
                .cloned(),
        );
        let references = task_references(
            &source,
            &identity,
            owner,
            &named,
            &Json::Array(attachments),
            ReferencePages::Immutable,
        )
        .await
        .map_err(failure)?;
        Ok((stored, references))
    }
}
