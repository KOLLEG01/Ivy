//! Read-only native inspection bound to the journal's authenticated connection.
//! Principal and service-node identities come from the owner's admitted settings.
use crate::{
    evidence_bytes::{BlobStore, ChunkKey},
    journal_store::{Contracts, JournalStore},
    native_inspector::InspectionStore,
    operations::{Document, Store},
    read_evidence::ReadEvidenceStore,
};
use ivy_contracts::{IvyError, IvyJson as Json, IvyResult, JsString};
use ivy_sdk::{connection_owner::connection_owner, rpc::RpcClient, tools::ToolClients};

pub struct InspectionJournal<'a, 'db, R, P, C, I> {
    journal: &'a JournalStore<'db, R, P, C, I>,
    tools: ToolClients,
    principal_id: JsString,
    service_node_id: JsString,
}

fn truthy(value: Option<&Json>) -> bool {
    match value {
        None | Some(Json::Null) => false,
        Some(Json::Bool(value)) => *value,
        Some(Json::Number(value)) => *value != 0.0 && !value.is_nan(),
        Some(Json::String(value)) => !value.0.is_empty(),
        _ => true,
    }
}

impl<'a, 'db, R: RpcClient + 'static, P: Contracts, C: Fn() -> String, I: Fn() -> JsString>
    InspectionJournal<'a, 'db, R, P, C, I>
{
    /// Keep this adapter for the engine lifetime to reuse selected tool bindings
    /// on the same authenticated connection as its durable journal operations.
    pub fn new(
        journal: &'a JournalStore<'db, R, P, C, I>,
        principal_id: JsString,
        service_node_id: JsString,
    ) -> Self {
        Self {
            journal,
            tools: ToolClients::new(journal.client.clone()),
            principal_id,
            service_node_id,
        }
    }
}

impl<R, P, C, I> BlobStore for InspectionJournal<'_, '_, R, P, C, I> {
    type Pin = Json;
    type Error = IvyError;
    fn save(&self, key: ChunkKey, name: &str, parent: &JsString, bytes: &[u8]) -> IvyResult<Json> {
        BlobStore::save(self.journal, key, name, parent, bytes)
    }
    fn read(&self, key: ChunkKey, pin: &Json, parent: &JsString) -> IvyResult<Vec<u8>> {
        BlobStore::read(self.journal, key, pin, parent)
    }
}

impl<R: RpcClient + 'static, P: Contracts, C: Fn() -> String, I: Fn() -> JsString> ReadEvidenceStore
    for InspectionJournal<'_, '_, R, P, C, I>
{
    fn validate(&self, definition: &str, value: &Json) -> IvyResult<()> {
        ReadEvidenceStore::validate(self.journal, definition, value)
    }
    async fn read_document(&self, key: &str, address: &Json, primary: bool) -> IvyResult<Document> {
        ReadEvidenceStore::read_document(self.journal, key, address, primary).await
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

impl<R: RpcClient + 'static, P: Contracts, C: Fn() -> String, I: Fn() -> JsString> InspectionStore
    for InspectionJournal<'_, '_, R, P, C, I>
{
    async fn verify_owner(&self) -> IvyResult<()> {
        let node = connection_owner(self.journal.client.as_ref(), &self.service_node_id).await?;
        if node.get("principalId").and_then(Json::as_js_string) == Some(&self.principal_id)
            && node.get("serviceName") == Some(&Json::from("task-board"))
            && truthy(node.get("connected"))
            && truthy(node.get("synced"))
        {
            Ok(())
        } else {
            Err(IvyError::new(
                "task_board_principal_mismatch",
                "Workflow recovery requires the configured authenticated logical principal and a live synchronized service connection.",
            ))
        }
    }
    async fn named_document(
        &self,
        key: &str,
        name: &str,
        parent: &JsString,
    ) -> IvyResult<Option<Document>> {
        Store::named(self.journal, key, name, Some(parent)).await
    }
    fn tools(&self) -> &ToolClients {
        &self.tools
    }
    fn principal_id(&self) -> JsString {
        self.principal_id.clone()
    }
    fn now(&self) -> JsString {
        (self.journal.now)().into()
    }
}
