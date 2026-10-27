//! Persist hydrated Task conversations through the shared immutable-tree writer.
use super::persistent_reads::{
    comment_port::CommentPort,
    task_comments::{TaskComments, validate_task_content},
};
use super::{
    Contracts, JournalStore,
    mutation_ports::{RpcMutationSource, WriteFailure},
};
use super::{
    comment_writes::tree::{CommentWriteJobs, TreeCommentWriter},
    reference_writes::{task_attachment_pins, task_references},
};
use crate::{
    local_executor::LocalExecutor,
    operations::{Store, decoded::failure},
};
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString, json::canonical,
    limits::JSON_OBJECT_CONTENT_BYTES,
};
use ivy_sdk::{rpc::RpcClient, task_comments::RpcCommentSource};
use std::sync::Arc;

impl<R: RpcClient + 'static, P: Contracts, C: Fn() -> String, I: Fn() -> JsString>
    JournalStore<'_, R, P, C, I>
{
    pub(super) async fn publish_task_content(
        &self,
        value: &Json,
        destination: &Json,
        version: &str,
    ) -> IvyResult<(Json, Json)> {
        let owner = destination.required("objectId")?;
        let comments = TaskComments::new(
            Arc::new(RpcCommentSource::new(self.client.clone())),
            tokio::runtime::Handle::current(),
        );
        let source = RpcMutationSource(self.client.as_ref());
        let source_failure = WriteFailure::<()>::from;
        let port = CommentPort {
            comments: &comments,
            source_failure: &source_failure,
        };
        let identity = |scope: Json| async move {
            self.prepared_mutation_id(&scope.as_str()?)
                .await
                .map(Json::String)
                .map_err(WriteFailure::<()>::from)
        };
        let executor = LocalExecutor::default();
        let jobs = CommentWriteJobs::default();
        let writer = TreeCommentWriter {
            jobs: &jobs,
            executor: &executor,
            source: &source,
            identity: &identity,
            comments: &port,
        };
        let result = async {
            let full = if value
                .get("commentStore")
                .is_some_and(|store| !store.is_null())
            {
                port.read_for_write(&source, value, owner)
                    .await
                    .map_err(failure)?
            } else {
                value.clone()
            };
            validate_task_content(&full, owner.as_js_string().ok_or_else(IvyError::internal)?)?;
            let stored = executor
                .drive(writer.write_graph(&full, owner, &Json::Null, &Json::from(version)))
                .await
                .map_err(failure)?;
            self.contracts
                .validate_record("task-board/task", &version.into(), &stored)?;
            canonical(&stored, JSON_OBJECT_CONTENT_BYTES)?;
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
            let attachments = task_attachment_pins::<()>(&full).map_err(failure)?;
            let references = task_references(&source, &identity, owner, &named, &attachments)
                .await
                .map_err(failure)?;
            Ok((stored, references))
        }
        .await;
        // A failed sibling must not outlive its publication owner.
        executor.drive(jobs.settled()).await;
        comments.settled().await;
        result
    }
}
