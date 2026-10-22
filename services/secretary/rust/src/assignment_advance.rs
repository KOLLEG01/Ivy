//! Ordered assignment phase advancement. Process execution belongs to the runtime.
use crate::{
    assignment_delivery::AssignmentDelivery,
    assignment_driver::{CollectedPreflight, PreflightWork},
    assignment_execution::AssignmentExecution,
    assignment_message::{message_batch, message_conversation_unread},
    assignment_runner::release_calls,
    inbox_recovery::{RecoveryIssues, RecoveryScan},
    store::Record,
};
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString};
use ivy_sdk::clock::parse_date;
use std::collections::HashSet;

fn phase(current: &IvyJson, expected: &str) -> bool {
    current.get("value").and_then(|v| v.get("phase")) == Some(&expected.into())
}
fn unknown(current: &IvyJson) -> bool {
    current
        .get("value")
        .and_then(|v| v.get("delivery"))
        .and_then(|v| v.get("state"))
        == Some(&"outcome_unknown".into())
}
fn reused(value: &IvyJson) -> bool {
    value
        .get("assignmentSnapshot")
        .and_then(|v| v.get("execution"))
        .and_then(|v| v.get("reuse"))
        .is_some_and(|v| !v.is_null() && v != &"new".into())
}
pub struct AssignmentAdvance<'a, 'b, 'engine, 'journal> {
    pub execution: &'a AssignmentExecution<'b, 'engine, 'journal>,
    pub minimum_notification_age_minutes: f64,
}
impl AssignmentAdvance<'_, '_, '_, '_> {
    /// Some(document) is the one newly fenced preflight job. The owning runtime
    /// must launch it with its Host process adapter and keep its ID in `preflight`
    /// until completion. A lost job remains fenced; it must never be replayed.
    pub async fn advance(
        &self,
        current: &IvyJson,
        preflight: &mut HashSet<JsString>,
        issues: &mut RecoveryScan,
    ) -> IvyResult<Option<IvyJson>> {
        let work = CollectedPreflight::new(preflight);
        self.advance_with_work(current, &work, &std::cell::RefCell::new(issues))
            .await?;
        Ok(work.jobs.into_inner().pop())
    }
    pub(crate) async fn advance_with_work(
        &self,
        current: &IvyJson,
        preflight: &dyn PreflightWork,
        issues: &dyn RecoveryIssues,
    ) -> IvyResult<()> {
        let execution = self.execution;
        let evidence = execution.native.evidence;
        let store = evidence.store;
        let tools = execution.native.tools.as_ref();
        let delivery = AssignmentDelivery {
            evidence,
            tools,
            clock: execution.clock,
            minimum_notification_age_minutes: self.minimum_notification_age_minutes,
        };
        let mut current = current.clone();
        release_calls(store.journal, current.required("value")?)?;
        if ["settled", "deleted", "failed"]
            .iter()
            .any(|p| phase(&current, p))
        {
            if phase(&current, "settled") && unknown(&current) {
                delivery.step_with_issues(&current, issues).await?;
            }
            return Ok(());
        }
        if phase(&current, "queued") {
            let value = current.required("value")?;
            let payload = value.required("trigger")?.required("payload")?;
            let batch = message_batch(payload);
            if payload.get("kind") == Some(&"secretary.message.batch".into()) && batch.is_none() {
                return Err(IvyError::new(
                    "secretary_message_unread_pending",
                    "The earlier message batch cannot be checked by this release.",
                ));
            }
            if let Some(batch) = batch {
                let due = batch
                    .required("dueAt")?
                    .as_js_string()
                    .ok_or_else(IvyError::internal)?;
                if (execution.clock.utc_millis() as f64) < parse_date(due) {
                    return Ok(());
                }
                if !message_conversation_unread(store.client.as_ref(), tools, &store.root, &batch)
                    .await?
                {
                    let now = execution.now()?;
                    let mut next = value.clone();
                    next.insert("phase", "settled".into());
                    next.insert("result", "{\"schemaVersion\":1,\"urgency\":\"low\",\"notification\":\"none\",\"text\":\"\",\"reason\":\"Conversation was read before the review.\"}".into());
                    next.insert("completedAt", IvyJson::String(now.clone()));
                    next.insert("updatedAt", IvyJson::String(now));
                    execution.update(&current, &next).await?;
                    return Ok(());
                }
            }
            let now = execution.now()?;
            let mut next = value.clone();
            let command = value
                .required("assignmentSnapshot")?
                .get("preflight")
                .is_some_and(|v| !v.is_null());
            next.insert(
                "phase",
                if command { "preflight" } else { "starting" }.into(),
            );
            next.insert("startedAt", IvyJson::String(now.clone()));
            next.insert("updatedAt", IvyJson::String(now));
            current = execution.update(&current, &next).await?;
        }
        if phase(&current, "preflight") {
            let id = current
                .required("value")?
                .required("executionId")?
                .as_js_string()
                .cloned()
                .ok_or_else(IvyError::internal)?;
            if preflight.contains(&id) {
                return Ok(());
            }
            current = store
                .read(
                    Record::Execution,
                    current.required("pin")?.required("objectId")?,
                )
                .await?;
            if !phase(&current, "preflight") {
                return Ok(());
            }
            let value = current.required("value")?;
            if value.required("preflightOutput")? != &IvyJson::Null {
                return Err(IvyError::new(
                    "secretary_preflight_outcome_unknown",
                    "The original preflight result needs review.",
                ));
            }
            if !preflight.is_empty() {
                return Ok(());
            }
            let target = execution.target(value).await?;
            let mut next = value.clone();
            if target.get("hostId") != evidence.settings.required("identity")?.get("hostId") {
                let now = execution.now()?;
                next.insert("phase", "failed".into());
                next.insert("errorCode", "secretary_preflight_host_mismatch".into());
                next.insert("completedAt", IvyJson::String(now.clone()));
                next.insert("updatedAt", IvyJson::String(now));
                execution.update(&current, &next).await?;
                return Ok(());
            }
            evidence.verify_owner().await?;
            next.insert("preflightOutput", "".into());
            next.insert("updatedAt", IvyJson::String(execution.now()?));
            current = execution.update(&current, &next).await?;
            preflight.admit(id, current).await;
            return Ok(());
        }
        if phase(&current, "starting") {
            current = execution.start(&current).await?;
        }
        if phase(&current, "running") {
            current = execution.run(&current).await?;
        }
        if phase(&current, "completed") && delivery.step_with_issues(&current, issues).await? {
            current = store
                .read(
                    Record::Execution,
                    current.required("pin")?.required("objectId")?,
                )
                .await?;
            let mut next = current.required("value")?.clone();
            next.insert(
                "phase",
                if reused(&next) {
                    "settled"
                } else {
                    "archiving"
                }
                .into(),
            );
            next.insert("updatedAt", IvyJson::String(execution.now()?));
            current = execution.update(&current, &next).await?;
        }
        if phase(&current, "archived") && unknown(&current) {
            delivery.step_with_issues(&current, issues).await?;
        }
        if phase(&current, "archiving") {
            current = execution.archive(&current).await?;
        }
        if phase(&current, "archived") {
            if let Some(due) = current
                .required("value")?
                .get("deleteAfter")
                .and_then(IvyJson::as_js_string)
                .filter(|s| !s.0.is_empty())
            {
                if parse_date(due) <= execution.clock.utc_millis() as f64 {
                    let mut next = current.required("value")?.clone();
                    next.insert("phase", "deleting".into());
                    next.insert("updatedAt", IvyJson::String(execution.now()?));
                    current = execution.update(&current, &next).await?;
                }
            }
        }
        if phase(&current, "deleting") {
            execution.remove(&current).await?;
        }
        Ok(())
    }
}
