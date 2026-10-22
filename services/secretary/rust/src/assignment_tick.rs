//! Serialized assignment scheduler pass using the original per-assignment order.
use crate::{
    assignment_progress::SchedulerAssignments,
    assignment_scheduler::AssignmentScheduler,
    inbox_recovery::{RecoveryIssues, RecoveryScan},
    store::Record,
};
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString};
use ivy_sdk::clock::date_to_iso;
use std::cell::RefCell;

fn text(value: &IvyJson, key: &str) -> IvyResult<JsString> {
    value
        .required(key)?
        .as_js_string()
        .cloned()
        .ok_or_else(IvyError::internal)
}
impl SchedulerAssignments {
    /// Called by the owning generation's serialized foreground chain.
    pub async fn tick(
        &mut self,
        scheduler: &AssignmentScheduler<'_, '_, '_>,
        issues: &mut RecoveryScan,
    ) -> IvyResult<()> {
        self.tick_with_issues(scheduler, &RefCell::new(issues))
            .await
    }
    /// Share diagnostics with independent generation workers without retaining
    /// an exclusive RefCell borrow while an RPC is pending.
    pub async fn tick_shared(
        &mut self,
        scheduler: &AssignmentScheduler<'_, '_, '_>,
        issues: &RefCell<RecoveryScan>,
    ) -> IvyResult<()> {
        self.tick_with_issues(scheduler, issues).await
    }
    async fn tick_with_issues(
        &mut self,
        scheduler: &AssignmentScheduler<'_, '_, '_>,
        issues: &dyn RecoveryIssues,
    ) -> IvyResult<()> {
        let evidence = scheduler.evidence;
        let store = evidence.store;
        evidence.verify_owner().await?;
        let mut configuration = scheduler.configuration().await?;
        let now = scheduler.clock.utc_millis();
        if configuration
            .required("value")?
            .get("execution")
            .is_none_or(IvyJson::is_null)
        {
            if let Some(execution) = scheduler.default_execution().await? {
                let mut value = configuration.required("value")?.clone();
                value.insert("execution", execution);
                value.insert("updatedAt", IvyJson::String(date_to_iso(now as f64)?));
                configuration = store
                    .amend(&configuration, Record::Configuration, &value)
                    .await?;
            }
        }
        let assignments = self.assignments_with_issues(scheduler, issues).await?;
        let budget = (32 / assignments.len().max(1)).max(1);
        for assignment in assignments {
            let assignment_id = text(assignment.required("value")?, "assignmentId")?;
            let assignment_name = String::from_utf16_lossy(&assignment_id.0);
            let key = format!("assignment:{assignment_name}");
            let outcome: IvyResult<()> = async {
                let gap_issue = format!("event-gap:{assignment_name}");
                let gap = store
                    .journal
                    .named("secretary/event-gap", &assignment_name, true)?;
                if let Some(gap) = gap {
                    if gap.value.get("relevant") == Some(&IvyJson::Bool(true)) {
                        issues.issue(&gap_issue, "secretary_event_gap");
                    }
                }
                let configuration = configuration.required("value")?;
                if scheduler
                    .events_inner(&assignment, configuration, budget, issues)
                    .await?
                {
                    scheduler
                        .flush_due_message_windows(&assignment, configuration, now)
                        .await?;
                    scheduler
                        .flush_due_object_windows(&assignment, configuration, now)
                        .await?;
                }
                scheduler.schedules(&assignment, configuration, now).await?;
                issues.clear(&key);
                Ok(())
            }
            .await;
            if let Err(error) = outcome {
                if evidence.cancellation.cancellation.is_cancelled() {
                    return Err(error);
                }
                issues.issue(&key, &error.code);
            }
        }
        Ok(())
    }
}
