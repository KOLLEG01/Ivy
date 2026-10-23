//! Document reads combined with the shared conversation reader.
use super::{
    document_reads::{DocumentReads, TreeReads},
    local_history::HistoryDocuments,
    local_reads::SqliteReads,
    read_ports::ReadPortsWithComments,
    task_comments::{CommentFailure, CommentReads, TaskComments},
};
use crate::{
    engine_read::read_policy::{QuestionPolicies, TaskReadStore},
    engine_read::{comment_reads::CommentReadStore, condition_policy::ConditionStore},
    journal_store::{Contracts, ReadCache},
};
use ivy_contracts::{IvyError, IvyJson as Json, JsString};
use ivy_sdk::{
    rpc::{OptionalRpcClient, RequestOptions},
    task_comments::CommentSource,
};

#[allow(async_fn_in_trait)]
pub trait ObjectReads<E> {
    async fn read(&self, params: Option<Json>) -> Result<Json, E>;
}
impl<R, C, Parse, SqlFailure, P, M, E> HistoryDocuments<E>
    for PersistentReadStoreWithComments<'_, R, C, Parse, SqlFailure, P, M, E>
where
    R: ObjectReads<E>,
    C: Contracts,
    P: QuestionPolicies<E>,
    M: CommentReads + ?Sized,
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
        let address = address.ok_or_else(IvyError::internal)?;
        self.read_document(key, address, primary, include_archived, false)
            .await
    }
}
/// The transport classifies its typed RPC errors before entering Store policies.
/// Other ObjectReads implementations may retain an arbitrary source error.
pub struct RpcObjectReads<'a, R: ?Sized> {
    pub client: &'a R,
}
impl<R: OptionalRpcClient + ?Sized, E: From<IvyError>> ObjectReads<E> for RpcObjectReads<'_, R> {
    async fn read(&self, params: Option<Json>) -> Result<Json, E> {
        self.client
            .request_optional("objects.read", params, RequestOptions::default())
            .await
            .map_err(E::from)
    }
}
pub type PersistentReadStore<'a, R, C, Parse, SqlFailure, P, S: CommentSource + ?Sized, E> =
    PersistentReadStoreWithComments<'a, R, C, Parse, SqlFailure, P, TaskComments<S>, E>;

pub struct PersistentReadStoreWithComments<
    'a,
    R,
    C,
    Parse,
    SqlFailure,
    P,
    M: CommentReads + ?Sized,
    E,
> {
    pub local: &'a SqliteReads<'a, C, Parse, SqlFailure>,
    pub objects: &'a R,
    pub policies: &'a P,
    pub comments: &'a M,
    pub source_failure: &'a dyn Fn(M::Error) -> E,
    pub cache: Option<&'a ReadCache>,
}
struct DecodedTrees<'a, M: CommentReads + ?Sized, P, E> {
    comments: &'a M,
    policies: &'a P,
    source_failure: &'a dyn Fn(M::Error) -> E,
}
impl<M: CommentReads + ?Sized, P, E: From<IvyError>> DecodedTrees<'_, M, P, E> {
    fn failure(&self, error: CommentFailure<M::Error>) -> E {
        match error {
            CommentFailure::Source(error) => (self.source_failure)(error),
            CommentFailure::Ivy(error) => error.into(),
        }
    }
}
impl<M: CommentReads + ?Sized, P: QuestionPolicies<E>, E: From<IvyError>> TreeReads<E>
    for DecodedTrees<'_, M, P, E>
{
    async fn hydrate(&self, task: &Json, owner: &Json) -> Result<Option<Json>, E> {
        let owner = owner.as_js_string().ok_or_else(IvyError::internal)?;
        self.comments
            .read_tree_with(task, owner, self.policies, &|e| self.failure(e))
            .await
    }
    fn summary(&self, task: &Json, tree: &Json, owner: &Json) -> Result<Option<Json>, E> {
        self.comments
            .summary(
                task,
                tree,
                owner.as_js_string().ok_or_else(IvyError::internal)?,
            )
            .map_err(|e| self.failure(e))
    }
    async fn range(
        &self,
        tree: &Json,
        owner: &Json,
        start: usize,
        end: usize,
    ) -> Result<Vec<Json>, E> {
        let owner = owner.as_js_string().ok_or_else(IvyError::internal)?;
        self.comments
            .range(tree, owner, Some(start), Some(end), None)
            .await
            .map_err(|e| self.failure(e))
    }
}
impl<R, C, Parse, SqlFailure, P, M, E>
    PersistentReadStoreWithComments<'_, R, C, Parse, SqlFailure, P, M, E>
where
    R: ObjectReads<E>,
    C: Contracts,
    P: QuestionPolicies<E>,
    M: CommentReads + ?Sized,
    E: From<IvyError>,
    Parse: Fn(&str) -> Result<Json, E>,
    SqlFailure: Fn(ivy_store::rusqlite::Error) -> E,
{
    fn ports(&self) -> ReadPortsWithComments<'_, Self, M, E> {
        ReadPortsWithComments {
            records: self,
            comments: self.comments,
            source_failure: self.source_failure,
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
        let comments = DecodedTrees {
            comments: self.comments,
            policies: self.policies,
            source_failure: self.source_failure,
        };
        DocumentReads {
            local: self.local,
            objects: self.objects,
            comments: &comments,
            cache: self.cache,
        }
        .read_document(key, address, primary, include_archived, retained)
        .await
    }
}
impl<R, C, Parse, SqlFailure, P, M, E> TaskReadStore<E>
    for PersistentReadStoreWithComments<'_, R, C, Parse, SqlFailure, P, M, E>
where
    R: ObjectReads<E>,
    C: Contracts,
    P: QuestionPolicies<E>,
    M: CommentReads + ?Sized,
    E: From<IvyError>,
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
impl<R, C, Parse, SqlFailure, P, M, E> CommentReadStore<E>
    for PersistentReadStoreWithComments<'_, R, C, Parse, SqlFailure, P, M, E>
where
    R: ObjectReads<E>,
    C: Contracts,
    P: QuestionPolicies<E>,
    M: CommentReads + ?Sized,
    E: From<IvyError>,
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
impl<R, C, Parse, SqlFailure, P, M, E> ConditionStore<E>
    for PersistentReadStoreWithComments<'_, R, C, Parse, SqlFailure, P, M, E>
where
    C: Contracts,
    M: CommentReads + ?Sized,
    E: From<IvyError>,
    Parse: Fn(&str) -> Result<Json, E>,
    SqlFailure: Fn(ivy_store::rusqlite::Error) -> E,
{
    fn failed_handoff(&self, run_id: &JsString) -> Result<Option<Json>, E> {
        self.local.failed_handoff(run_id)
    }
}
