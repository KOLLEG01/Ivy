//! Scheduler effects over the same admitted journal and workspace owner reads.
use crate::{
    allocation::AllocationStore,
    journal_store::Contracts,
    lifecycle_journal::{LifecycleJournal, LifecyclePages},
    local_workspaces::{LocalWorkspaces, workspace_key},
    operations::{Document, Store},
    scheduler::SchedulerStore,
    wake::{WakeFailure, WakeStore},
};
use ivy_contracts::{IvyError, IvyJson as Json, IvyResult, JsString};
use ivy_sdk::rpc::RpcClient;
pub trait SchedulerJournalBindings: LifecyclePages {
    fn free_bytes(&self) -> IvyResult<Option<f64>>;
}
impl<R: RpcClient + 'static, P: Contracts, C: Fn() -> String, I: Fn() -> JsString, Q> WakeStore
    for LifecycleJournal<'_, '_, R, P, C, I, Q>
{
    async fn watched_task(&self, id: &JsString) -> IvyResult<Json> {
        let (task, metadata) = self
            .journal
            .read_with_metadata("task-board/task", &Json::String(id.clone()), true, true)
            .await?;
        Ok(Json::object([
            ("value", task.value),
            ("metadata", metadata),
        ]))
    }
    fn deleted(&self, error: &IvyError) -> bool {
        WakeFailure::deleted(error)
    }
}
impl<
    R: RpcClient + 'static,
    P: Contracts,
    C: Fn() -> String,
    I: Fn() -> JsString,
    Q: LifecyclePages,
> AllocationStore for LifecycleJournal<'_, '_, R, P, C, I, Q>
{
    async fn page(&self, k: &str, o: &Json) -> IvyResult<Json> {
        self.pages.page(k, o).await
    }
    async fn workspace_owner(
        &self,
        h: &JsString,
        p: &JsString,
        t: &JsString,
    ) -> IvyResult<Option<JsString>> {
        LocalWorkspaces {
            connection: self.journal.journal.connection,
            store: self.journal,
            normalize: workspace_key,
        }
        .owner(h, p, t)
        .await
    }
    async fn named_plan(&self, n: &JsString) -> IvyResult<Option<Document>> {
        self.journal
            .named_record(
                "task-board/native-plan",
                n,
                self.journal.root_object_id().as_ref(),
            )
            .await
    }
}
impl<
    R: RpcClient + 'static,
    P: Contracts,
    C: Fn() -> String,
    I: Fn() -> JsString,
    Q: SchedulerJournalBindings,
> SchedulerStore for LifecycleJournal<'_, '_, R, P, C, I, Q>
{
    fn free_bytes(&self) -> IvyResult<Option<f64>> {
        self.pages.free_bytes()
    }
    fn pending_operation(&self, task_id: &JsString) -> IvyResult<Option<JsString>> {
        self.journal
            .journal
            .pending_operation(task_id, "scheduler", None)
    }
}
