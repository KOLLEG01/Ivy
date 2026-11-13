//! Original-turn execution and retention transitions through the shared native owner.
use crate::{
    assignment_decision::RecordedDecision,
    assignment_native::AssignmentNative,
    assignment_prompt::assignment_prompt,
    assignment_result::{assignment_completion, assignment_turn},
    assignment_runner::{AssignmentRunner, ExecutionPhase, release_lease, thread_lease_name},
    store::Record,
};
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString, checked_native_contract::CheckedNativeContracts,
};
use ivy_sdk::{
    clock::{Clock, date_to_iso},
    native_turn_contents::read_native_turn_contents,
    operation_ids::new_operation_id,
};

fn text(value: &IvyJson, key: &str) -> IvyResult<JsString> {
    value
        .required(key)?
        .as_js_string()
        .cloned()
        .ok_or_else(IvyError::internal)
}
fn nonempty(value: Option<&IvyJson>) -> bool {
    value
        .and_then(IvyJson::as_js_string)
        .is_some_and(|s| !s.0.is_empty())
}
pub struct AssignmentExecution<'a, 'engine, 'journal> {
    pub native: AssignmentNative<'a, 'engine, 'journal>,
    pub clock: &'a dyn Clock,
    pub retained: &'a CheckedNativeContracts,
    pub notification_policy: &'a JsString,
}
pub struct AssignmentRun {
    pub document: IvyJson,
    pub decision: Option<RecordedDecision>,
}
impl AssignmentExecution<'_, '_, '_> {
    pub async fn target(&self, execution: &IvyJson) -> IvyResult<IvyJson> {
        if let Some(target) = execution.get("nativeTarget").filter(|v| !v.is_null()) {
            return Ok(target.clone());
        }
        let selection = crate::agent_target::AgentSelection {
            service_node_id: text(execution, "serviceNodeId")?,
            backend: ivy_contracts::native_backend::BackendIdentity::optional(
                execution.required("executionTarget")?.get("backend"),
            )?,
        };
        Ok(selection.resolve(&self.native.tools).await?.to_json())
    }
    pub async fn start(&self, current: &IvyJson) -> IvyResult<IvyJson> {
        let execution = current.required("value")?;
        if execution.get("nativeTarget").is_none_or(IvyJson::is_null) {
            let target = self.target(execution).await?;
            let now = self.now()?;
            let mut next = execution.clone();
            next.insert("nativeTarget", target);
            next.insert("phase", ExecutionPhase::Starting.json());
            next.insert(
                "startedAt",
                execution
                    .get("startedAt")
                    .filter(|v| !v.is_null())
                    .cloned()
                    .unwrap_or(IvyJson::String(now.clone())),
            );
            next.insert("updatedAt", IvyJson::String(now));
            return self.update(current, &next).await;
        }
        let assignment = execution.required("assignmentSnapshot")?;
        let reuse = assignment
            .get("execution")
            .and_then(|v| v.get("reuse"))
            .filter(|v| !v.is_null());
        let journal = self.native.evidence.store.journal;
        let lease_key = "secretary/thread-lease";
        let mut lease = None;
        if reuse.is_some_and(|v| v != &"new".into()) {
            let name = thread_lease_name(assignment, execution.required("executionTarget")?)?;
            let active_id = current.required("pin")?.required("objectId")?;
            lease = Some(
                if let Some(saved) = journal.named(lease_key, &name, true)? {
                    if saved.value.get("serviceNodeId") != execution.get("serviceNodeId")
                        || saved.value.get("threadCwd")
                            != execution.required("executionTarget")?.get("threadCwd")
                    {
                        return Err(IvyError::new(
                            "secretary_reuse_target_changed",
                            "A reused Secretary thread belongs to a different execution target.",
                        ));
                    }
                    let mut thread_id = saved.value.required("threadId")?.clone();
                    if nonempty(saved.value.get("activeExecutionObjectId"))
                        && saved.value.get("activeExecutionObjectId") != Some(active_id)
                    {
                        match self
                            .native
                            .evidence
                            .store
                            .read(
                                Record::Execution,
                                saved.value.required("activeExecutionObjectId")?,
                            )
                            .await
                        {
                            Ok(prior) => {
                                let value = prior.required("value")?;
                                if !matches!(
                                    value.required("phase")?.as_str()?.as_str(),
                                    "settled" | "deleted" | "failed" | "completed" | "archived"
                                ) {
                                    return Err(IvyError::new(
                                        "secretary_execution_native_pending",
                                        "The reused Secretary thread is busy.",
                                    ));
                                }
                                if thread_id.is_null() {
                                    thread_id = value.required("threadId")?.clone();
                                }
                            }
                            Err(error) if error.code == "not_found" => {}
                            Err(error) => return Err(error),
                        }
                    }
                    let mut next = saved.value.clone();
                    if !nonempty(Some(&thread_id)) {
                        next.insert(
                            "createdByExecutionId",
                            execution.required("executionId")?.clone(),
                        );
                    }
                    next.insert("threadId", thread_id);
                    next.insert("activeExecutionObjectId", active_id.clone());
                    journal.amend_technical(lease_key, &saved.pin, &next)?
                } else {
                    journal.create_technical(
                        lease_key,
                        &name,
                        &IvyJson::object([
                            (
                                "serviceNodeId",
                                execution.required("serviceNodeId")?.clone(),
                            ),
                            (
                                "threadCwd",
                                execution
                                    .required("executionTarget")?
                                    .required("threadCwd")?
                                    .clone(),
                            ),
                            (
                                "createdByExecutionId",
                                execution.required("executionId")?.clone(),
                            ),
                            ("activeExecutionObjectId", active_id.clone()),
                            ("threadId", IvyJson::Null),
                        ]),
                    )?
                },
            );
        }
        let current = self.with_operation(current, "threadStart").await?;
        let execution = current.required("value")?;
        let prompt = assignment_prompt(execution, self.notification_policy)?;
        let creating = lease.as_ref().is_none_or(|lease| {
            lease.value.get("createdByExecutionId") == execution.get("executionId")
                && !nonempty(execution.get("threadId"))
        });
        let target = execution.required("executionTarget")?;
        let owner = self.native.owner(execution)?;
        let mut params = IvyJson::object([
            ("cwd", target.required("threadCwd")?.clone()),
            ("permissions", target.required("permissions")?.clone()),
            ("approvalPolicy", "never".into()),
            (
                "developerInstructions",
                prompt.required("developerInstructions")?.clone(),
            ),
        ]);
        let method = if creating {
            params.insert("ephemeral", IvyJson::Bool(false));
            params.insert("serviceName", "secretary".into());
            params.insert("threadSource", "ivy-secretary".into());
            if nonempty(target.get("model")) {
                params.insert("model", target.required("model")?.clone());
            }
            "thread/start"
        } else {
            params.insert(
                "threadId",
                lease.as_ref().unwrap().value.required("threadId")?.clone(),
            );
            params.insert("excludeTurns", IvyJson::Bool(true));
            "thread/resume"
        };
        let result = self
            .native
            .completed(
                &owner,
                &text(execution.required("nativeOperations")?, "threadStart")?,
                &method.into(),
                &params,
            )
            .await?;
        let thread_id = if creating {
            match result
                .get("reply")
                .and_then(|v| v.get("result"))
                .and_then(|v| v.get("thread"))
                .and_then(|v| v.get("id"))
                .filter(|v| !v.is_null())
            {
                Some(id) => {
                    IvyJson::String(id.as_js_string().cloned().ok_or_else(IvyError::internal)?)
                }
                None => "".into(),
            }
        } else {
            lease.as_ref().unwrap().value.required("threadId")?.clone()
        };
        if !nonempty(Some(&thread_id)) {
            return Err(IvyError::new(
                "secretary_execution_thread_missing",
                "The Secretary task start has no thread identity.",
            ));
        }
        let mut next = execution.clone();
        next.insert("threadId", thread_id.clone());
        next.insert("phase", ExecutionPhase::Running.json());
        next.insert("updatedAt", IvyJson::String(self.now()?));
        let current = self.update(&current, &next).await?;
        if let Some(lease) = lease.filter(|l| !nonempty(l.value.get("threadId"))) {
            let mut next = lease.value;
            next.insert("threadId", thread_id);
            journal.amend_technical(lease_key, &lease.pin, &next)?;
        }
        Ok(current)
    }
    pub(crate) async fn update(&self, current: &IvyJson, value: &IvyJson) -> IvyResult<IvyJson> {
        AssignmentRunner {
            evidence: self.native.evidence,
        }
        .update(current, value)
        .await
    }
    pub(crate) fn now(&self) -> IvyResult<JsString> {
        date_to_iso(self.clock.utc_millis() as f64)
    }
    pub async fn with_operation(&self, current: &IvyJson, key: &str) -> IvyResult<IvyJson> {
        let value = current.required("value")?;
        let mut operations = value.required("nativeOperations")?.clone();
        if nonempty(operations.get(key)) {
            return Ok(current.clone());
        }
        let id = new_operation_id(self.native.evidence.store.client.clone(), None).await?;
        let now = self.now()?;
        operations.insert(key, IvyJson::String(id));
        let mut next = value.clone();
        next.insert("nativeOperations", operations);
        next.insert("updatedAt", IvyJson::String(now));
        self.update(current, &next).await
    }
    pub async fn run(&self, current: &IvyJson) -> IvyResult<AssignmentRun> {
        let execution = current.required("value")?;
        if !nonempty(execution.get("threadId")) {
            return Err(IvyError::new(
                "secretary_execution_thread_missing",
                "The running Secretary execution has no task.",
            ));
        }
        if !nonempty(execution.get("turnId")) {
            let current = self.with_operation(current, "turnStart").await?;
            let execution = current.required("value")?;
            let target = execution.required("executionTarget")?;
            let owner = self.native.owner(execution)?;
            let mut params = IvyJson::object([
                ("threadId", execution.required("threadId")?.clone()),
                ("effort", target.required("effort")?.clone()),
                (
                    "input",
                    IvyJson::Array(vec![IvyJson::object([
                        ("type", "text".into()),
                        (
                            "text",
                            assignment_prompt(execution, self.notification_policy)?
                                .required("prompt")?
                                .clone(),
                        ),
                        ("text_elements", IvyJson::Array(vec![])),
                    ])]),
                ),
            ]);
            if nonempty(target.get("model")) {
                params.insert("model", target.required("model")?.clone());
            }
            let result = self
                .native
                .completed(
                    &owner,
                    &text(execution.required("nativeOperations")?, "turnStart")?,
                    &"turn/start".into(),
                    &params,
                )
                .await?;
            let id = result
                .get("reply")
                .and_then(|v| v.get("result"))
                .and_then(|v| v.get("turn"))
                .and_then(|v| v.get("id"))
                .filter(|v| !v.is_null());
            let turn_id = match id {
                Some(id) => id.as_js_string().cloned().ok_or_else(IvyError::internal)?,
                None => JsString::from(""),
            };
            if turn_id.0.is_empty() {
                return Err(IvyError::new(
                    "secretary_execution_turn_missing",
                    "The Secretary task start has no turn identity.",
                ));
            }
            let mut next = execution.clone();
            next.insert("turnId", IvyJson::String(turn_id));
            next.insert("updatedAt", IvyJson::String(self.now()?));
            return Ok(AssignmentRun {
                document: self.update(&current, &next).await?,
                decision: None,
            });
        }
        let owner = self.native.owner(execution)?;
        let status = owner.status().await?;
        if !nonempty(status.get("epoch")) {
            return Err(IvyError::new(
                "secretary_execution_native_pending",
                "The Secretary AgentManager is not ready.",
            ));
        }
        let epoch = text(&status, "epoch")?;
        let thread_id = text(execution, "threadId")?;
        let turn_id = text(execution, "turnId")?;
        let read = owner
            .read(
                &"thread/turns/list".into(),
                IvyJson::object([
                    ("threadId", IvyJson::String(thread_id.clone())),
                    ("cursor", IvyJson::Null),
                    ("limit", IvyJson::Number(1.0)),
                    ("sortDirection", "desc".into()),
                    ("itemsView", "notLoaded".into()),
                ]),
                Some(&epoch),
                false,
            )
            .await?;
        let turn = assignment_turn(execution, &read)?;
        if turn.get("status") == Some(&"inProgress".into()) {
            return Ok(AssignmentRun {
                document: current.clone(),
                decision: None,
            });
        }
        let now = self.now()?;
        let next = if turn.get("status") == Some(&"completed".into()) {
            let contents =
                read_native_turn_contents(&owner, self.retained, read, &thread_id, &turn_id, None)
                    .await?;
            assignment_completion(execution, contents.turn(), contents.final_answer(), &now)
        } else {
            assignment_completion(execution, turn, None, &now)
        }
        .ok_or_else(IvyError::internal)?;
        let saved = self.update(current, &next.execution).await?;
        release_lease(self.native.evidence.store.journal, &saved)?;
        let decision = next
            .decision
            .map(|decision| RecordedDecision::record(&saved, decision))
            .transpose()?;
        Ok(AssignmentRun {
            document: saved,
            decision,
        })
    }
    pub async fn archive(&self, current: &IvyJson) -> IvyResult<IvyJson> {
        if !nonempty(current.required("value")?.get("threadId")) {
            return Err(IvyError::new(
                "secretary_execution_thread_missing",
                "Secretary cannot archive an unidentified task.",
            ));
        }
        let current = self.with_operation(current, "archive").await?;
        let execution = current.required("value")?;
        let owner = self.native.owner(execution)?;
        self.native
            .completed(
                &owner,
                &text(execution.required("nativeOperations")?, "archive")?,
                &"thread/archive".into(),
                &IvyJson::object([("threadId", execution.required("threadId")?.clone())]),
            )
            .await?;
        let at = self.clock.utc_millis() as f64;
        let archived_at = date_to_iso(at)?;
        let delete_after = date_to_iso(at + 7.0 * 24.0 * 60.0 * 60.0 * 1000.0)?;
        let mut next = execution.clone();
        next.insert("phase", ExecutionPhase::Archived.json());
        crate::assignment_cleanup::CleanupAction::Archive.confirmed(&mut next, &archived_at);
        next.insert("archivedAt", IvyJson::String(archived_at.clone()));
        next.insert("deleteAfter", IvyJson::String(delete_after));
        next.insert("updatedAt", IvyJson::String(archived_at));
        self.update(&current, &next).await
    }
    pub async fn remove(&self, current: &IvyJson) -> IvyResult<IvyJson> {
        if !nonempty(current.required("value")?.get("threadId")) {
            return Err(IvyError::new(
                "secretary_execution_thread_missing",
                "Secretary cannot delete an unidentified task.",
            ));
        }
        let current = self.with_operation(current, "delete").await?;
        let execution = current.required("value")?;
        let owner = self.native.owner(execution)?;
        self.native
            .completed(
                &owner,
                &text(execution.required("nativeOperations")?, "delete")?,
                &"thread/delete".into(),
                &IvyJson::object([("threadId", execution.required("threadId")?.clone())]),
            )
            .await?;
        let now = self.now()?;
        let mut next = execution.clone();
        next.insert("phase", ExecutionPhase::Deleted.json());
        crate::assignment_cleanup::CleanupAction::Delete.confirmed(&mut next, &now);
        next.insert("deletedAt", IvyJson::String(now.clone()));
        next.insert("updatedAt", IvyJson::String(now));
        self.update(&current, &next).await
    }
}
