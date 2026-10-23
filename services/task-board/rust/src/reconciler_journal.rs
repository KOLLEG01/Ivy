//! Recovery scans and cleanup use the same admitted TaskBoard journal.
use crate::{
    journal_store::Contracts,
    lifecycle_journal::LifecycleJournal,
    local_gates::{LocalGates, PendingGate},
    reconciler::ReconcileStore,
    scheduler_journal::SchedulerJournalBindings,
};
use ivy_contracts::{IvyResult, JsString};
use ivy_sdk::rpc::RpcClient;
use ivy_store::sqlite::error;
impl<
    R: RpcClient + 'static,
    P: Contracts,
    C: Fn() -> String,
    I: Fn() -> JsString,
    Q: SchedulerJournalBindings,
> ReconcileStore for LifecycleJournal<'_, '_, R, P, C, I, Q>
{
    fn pending_coordination_page(
        &self,
        after: Option<&JsString>,
        limit: i64,
    ) -> IvyResult<Vec<PendingGate>> {
        LocalGates::new(self.journal.journal.connection)
            .pending_page(after, limit)
            .map_err(error)
    }
    async fn collect_local(&self) -> IvyResult<()> {
        self.journal.collect_local().await
    }
}
