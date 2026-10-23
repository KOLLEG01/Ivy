//! Native comment reads shared by document readers and publishers.
use super::{
    document_reads::TreeReads,
    task_comments::{CommentFailure, TaskComments},
};
use ivy_contracts::{IvyError, IvyJson as Json, JsString};
use ivy_sdk::task_comments::CommentSource;

pub(crate) struct CommentPort<'a, S: CommentSource + ?Sized, E> {
    pub(crate) comments: &'a TaskComments<S>,
    pub(crate) source_failure: &'a dyn Fn(S::Error) -> E,
}
impl<S: CommentSource + ?Sized, E: From<IvyError>> CommentPort<'_, S, E> {
    fn failure(&self, error: CommentFailure<S::Error>) -> E {
        match error {
            CommentFailure::Source(e) => (self.source_failure)(e),
            CommentFailure::Ivy(e) => e.into(),
        }
    }
    fn owner(owner: &Json) -> Result<&JsString, E> {
        owner
            .as_js_string()
            .ok_or_else(IvyError::internal)
            .map_err(E::from)
    }
}
impl<S: CommentSource + ?Sized, E: From<IvyError>> TreeReads<E> for CommentPort<'_, S, E> {
    async fn hydrate(&self, task: &Json, owner: &Json) -> Result<Option<Json>, E> {
        self.comments
            .hydrate(task, Self::owner(owner)?)
            .await
            .map_err(|e| self.failure(e))
    }
    fn summary(&self, task: &Json, tree: &Json, owner: &Json) -> Result<Option<Json>, E> {
        self.comments
            .summary(task, tree, Self::owner(owner)?)
            .map_err(|e| self.failure(e))
    }
    async fn range(
        &self,
        tree: &Json,
        owner: &Json,
        start: usize,
        end: usize,
    ) -> Result<Vec<Json>, E> {
        self.comments
            .range(tree, Self::owner(owner)?, Some(start), Some(end), None)
            .await
            .map_err(|e| self.failure(e))
    }
}
impl<S: CommentSource + ?Sized, E>
    CommentPort<'_, S, crate::journal_store::mutation_ports::WriteFailure<E>>
{
    pub(crate) async fn read_for_write<
        M: crate::journal_store::mutation_ports::LocalMutationSource<E>,
    >(
        &self,
        source: &M,
        task: &Json,
        owner: &Json,
    ) -> Result<Json, crate::journal_store::mutation_ports::WriteFailure<E>> {
        if let Some(full) = super::legacy_reads::read_legacy_with(task, Some(owner), |params| {
            source.request_owned("objects.read", params)
        })
        .await?
        {
            return Ok(full);
        }
        self.hydrate(task, owner)
            .await?
            .ok_or_else(|| IvyError::internal().into())
    }
    pub(crate) async fn node_for_write(
        &self,
        pin: &Json,
        owner: &Json,
    ) -> Result<Json, crate::journal_store::mutation_ports::WriteFailure<E>> {
        Ok(self
            .comments
            .node(pin, Self::owner(owner)?, None)
            .await
            .map_err(|e| self.failure(e))?
            .as_ref()
            .clone())
    }
    pub(crate) fn written_node(
        &self,
        hash: String,
        pin: &Json,
        node: Json,
    ) -> Result<(), crate::journal_store::mutation_ports::WriteFailure<E>> {
        self.comments
            .remember_node(hash, pin, node)
            .map_err(|e| self.failure(e))
    }
    pub(crate) fn cached_write_pin(&self, hash: &str) -> Option<Json> {
        self.comments.pin_for_hash(hash)
    }
    pub(crate) fn summary_for_write(
        &self,
        task: &Json,
        owner: &Json,
    ) -> Result<Json, crate::journal_store::mutation_ports::WriteFailure<E>> {
        Ok(super::comment_projections::current_summary(
            task,
            Self::owner(owner)?,
        )?)
    }
}
