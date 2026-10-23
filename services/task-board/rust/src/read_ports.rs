//! Compose value reads with the actual immutable conversation core. The value
//! reader owns header/retained/local/materialized routing and has no default.
use super::task_comments::{CommentFailure, CommentReads, TaskComments};
use crate::{
    engine_read::comment_reads::CommentReadStore,
    engine_read::read_policy::{QuestionPolicies, TaskReadStore},
};

use ivy_contracts::{IvyError, IvyJson as Json, JsString};
use ivy_sdk::task_comments::CommentSource;
use std::{borrow::Cow, future::Future};

pub enum TreeReadMode<'a> {
    Retained,
    Remote(&'a JsString),
    /// The owning Store supplies operation.request.taskId, not the local pin.
    Prepared(&'a JsString),
}

pub type ReadPorts<'a, R, S: CommentSource + ?Sized, E> =
    ReadPortsWithComments<'a, R, TaskComments<S>, E>;

pub struct ReadPortsWithComments<'a, R, M: CommentReads + ?Sized, E> {
    pub records: &'a R,
    pub comments: &'a M,
    /// A typed RPC source uses its Ivy conversion; an arbitrary thrown source
    /// uses its identity carrier. Neither classification is guessed here.
    pub source_failure: &'a dyn Fn(M::Error) -> E,
}
impl<R, M: CommentReads + ?Sized, E: From<IvyError>> ReadPortsWithComments<'_, R, M, E> {
    fn failure(&self, error: CommentFailure<M::Error>) -> E {
        match error {
            CommentFailure::Source(error) => (self.source_failure)(error),
            CommentFailure::Ivy(error) => error.into(),
        }
    }
    pub async fn read_tree<P: QuestionPolicies<E>>(
        &self,
        value: &Json,
        task_id: &JsString,
        policies: &P,
    ) -> Result<Option<Json>, E> {
        self.comments
            .read_tree_with(value, task_id, policies, &|error| self.failure(error))
            .await
    }
    /// Compose only the tree-backed Task value path. `None` requires the
    /// separate inline/retained-prefix path; it is never an empty fallback.
    pub async fn read_tree_record<'a, P, F, Fut>(
        &self,
        value: &'a Json,
        mode: TreeReadMode<'_>,
        policies: &P,
        verification_read: F,
    ) -> Result<Option<Cow<'a, Json>>, E>
    where
        P: QuestionPolicies<E>,
        F: FnMut(Option<Json>) -> Fut,
        Fut: Future<Output = Result<Json, E>>,
    {
        let full = match mode {
            TreeReadMode::Retained => return Ok(Some(Cow::Borrowed(value))),
            TreeReadMode::Remote(id) | TreeReadMode::Prepared(id) => {
                self.read_tree(value, id, policies).await?
            }
        };
        let Some(full) = full else {
            return Ok(None);
        };
        let full = match mode {
            TreeReadMode::Remote(_) => super::record_upgrades::upgrade_stored_with(
                "task-board/task",
                &full,
                verification_read,
            )
            .await?
            .into_owned(),
            TreeReadMode::Prepared(_) => full,
            TreeReadMode::Retained => unreachable!(),
        };
        Ok(Some(Cow::Owned(full)))
    }
}
impl<R: TaskReadStore<E>, M: CommentReads + ?Sized, E> TaskReadStore<E>
    for ReadPortsWithComments<'_, R, M, E>
{
    async fn read(&self, key: &str, address: &Json, header_only: bool) -> Result<Json, E> {
        self.records.read(key, address, header_only).await
    }
}
impl<R: TaskReadStore<E>, M: CommentReads + ?Sized, E: From<IvyError>> CommentReadStore<E>
    for ReadPortsWithComments<'_, R, M, E>
{
    fn comment_summary(
        &self,
        task: &Json,
        tree: &Json,
        task_id: &JsString,
    ) -> Result<Option<Json>, E> {
        self.comments
            .summary(task, tree, task_id)
            .map_err(|error| self.failure(error))
    }
    async fn comment_range(
        &self,
        tree: &Json,
        task_id: &JsString,
        start: usize,
        end: usize,
    ) -> Result<Vec<Json>, E> {
        self.comments
            .range(tree, task_id, Some(start), Some(end), None)
            .await
            .map_err(|error| self.failure(error))
    }
}
