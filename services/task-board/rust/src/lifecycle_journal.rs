//! Lifecycle reads/effects on the journal's authenticated connection.
//! Paging remains bound to the owning store's current local/remote query policy.
use crate::{
    journal_store::{Contracts, JournalStore},
    local_workspaces::{LocalWorkspaces, workspace_key},
    native_lifecycle::{LifecycleDocument, LifecycleStore},
    operations::{Document, Store},
};
use ivy_contracts::{IvyJson as Json, IvyResult, JsString};
use ivy_sdk::rpc::{RequestOptions, RpcClient};
#[allow(async_fn_in_trait)]
pub trait LifecyclePages {
    async fn page(&self, key: &str, options: &Json) -> IvyResult<Json>;
}
pub struct LifecycleJournal<'a, 'db, R, P, C, I, Q> {
    pub journal: &'a JournalStore<'db, R, P, C, I>,
    pub pages: &'a Q,
}
impl<R: RpcClient + 'static, P: Contracts, C: Fn() -> String, I: Fn() -> JsString, Q> Store
    for LifecycleJournal<'_, '_, R, P, C, I, Q>
{
    fn root_object_id(&self) -> Option<JsString> {
        self.journal.root_object_id()
    }
    fn contract_version(&self, k: &str) -> IvyResult<String> {
        self.journal.contract_version(k)
    }
    fn retired_operation(&self, c: &JsString, id: &JsString) -> IvyResult<bool> {
        self.journal.retired_operation(c, id)
    }
    fn local_key(&self, id: &JsString) -> IvyResult<Option<String>> {
        self.journal.local_key(id)
    }
    fn complete_local_workflow(&self, id: &JsString, t: &JsString) -> IvyResult<()> {
        self.journal.complete_local_workflow(id, t)
    }
    async fn named(&self, k: &str, n: &str, p: Option<&JsString>) -> IvyResult<Option<Document>> {
        self.journal.named(k, n, p).await
    }
    async fn read(&self, k: &str, a: &Json, p: bool) -> IvyResult<Document> {
        self.journal.read(k, a, p).await
    }
    async fn write(&self, k: &str, v: &Json, m: &str, d: &Json) -> IvyResult<Json> {
        self.journal.write(k, v, m, d).await
    }
    async fn prepared_mutation_id(&self, m: &str) -> IvyResult<JsString> {
        self.journal.prepared_mutation_id(m).await
    }
    async fn write_prepared(&self, k: &str, v: &Json, m: &JsString, d: &Json) -> IvyResult<Json> {
        self.journal.write_prepared(k, v, m, d).await
    }
}
impl<
    R: RpcClient + 'static,
    P: Contracts,
    C: Fn() -> String,
    I: Fn() -> JsString,
    Q: LifecyclePages,
> LifecycleStore for LifecycleJournal<'_, '_, R, P, C, I, Q>
{
    async fn lifecycle_read(
        &self,
        k: &str,
        a: &Json,
        p: bool,
        archived: bool,
    ) -> IvyResult<LifecycleDocument> {
        let (document, metadata) = self.journal.read_with_metadata(k, a, p, archived).await?;
        Ok(LifecycleDocument { document, metadata })
    }
    async fn page(&self, k: &str, o: &Json) -> IvyResult<Json> {
        self.pages.page(k, o).await
    }
    async fn archive(&self, p: &Json) -> IvyResult<Json> {
        self.journal
            .client
            .request("objects.archive", p.clone(), RequestOptions::default())
            .await
    }
    async fn stat(&self, id: &JsString) -> IvyResult<Json> {
        self.journal
            .client
            .request(
                "objects.stat",
                Json::object([("objectId", Json::String(id.clone()))]),
                RequestOptions::default(),
            )
            .await
    }
    fn release_workspace(&self, id: &JsString) -> IvyResult<()> {
        LocalWorkspaces {
            connection: self.journal.journal.connection,
            store: self.journal,
            normalize: workspace_key,
        }
        .release(id)
    }
}
