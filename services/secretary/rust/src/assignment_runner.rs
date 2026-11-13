//! Secretary-owned execution failure policy and retained native resource cleanup.
use crate::{inbox_evidence::InboxEvidence, journal::Journal, store::Record};
use ivy_contracts::{IvyJson, IvyResult};

const NATIVE_CALL: &str = "secretary/assignment-native-call";
const THREAD_LEASE: &str = "secretary/thread-lease";
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExecutionPhase {
    Queued,
    Preflight,
    Starting,
    Running,
    Completed,
    Settled,
    Archiving,
    Archived,
    Deleting,
    Deleted,
    Failed,
}
impl ExecutionPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Preflight => "preflight",
            Self::Starting => "starting",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Settled => "settled",
            Self::Archiving => "archiving",
            Self::Archived => "archived",
            Self::Deleting => "deleting",
            Self::Deleted => "deleted",
            Self::Failed => "failed",
        }
    }
    pub fn read(value: Option<&IvyJson>) -> Option<Self> {
        [
            Self::Queued,
            Self::Preflight,
            Self::Starting,
            Self::Running,
            Self::Completed,
            Self::Settled,
            Self::Archiving,
            Self::Archived,
            Self::Deleting,
            Self::Deleted,
            Self::Failed,
        ]
        .into_iter()
        .find(|phase| value.is_some_and(|v| v.is_str(phase.as_str())))
    }
    pub fn terminal(self) -> bool {
        matches!(self, Self::Settled | Self::Deleted | Self::Failed)
    }
    pub fn json(self) -> IvyJson {
        self.as_str().into()
    }
}
pub fn thread_lease_name(assignment: &IvyJson, target: &IvyJson) -> IvyResult<String> {
    let mut name = if assignment.required("execution")?.get("reuse") == Some(&"main".into()) {
        "main".to_owned()
    } else {
        format!(
            "assignment-{}",
            assignment.required("assignmentId")?.as_str()?
        )
    };
    if let Some(backend) = target.get("backend") {
        ivy_contracts::native_backend::BackendIdentity::from_json(backend)?;
        name.push('-');
        name.push_str(&ivy_contracts::hash::hash_json(backend)?[7..]);
    }
    Ok(name)
}

pub fn execution_failure_disposition(code: &str, attempts: f64, age_ms: f64) -> &'static str {
    let pending = matches!(
        code,
        "secretary_execution_native_pending" | "secretary_message_unread_pending"
    );
    if pending && age_ms < 30.0 * 60.0 * 1000.0 {
        return "pending";
    }
    if pending
        || attempts >= 5.0
        || matches!(
            code,
            "secretary_assignment_result_invalid"
                | "secretary_execution_answer_missing"
                | "secretary_execution_turn_missing"
                | "secretary_execution_turn_changed"
                | "secretary_execution_thread_missing"
                | "secretary_execution_reuse_target_changed"
                | "secretary_reuse_target_changed"
                | "secretary_preflight_outcome_unknown"
                | "secretary_preflight_host_mismatch"
                | "secretary_execution_native_failed"
                | "native_owner_mismatch"
                | "native_evidence_mismatch"
                | "native_evidence_regressed"
                | "tool_definition_changed"
                | "secretary_assignment_result_changed"
                | "secretary_assignment_notice_mismatch"
        )
    {
        "terminal"
    } else {
        "retry"
    }
}

/// Call only after the execution revision has been durably saved. Recovery keeps
/// native call bytes for precisely the phase that can still need that operation.
pub fn release_calls(journal: &Journal, execution: &IvyJson) -> IvyResult<()> {
    let phase_value = execution.required("phase")?;
    let phase = ExecutionPhase::read(Some(phase_value));
    if phase.is_none() {
        // This cleanup helper also accepts retained string phases outside the schema.
        phase_value.as_str()?;
    }
    let turn_started = execution
        .get("turnId")
        .and_then(IvyJson::as_js_string)
        .is_some_and(|s| !s.0.is_empty());
    let operations = execution.required("nativeOperations")?;
    for (key, needed) in [
        ("threadStart", phase == Some(ExecutionPhase::Starting)),
        (
            "turnStart",
            phase == Some(ExecutionPhase::Running) && !turn_started,
        ),
        ("archive", phase == Some(ExecutionPhase::Archiving)),
        ("delete", phase == Some(ExecutionPhase::Deleting)),
    ] {
        if needed {
            continue;
        }
        let Some(id) = operations.get(key).filter(|id| !id.is_null()) else {
            continue;
        };
        let id = id.as_str()?;
        if !id.is_empty() {
            if let Some(call) = journal.named(NATIVE_CALL, &id, true)? {
                journal.delete_technical(NATIVE_CALL, &call.pin.object_id)?;
            }
        }
    }
    Ok(())
}

/// A completed execution may release only its own reusable-thread lease.
pub fn release_lease(journal: &Journal, current: &IvyJson) -> IvyResult<()> {
    let execution = current.required("value")?;
    let assignment = execution.required("assignmentSnapshot")?;
    let reuse = assignment
        .get("execution")
        .and_then(|v| v.get("reuse"))
        .filter(|v| !v.is_null());
    if reuse.is_none() || reuse == Some(&"new".into()) {
        return Ok(());
    }
    let name = thread_lease_name(assignment, execution.required("executionTarget")?)?;
    if let Some(lease) = journal.named(THREAD_LEASE, &name, true)? {
        if lease.value.get("activeExecutionObjectId") == current.required("pin")?.get("objectId") {
            let mut next = lease.value.clone();
            if next.get("threadId").is_none_or(IvyJson::is_null) {
                next.insert("threadId", execution.required("threadId")?.clone());
            }
            next.insert("activeExecutionObjectId", IvyJson::Null);
            journal.amend_technical(THREAD_LEASE, &lease.pin, &next)?;
        }
    }
    Ok(())
}

pub struct AssignmentRunner<'a, 'engine, 'journal> {
    pub evidence: &'a InboxEvidence<'engine, 'journal>,
}
impl AssignmentRunner<'_, '_, '_> {
    pub async fn update(&self, current: &IvyJson, value: &IvyJson) -> IvyResult<IvyJson> {
        self.evidence.verify_owner().await?;
        let saved = self
            .evidence
            .store
            .amend(current, Record::Execution, value)
            .await?;
        release_calls(self.evidence.store.journal, saved.required("value")?)?;
        Ok(saved)
    }
}
