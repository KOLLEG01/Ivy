//! Lifecycle reads/effects on the journal's authenticated connection.
//! Paging remains bound to the owning store's current local/remote query policy.
use crate::{
    evidence_bytes::{BlobStore, ChunkKey},
    journal_store::{Contracts, JournalStore},
    local_workspaces::{LocalWorkspaces, workspace_key},
    native_lifecycle::{LifecycleDocument, LifecycleStore},
    operations::{Document, PreparedPayload, Store, TaskChange},
    read_evidence::ReadEvidenceStore,
};
use ivy_contracts::{IvyError, IvyJson as Json, IvyResult, JsString};
use ivy_sdk::rpc::{RequestOptions, RpcClient};
#[allow(async_fn_in_trait)]
pub trait LifecyclePages {
    async fn page(&self, key: &str, options: &Json) -> IvyResult<Json>;
}
pub struct LifecycleJournal<'a, 'db, R, P, C, I, Q> {
    pub journal: &'a JournalStore<'db, R, P, C, I>,
    pub pages: &'a Q,
}
impl<R, P, C, I, Q> BlobStore for LifecycleJournal<'_, '_, R, P, C, I, Q> {
    type Pin = Json;
    type Error = IvyError;
    fn save(&self, key: ChunkKey, name: &str, parent: &JsString, bytes: &[u8]) -> IvyResult<Json> {
        BlobStore::save(self.journal, key, name, parent, bytes)
    }
    fn read(&self, key: ChunkKey, pin: &Json, parent: &JsString) -> IvyResult<Vec<u8>> {
        BlobStore::read(self.journal, key, pin, parent)
    }
}
impl<R: RpcClient + 'static, P: Contracts, C: Fn() -> String, I: Fn() -> JsString, Q>
    ReadEvidenceStore for LifecycleJournal<'_, '_, R, P, C, I, Q>
{
    fn validate(&self, definition: &str, value: &Json) -> IvyResult<()> {
        ReadEvidenceStore::validate(self.journal, definition, value)
    }
    async fn read_document(&self, key: &str, address: &Json, primary: bool) -> IvyResult<Document> {
        ReadEvidenceStore::read_document(self.journal, key, address, primary).await
    }
    async fn read_task_routing(&self, address: &Json) -> IvyResult<Json> {
        ReadEvidenceStore::read_task_routing(self.journal, address).await
    }
    async fn write_document(
        &self,
        key: &str,
        value: &Json,
        mutation: &str,
        destination: &Json,
    ) -> IvyResult<Json> {
        ReadEvidenceStore::write_document(self.journal, key, value, mutation, destination).await
    }
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
    fn retired_operation_receipt(&self, c: &JsString, id: &JsString) -> IvyResult<Option<Json>> {
        self.journal.retired_operation_receipt(c, id)
    }
    fn local_key(&self, id: &JsString) -> IvyResult<Option<String>> {
        self.journal.local_key(id)
    }
    fn complete_local_workflow(&self, id: &JsString, t: &JsString) -> IvyResult<()> {
        self.journal.complete_local_workflow(id, t)
    }
    fn task_operations_pending(
        &self,
        task: &JsString,
        except: Option<&JsString>,
    ) -> IvyResult<bool> {
        self.journal.task_operations_pending(task, except)
    }
    async fn named(&self, k: &str, n: &str, p: Option<&JsString>) -> IvyResult<Option<Document>> {
        self.journal.named(k, n, p).await
    }
    async fn read(&self, k: &str, a: &Json, p: bool) -> IvyResult<Document> {
        Store::read(self.journal, k, a, p).await
    }
    async fn read_task_header(&self, address: &Json, primary: bool) -> IvyResult<Document> {
        self.journal.read_task_header(address, primary).await
    }
    async fn read_task_comments(
        &self,
        task: &Json,
        task_id: &JsString,
        ids: &[Json],
    ) -> IvyResult<Vec<Json>> {
        self.journal.read_task_comments(task, task_id, ids).await
    }
    async fn has_task_comment(
        &self,
        task: &Json,
        task_id: &JsString,
        id: &JsString,
    ) -> IvyResult<bool> {
        self.journal.has_task_comment(task, task_id, id).await
    }
    async fn read_completion_comments(
        &self,
        task: &Json,
        task_id: &JsString,
        run: &JsString,
        inputs: &[Json],
    ) -> IvyResult<crate::operations::CompletionComments> {
        self.journal
            .read_completion_comments(task, task_id, run, inputs)
            .await
    }
    async fn read_prepared(&self, k: &str, a: &Json) -> IvyResult<Document> {
        self.journal.read_prepared(k, a).await
    }
    async fn prepare_payload(&self, k: &str, v: Json, d: &Json) -> IvyResult<PreparedPayload> {
        self.journal.prepare_payload(k, v, d).await
    }
    async fn prepare_task_change(
        &self,
        value: &Json,
        destination: &Json,
        change: TaskChange,
    ) -> IvyResult<PreparedPayload> {
        self.journal
            .prepare_task_change(value, destination, change)
            .await
    }
    async fn write(&self, k: &str, v: &Json, m: &str, d: &Json) -> IvyResult<Json> {
        self.journal.write(k, v, m, d).await
    }
    async fn prepared_mutation_id(&self, m: &str) -> IvyResult<JsString> {
        self.journal.prepared_mutation_id(m).await
    }
    async fn write_prepared(
        &self,
        k: &str,
        v: &Json,
        m: &JsString,
        d: &Json,
        version: Option<&JsString>,
        references: Option<&Json>,
    ) -> IvyResult<Json> {
        self.journal
            .write_prepared(k, v, m, d, version, references)
            .await
    }
}
impl<R: RpcClient + 'static, P: Contracts, C: Fn() -> String, I: Fn() -> JsString>
    JournalStore<'_, R, P, C, I>
{
    pub(crate) async fn lifecycle_task_header(
        &self,
        address: &Json,
        primary: bool,
        include_archived: bool,
    ) -> IvyResult<LifecycleDocument> {
        let (mut document, metadata) = self
            .read_with_metadata("task-board/task", address, primary, include_archived)
            .await?;
        let value = &document.value;
        if !self.contracts.local("task-board/task")?
            && value.get("commentStore").is_some_and(|tree| {
                tree.get("kind").is_some_and(|kind| kind.is_str("tree"))
                    && tree.get("summary").is_some_and(|v| !v.is_null())
            })
            && ["comments", "commentDeliveries"].iter().all(|key| {
                value
                    .get(*key)
                    .and_then(Json::as_array)
                    .is_some_and(|v| v.is_empty())
            })
        {
            document.value =
                crate::journal_store::persistent_reads::record_upgrades::upgrade_stored_owned(
                    self.client.as_ref(),
                    "task-board/task",
                    document.value,
                )
                .await?;
        } else {
            document = self
                .materialize_document("task-board/task", document)
                .await?;
        }
        Ok(LifecycleDocument { document, metadata })
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
    async fn task_header(
        &self,
        address: &Json,
        primary: bool,
        archived: bool,
    ) -> IvyResult<LifecycleDocument> {
        self.journal
            .lifecycle_task_header(address, primary, archived)
            .await
    }
    async fn comment_entries(
        &self,
        task: &Json,
        id: &JsString,
        ids: &[Json],
    ) -> IvyResult<Vec<Json>> {
        self.journal.comment_entries(task, id, ids).await
    }
    async fn queued_comment_ids(&self, task: &Json, id: &JsString) -> IvyResult<Vec<Json>> {
        self.journal.queued_comment_ids(task, id).await
    }
    async fn migration_evidence(
        &self,
        catalogs: &ivy_contracts::checked_native_contract::CheckedNativeContracts,
        call: &Document,
        expected: &crate::native_intent::EvidenceIdentity,
    ) -> IvyResult<Json> {
        crate::native_evidence::NativeEvidence {
            store: self,
            catalogs,
        }
        .read(
            &call.pin.to_json(),
            call.value.required("evidence")?,
            expected,
        )
        .await
    }
    fn migration_quiescent(&self, task: &JsString, operation: &JsString) -> IvyResult<()> {
        if self
            .journal
            .journal
            .task_operations_pending(task, Some(operation))?
        {
            return Err(IvyError::new(
                "task_board_execution_unresolved",
                "Resolve this ticket's pending operations and original lifecycle outcomes before moving its route.",
            ));
        }
        Ok(())
    }
    async fn lifecycle_read(
        &self,
        k: &str,
        a: &Json,
        p: bool,
        archived: bool,
    ) -> IvyResult<LifecycleDocument> {
        let (document, metadata) = self.journal.read_with_metadata(k, a, p, archived).await?;
        let document = self.journal.materialize_document(k, document).await?;
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
                Json::object([
                    ("objectId", Json::String(id.clone())),
                    ("observe", true.into()),
                ]),
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
