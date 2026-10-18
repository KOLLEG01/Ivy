//! Serialized assignment scheduler pass using the original per-assignment order.
use crate::{
    assignment_error::{SchedulerFailure, SchedulerResult},
    assignment_progress::SchedulerAssignments,
    assignment_scheduler::AssignmentScheduler,
    inbox_recovery::RecoveryScan,
    store::Record,
};
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString};
use ivy_sdk::{
    clock::{date_to_iso, parse_date},
    rpc::RequestOptions,
};

fn text(value: &IvyJson, key: &str) -> SchedulerResult<JsString> {
    value
        .required(key)?
        .as_js_string()
        .cloned()
        .ok_or(SchedulerFailure::Uncoded)
}
impl SchedulerAssignments {
    /// Called by the owning generation's serialized foreground chain.
    pub async fn tick(
        &mut self,
        scheduler: &AssignmentScheduler<'_, '_, '_>,
        issues: &mut RecoveryScan,
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
        let assignments = self.assignments(scheduler, issues).await?;
        let budget = (32 / assignments.len().max(1)).max(1);
        for assignment in assignments {
            let assignment_id = text(assignment.required("value")?, "assignmentId")?;
            let assignment_name = String::from_utf16_lossy(&assignment_id.0);
            let key = format!("assignment:{assignment_name}");
            let outcome: SchedulerResult<()> = async {
                let gap_issue = format!("event-gap:{assignment_name}");
                let gap = store
                    .journal
                    .named("secretary/event-gap", &assignment_name, true)?;
                if let Some(gap) = gap {
                    if gap.value.get("relevant").is_none() {
                        let first = store
                            .client
                            .request(
                                "events.read",
                                IvyJson::object([
                                    (
                                        "afterSequence",
                                        gap.value.required("prunedThroughSequence")?.clone(),
                                    ),
                                    ("filter", IvyJson::Object(vec![])),
                                    ("limit", IvyJson::Number(1.0)),
                                ]),
                                RequestOptions::default(),
                            )
                            .await?;
                        let original = store.original(Record::Assignment, &assignment).await?;
                        let written = original
                            .get("writtenAt")
                            .filter(|v| !v.is_null())
                            .unwrap_or(original.required("value")?.required("createdAt")?);
                        let first_event = first
                            .required("items")?
                            .as_array()
                            .ok_or(SchedulerFailure::Uncoded)?
                            .first();
                        let before_assignment = first.get("gap").is_none_or(IvyJson::is_null)
                            && first_event.is_some_and(|event| {
                                event
                                    .get("occurredAt")
                                    .and_then(IvyJson::as_js_string)
                                    .is_some_and(|at| {
                                        parse_date(at)
                                            < written
                                                .as_js_string()
                                                .map(parse_date)
                                                .unwrap_or(f64::NAN)
                                    })
                            });
                        if before_assignment {
                            store
                                .journal
                                .delete_technical("secretary/event-gap", &gap.pin.object_id)?;
                            issues.issues.retain(|(id, _)| id != &gap_issue);
                        } else {
                            let mut value = gap.value.clone();
                            value.insert("relevant", true.into());
                            store.journal.amend_technical(
                                "secretary/event-gap",
                                &gap.pin,
                                &value,
                            )?;
                            issues.issue(&gap_issue, "secretary_event_gap");
                        }
                    } else if gap.value.get("relevant") == Some(&IvyJson::Bool(true)) {
                        issues.issue(&gap_issue, "secretary_event_gap");
                    }
                }
                let configuration = configuration.required("value")?;
                if scheduler
                    .events_inner(&assignment, configuration, budget, issues)
                    .await?
                {
                    scheduler
                        .flush_due_message_windows_inner(&assignment, configuration, now)
                        .await?;
                    scheduler
                        .flush_due_object_windows_inner(&assignment, configuration, now)
                        .await?;
                }
                scheduler
                    .schedules_inner(&assignment, configuration, now)
                    .await?;
                issues.issues.retain(|(id, _)| id != &key);
                Ok(())
            }
            .await;
            if let Err(error) = outcome {
                if evidence.cancellation.cancellation.is_cancelled() {
                    return Err(IvyError::from(error));
                }
                issues.issue(&key, &error.code("secretary_assignment_failed"));
            }
        }
        Ok(())
    }
}
