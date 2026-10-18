//! Original-turn execution and retention transitions through the shared native owner.
use crate::{
    assignment_native::AssignmentNative,
    assignment_prompt::assignment_prompt,
    assignment_result::{assignment_completion, assignment_turn},
    assignment_runner::{AssignmentRunner, release_lease},
};
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString, checked_native_contract::CheckedNativeContracts,
    json::try_js_string,
};
use ivy_sdk::{
    clock::{Clock, date_to_iso},
    native_turn_contents::{read_native_turn_contents, verify_native_turn_contents},
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
impl AssignmentExecution<'_, '_, '_> {
    async fn update(&self, current: &IvyJson, value: &IvyJson) -> IvyResult<IvyJson> {
        AssignmentRunner {
            evidence: self.native.evidence,
        }
        .update(current, value)
        .await
    }
    fn now(&self) -> IvyResult<JsString> {
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
    pub async fn run(&self, current: &IvyJson) -> IvyResult<IvyJson> {
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
                Some(id) => try_js_string(id)?,
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
            return self.update(&current, &next).await;
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
            return Ok(current.clone());
        }
        let now = self.now()?;
        let items = if turn.get("status") == Some(&"completed".into()) {
            let contents =
                read_native_turn_contents(&owner, self.retained, &read, &thread_id, &turn_id, None)
                    .await?;
            verify_native_turn_contents(&owner, self.retained, &contents, &thread_id, &turn_id)
                .await?
                .items
        } else {
            vec![]
        };
        let next =
            assignment_completion(execution, turn, &items, &now).ok_or_else(IvyError::internal)?;
        let saved = self.update(current, &next).await?;
        release_lease(self.native.evidence.store.journal, &saved)?;
        Ok(saved)
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
        next.insert("phase", "archived".into());
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
        next.insert("phase", "deleted".into());
        next.insert("deletedAt", IvyJson::String(now.clone()));
        next.insert("updatedAt", IvyJson::String(now));
        self.update(&current, &next).await
    }
}
