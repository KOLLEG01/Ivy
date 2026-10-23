//! Retain an automatic request before invoking it. Only conclusive unapplied
//! failure permits a successor; current wake and saved-context policy is mandatory.
use crate::{
    allocation::{AllocationRuntime, AllocationStore, Allocator},
    coordination::recovery_retry_delay_ms,
    inspection_journal::verify_owner,
    native_intent::TaskPrompts,
    operations::{Document, Operations, Store, mutation},
    wake::{WakeStore, comments_pending, next_execution_mode, queued_input, should_wake},
};
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString, hash::hash_json, json::canonical,
    limits::MANAGEMENT_FRAME_BYTES,
};
use ivy_sdk::{
    clock::date_to_iso, native_evidence::native_instant, operation_ids::scoped_operation_id,
    rpc::RpcClient, tools::ToolClients,
};
use std::{cmp::Ordering, sync::Arc};
#[allow(async_fn_in_trait)]
pub trait SchedulerStore: AllocationStore + WakeStore {
    fn free_bytes(&self) -> IvyResult<Option<f64>>;
    fn pending_operation(&self, task_id: &JsString) -> IvyResult<Option<JsString>>;
}
#[allow(async_fn_in_trait)]
pub trait SchedulerAllocation {
    async fn allocate(
        &self,
        task: &Document,
        operation: &JsString,
        feedback: Option<&JsString>,
        retain_workspace: bool,
    ) -> IvyResult<Json>;
}
impl<S: AllocationStore, E: AllocationRuntime> SchedulerAllocation for Allocator<'_, S, E> {
    async fn allocate(
        &self,
        t: &Document,
        o: &JsString,
        f: Option<&JsString>,
        retain_workspace: bool,
    ) -> IvyResult<Json> {
        Allocator::allocate(self, t, o, f, retain_workspace).await
    }
}
#[allow(async_fn_in_trait)]
pub trait SchedulerOperations {
    async fn find(&self, principal: &JsString, operation: &JsString)
    -> IvyResult<Option<Document>>;
}
impl<S: Store, C: Fn() -> String> SchedulerOperations for Operations<S, C> {
    async fn find(&self, p: &JsString, o: &JsString) -> IvyResult<Option<Document>> {
        Operations::find(self, p, o).await
    }
}
#[allow(async_fn_in_trait)]
pub trait SchedulerPolicies: TaskPrompts {
    async fn read_task(&self, id: &JsString) -> IvyResult<Document>;
    fn enabled(&self) -> bool;
    fn interval_ms(&self) -> f64;
    fn now(&self) -> JsString;
    fn compare_default(&self, left: &JsString, right: &JsString) -> Ordering;
    async fn aggregate(&self, task: &Json, id: &JsString) -> IvyResult<()>;
    async fn forward_delegated_review(&self, task: &Document) -> IvyResult<()>;
    async fn unstarted_missing_context(&self, task: &Document) -> IvyResult<bool>;
    async fn unstarted_archived_context(&self, task: &Document) -> IvyResult<bool>;
    async fn failed_rate_limit_retry_at(&self, task: &Document) -> IvyResult<Option<JsString>>;
    async fn schedule(&self, request: &Json) -> IvyResult<Json>;
    async fn recover_operation(&self, id: &JsString) -> IvyResult<Json>;
}
#[allow(async_fn_in_trait)]
pub trait SchedulerEngine: AllocationRuntime + SchedulerPolicies {
    async fn verify_owner(&self) -> IvyResult<()>;
    async fn scoped_operation_id(&self, scope: &Json) -> IvyResult<JsString>;
}
/// One admitted connection and explicit current engine policies; no default
/// prompt, child handoff, aggregate or automatic retry policy.
pub struct SchedulerRuntime<R, P> {
    pub client: Arc<R>,
    pub principal_id: JsString,
    pub service_node_id: JsString,
    pub policies: P,
    tools: Arc<ToolClients>,
}
impl<R: RpcClient + 'static, P> SchedulerRuntime<R, P> {
    pub fn new(
        client: Arc<R>,
        principal_id: JsString,
        service_node_id: JsString,
        policies: P,
    ) -> Self {
        Self {
            tools: Arc::new(ToolClients::new(client.clone())),
            client,
            principal_id,
            service_node_id,
            policies,
        }
    }
}
impl<R, P: TaskPrompts> TaskPrompts for SchedulerRuntime<R, P> {
    fn task_prompt(&self, t: &Json, o: &JsString, c: Option<&Json>) -> IvyResult<JsString> {
        self.policies.task_prompt(t, o, c)
    }
    fn task_update_prompt(&self, t: &Json, o: &JsString, c: bool) -> IvyResult<JsString> {
        self.policies.task_update_prompt(t, o, c)
    }
}
impl<R: RpcClient + 'static, P: SchedulerPolicies> AllocationRuntime for SchedulerRuntime<R, P> {
    fn client(&self) -> Arc<dyn RpcClient> {
        self.client.clone()
    }
    fn tools(&self) -> &Arc<ToolClients> {
        &self.tools
    }
    fn principal_id(&self) -> &JsString {
        &self.principal_id
    }
    fn service_node_id(&self) -> &JsString {
        &self.service_node_id
    }
    fn compare_default(&self, a: &JsString, b: &JsString) -> Ordering {
        self.policies.compare_default(a, b)
    }
}
impl<R: RpcClient + 'static, P: SchedulerPolicies> SchedulerPolicies for SchedulerRuntime<R, P> {
    async fn read_task(&self, id: &JsString) -> IvyResult<Document> {
        self.policies.read_task(id).await
    }
    fn enabled(&self) -> bool {
        self.policies.enabled()
    }
    fn interval_ms(&self) -> f64 {
        self.policies.interval_ms()
    }
    fn now(&self) -> JsString {
        self.policies.now()
    }
    fn compare_default(&self, a: &JsString, b: &JsString) -> Ordering {
        self.policies.compare_default(a, b)
    }
    async fn aggregate(&self, t: &Json, id: &JsString) -> IvyResult<()> {
        self.policies.aggregate(t, id).await
    }
    async fn forward_delegated_review(&self, t: &Document) -> IvyResult<()> {
        self.policies.forward_delegated_review(t).await
    }
    async fn unstarted_missing_context(&self, t: &Document) -> IvyResult<bool> {
        self.policies.unstarted_missing_context(t).await
    }
    async fn unstarted_archived_context(&self, t: &Document) -> IvyResult<bool> {
        self.policies.unstarted_archived_context(t).await
    }
    async fn failed_rate_limit_retry_at(&self, t: &Document) -> IvyResult<Option<JsString>> {
        self.policies.failed_rate_limit_retry_at(t).await
    }
    async fn schedule(&self, r: &Json) -> IvyResult<Json> {
        self.policies.schedule(r).await
    }
    async fn recover_operation(&self, id: &JsString) -> IvyResult<Json> {
        self.policies.recover_operation(id).await
    }
}
impl<R: RpcClient + 'static, P: SchedulerPolicies> SchedulerEngine for SchedulerRuntime<R, P> {
    async fn verify_owner(&self) -> IvyResult<()> {
        verify_owner(
            self.client.as_ref(),
            &self.principal_id,
            &self.service_node_id,
        )
        .await
    }
    async fn scoped_operation_id(&self, s: &Json) -> IvyResult<JsString> {
        scoped_operation_id(self.client.clone(), s).await
    }
}
pub struct Scheduler<'a, S, E, A, O> {
    pub store: &'a S,
    pub engine: &'a E,
    pub allocator: &'a A,
    pub operations: &'a O,
}
fn f<'a>(v: &'a Json, k: &str) -> &'a Json {
    static NULL: Json = Json::Null;
    v.get(k).unwrap_or(&NULL)
}
fn s<'a>(v: &'a Json, k: &str) -> IvyResult<&'a JsString> {
    v.required(k)?.as_js_string().ok_or_else(IvyError::internal)
}
fn n(v: &Json, k: &str) -> IvyResult<f64> {
    v.get(k)
        .and_then(Json::as_f64)
        .filter(|value| value.fract() == 0.0 && (1.0..=9007199254740991.0).contains(value))
        .ok_or_else(IvyError::internal)
}
fn eq(v: &Json, k: &str, t: &str) -> bool {
    v.get(k) == Some(&Json::from(t))
}
fn present(v: &Json) -> bool {
    !v.is_null()
}
fn arr(v: &Json) -> &[Json] {
    v.as_array().unwrap_or(&[])
}
fn require(ok: bool, c: &str, m: &str) -> IvyResult<()> {
    if ok { Ok(()) } else { Err(IvyError::new(c, m)) }
}
fn same(a: &Json, b: &Json) -> IvyResult<bool> {
    Ok(canonical(a, MANAGEMENT_FRAME_BYTES)? == canonical(b, MANAGEMENT_FRAME_BYTES)?)
}
fn delay(interval: f64, code: Option<&str>) -> IvyResult<i64> {
    require(
        interval.is_finite() && (0.0..=9007199254740991.0).contains(&interval),
        "invalid_arguments",
        "The scheduler interval must be a finite nonnegative duration.",
    )?;
    Ok((interval.ceil() as i64).max(recovery_retry_delay_ms(code)))
}
fn concat(prefix: &str, value: &JsString, suffix: &str) -> JsString {
    let mut s = JsString::from(prefix);
    s.0.extend(&value.0);
    s.0.extend(suffix.encode_utf16());
    s
}
pub fn rate_limit_waiting_detail(at: &JsString) -> JsString {
    concat(
        "Agent temporarily unavailable; automatic continuation after ",
        at,
        ".",
    )
}
impl<S: SchedulerStore, E: SchedulerEngine, A: SchedulerAllocation, O: SchedulerOperations>
    Scheduler<'_, S, E, A, O>
{
    async fn proposed(
        &self,
        task: &Document,
        sequence: f64,
        recover: bool,
        retry: Option<&JsString>,
    ) -> IvyResult<Option<Json>> {
        let value = &task.value;
        let at = self.engine.now();
        let operation = self
            .engine
            .scoped_operation_id(&Json::Array(vec![
                "task-board-scheduler".into(),
                task.pin.required("objectId")?.clone(),
                task.pin.required("revision")?.clone(),
                Json::Number(sequence),
            ]))
            .await?;
        let base = Json::object([
            ("operationId", Json::String(operation)),
            ("taskId", task.pin.required("objectId")?.clone()),
            ("expectedRevision", task.pin.required("revision")?.clone()),
        ]);
        if !next_execution_mode(value, &at) {
            return Ok(None);
        }
        if recover {
            let mut r = base;
            r.insert("action", "recoverThread".into());
            r.insert("run", value.required("lastRun")?.clone());
            return Ok(Some(r));
        }
        if !should_wake(self.store, value, &at).await? {
            return Ok(None);
        }
        let comments = arr(f(value, "commentDeliveries"))
            .iter()
            .filter(|delivery| {
                eq(delivery, "state", "queued")
                    && arr(f(value, "comments")).iter().any(|comment| {
                        f(comment, "commentId") == f(delivery, "commentId") && queued_input(comment)
                    })
            })
            .take(256)
            .map(|delivery| delivery.required("commentId").cloned())
            .collect::<IvyResult<Vec<_>>>()?;
        let feedback = self
            .engine
            .task_prompt(value, self.engine.service_node_id(), None)?;
        let allocation = self
            .allocator
            .allocate(
                task,
                s(&base, "operationId")?,
                Some(&feedback),
                comments_pending(value)
                    || f(value, "executionChangeRequested").as_bool() == Some(true),
            )
            .await;
        let allocation = match allocation {
            Ok(allocation) => allocation,
            Err(error) => {
                let reason = match error.code.as_str() {
                    "task_board_project_unavailable"
                    | "task_board_workspace_mismatch"
                    | "task_board_workspace_conflict"
                    | "task_board_workspace_reserved"
                    | "target_conflict"
                    | "not_found" => "workspace",
                    "task_board_capability_unavailable" => "capability",
                    _ => "host",
                };
                let mut detail = error.message_js();
                detail.0.truncate(2048);
                let previous = f(value, "runtimeWait");
                if f(previous, "retryAt")
                    .as_js_string()
                    .is_some_and(|retry| retry > &at)
                    && f(previous, "detail") == &Json::String(detail.clone())
                {
                    return Ok(None);
                }
                let retry_at = match retry {
                    Some(retry) => retry.clone(),
                    None => date_to_iso(
                        (native_instant(&at, None)?
                            + recovery_retry_delay_ms(Some(error.code.as_str())))
                            as f64,
                    )?,
                };
                let wait = Json::object([
                    ("reason", reason.into()),
                    ("detail", Json::String(detail)),
                    (
                        "since",
                        Json::String(f(previous, "since").as_js_string().cloned().unwrap_or(at)),
                    ),
                    ("retryAt", Json::String(retry_at)),
                ]);
                let mut request = base;
                request.insert("action", "defer".into());
                request.insert("runtimeWait", wait);
                return Ok(Some(request));
            }
        };
        let mut request = base;
        request.insert(
            "action",
            if present(f(value, "primaryResourceRef")) {
                "continue"
            } else {
                "start"
            }
            .into(),
        );
        if present(f(value, "primaryResourceRef")) {
            request.insert("feedback", Json::String(feedback));
        }
        for (key, value) in allocation.as_object().ok_or_else(IvyError::internal)? {
            request.insert(key.clone(), value.clone());
        }
        request.insert("commentIds", Json::Array(comments));
        Ok(Some(request))
    }
    async fn verify_attempt(&self, document: &Document, task: &Document) -> IvyResult<()> {
        let value = &document.value;
        let request = value.required("request")?;
        require(
            document.parent_id.as_ref() == Some(s(&task.pin, "objectId")?)
                && same(f(value, "task"), &task.pin)?
                && value.get("principalId").and_then(Json::as_js_string)
                    == Some(self.engine.principal_id())
                && request.get("taskId") == task.pin.get("objectId")
                && request.get("expectedRevision") == task.pin.get("revision")
                && value.get("requestHash") == Some(&Json::from(hash_json(request)?)),
            "task_board_identity_conflict",
            "Automatic attempt must retain its exact pinned task, principal and original request.",
        )?;
        if n(value, "sequence")? > 1.0 {
            require(
                present(f(value, "previous")) && present(f(value, "previousOperation")),
                "task_board_identity_conflict",
                "A successor needs both original predecessor pins.",
            )?;
            let previous = self
                .store
                .read(
                    "task-board/scheduler-attempt",
                    value.required("previous")?,
                    false,
                )
                .await?;
            let operation = self
                .store
                .read(
                    "task-board/operation",
                    value.required("previousOperation")?,
                    true,
                )
                .await?;
            require(
                previous.pin.get("objectId") == document.pin.get("objectId")
                    && n(&previous.pin, "revision")? < n(&document.pin, "revision")?
                    && n(&previous.value, "sequence")? + 1.0 == n(value, "sequence")?
                    && same(f(&previous.value, "task"), f(value, "task"))?
                    && previous.value.get("requestHash")
                        == Some(&Json::from(hash_json(previous.value.required("request")?)?))
                    && previous.value.get("principalId") == value.get("principalId")
                    && operation.value.get("callerPrincipalId") == value.get("principalId")
                    && operation.value.get("requestHash") == previous.value.get("requestHash")
                    && operation.value.get("operationId")
                        == f(&previous.value, "request").get("operationId"),
                "task_board_identity_conflict",
                "Scheduler succession must retain the exact previous attempt and its original Operation.",
            )?;
            self.retryable(&operation)?;
            require(
                native_instant(s(value, "preparedAt")?, None)?
                    >= native_instant(s(&operation.value, "updatedAt")?, None)?
                        + delay(
                            self.engine.interval_ms(),
                            f(f(&operation.value, "error"), "code")
                                .as_js_string()
                                .map(JsString::to_utf8)
                                .transpose()?
                                .as_deref(),
                        )?,
                "task_board_retry_delay",
                "A successor must retain the minimum delay after its conclusive predecessor failure.",
            )?;
        }
        Ok(())
    }
    fn retryable(&self, operation: &Document) -> IvyResult<()> {
        require(
            eq(&operation.value, "phase", "failed")
                && eq(f(&operation.value, "error"), "execution", "not_executed")
                && arr(f(&operation.value, "writes"))
                    .iter()
                    .all(|w| f(w, "outcome") == &Json::Null),
            "task_board_outcome_unresolved",
            "Only a conclusively failed, unapplied original operation permits another automatic attempt.",
        )
    }
    pub async fn task(&self, task_id: &JsString) -> IvyResult<Json> {
        self.engine.verify_owner().await?;
        if !self.engine.enabled() {
            return Ok(Json::Null);
        }
        let task = self.engine.read_task(task_id).await?;
        let value = &task.value;
        if let Some(pending) = self.store.pending_operation(task_id)? {
            return self.engine.recover_operation(&pending).await;
        }
        self.engine.forward_delegated_review(&task).await?;
        if present(f(f(value, "claim"), "run")) && !present(f(value, "publication")) {
            let run = self
                .store
                .read(
                    "task-board/run",
                    f(f(f(value, "claim"), "run"), "objectId"),
                    true,
                )
                .await?;
            if ["completed", "failed", "cancelled"]
                .iter()
                .any(|phase| eq(&run.value, "phase", phase))
            {
                self.engine.aggregate(value, task_id).await?;
                if present(f(&run.value, "result")) {
                    let result = self
                        .store
                        .read("task-board/result", f(&run.value, "result"), true)
                        .await?;
                    if let Some(original) = self
                        .operations
                        .find(self.engine.principal_id(), s(&result.value, "operationId")?)
                        .await?
                    {
                        if !["succeeded", "failed"]
                            .iter()
                            .any(|phase| eq(&original.value, "phase", phase))
                        {
                            self.engine
                                .recover_operation(s(&original.pin, "objectId")?)
                                .await?;
                            return Ok(Json::Null);
                        }
                    }
                }
                return Err(IvyError::new(
                    "task_board_claim_inconsistent",
                    "The claimed Run is terminal, but its original completion publication has not released this claim. Reconcile its retained evidence; do not start another execution.",
                ));
            }
        }
        let at = self.engine.now();
        if !next_execution_mode(value, &at) || !should_wake(self.store, value, &at).await? {
            return Ok(Json::Null);
        }
        let recover = self.engine.unstarted_missing_context(&task).await?;
        let restore = !recover && self.engine.unstarted_archived_context(&task).await?;
        let retry = if !recover && !restore {
            self.engine.failed_rate_limit_retry_at(&task).await?
        } else {
            None
        };
        if retry.as_ref().is_some_and(|retry| retry > &at) {
            return Ok(Json::Null);
        }
        if recover || restore {
            let run = self
                .store
                .read("task-board/run", value.required("lastRun")?, true)
                .await?;
            if native_instant(&at, None)?
                < native_instant(s(&run.value, "finishedAt")?, None)?
                    + delay(self.engine.interval_ms(), None)?
            {
                return Ok(Json::Null);
            }
        }
        require(
            self.store
                .free_bytes()?
                .is_none_or(|n| n >= 512.0 * 1024.0 * 1024.0),
            "task_board_storage_low",
            "New agent work is paused until the TaskBoard journal has at least 512 MiB free.",
        )?;
        let name = format!(
            "Scheduled task {} revision {}",
            task_id.to_utf8()?,
            n(&task.pin, "revision")?
        );
        let mut attempt = self
            .store
            .named("task-board/scheduler-attempt", &name, Some(task_id))
            .await?;
        let mut previous_operation = None;
        if let Some(saved) = &attempt {
            self.verify_attempt(saved, &task).await?;
            let operation = self
                .operations
                .find(
                    self.engine.principal_id(),
                    s(saved.value.required("request")?, "operationId")?,
                )
                .await?;
            let Some(operation) = operation else {
                return self.engine.schedule(saved.value.required("request")?).await;
            };
            require(
                operation.value.get("requestHash") == saved.value.get("requestHash"),
                "task_board_identity_conflict",
                "The original scheduler Operation must match its prepared intent.",
            )?;
            if eq(&operation.value, "phase", "succeeded") {
                return Ok(operation.value.required("outcome")?.clone());
            }
            if !eq(&operation.value, "phase", "failed") {
                return self
                    .engine
                    .recover_operation(s(&operation.pin, "objectId")?)
                    .await;
            }
            self.retryable(&operation)?;
            if native_instant(&self.engine.now(), None)?
                < native_instant(s(&operation.value, "updatedAt")?, None)?
                    + delay(
                        self.engine.interval_ms(),
                        f(f(&operation.value, "error"), "code")
                            .as_js_string()
                            .map(JsString::to_utf8)
                            .transpose()?
                            .as_deref(),
                    )?
            {
                return Ok(Json::Null);
            }
            previous_operation = Some(operation);
        }
        let sequence = attempt
            .as_ref()
            .map(|d| n(&d.value, "sequence"))
            .transpose()?
            .unwrap_or(0.0)
            + 1.0;
        let Some(request) = self
            .proposed(&task, sequence, recover, retry.as_ref())
            .await?
        else {
            return Ok(Json::Null);
        };
        let next = Json::object([
            ("schemaVersion", Json::Number(1.0)),
            (
                "principalId",
                Json::String(self.engine.principal_id().clone()),
            ),
            ("task", task.pin.clone()),
            ("sequence", Json::Number(sequence)),
            ("request", request.clone()),
            ("requestHash", hash_json(&request)?.into()),
            (
                "previous",
                attempt
                    .as_ref()
                    .map(|d| d.pin.clone())
                    .unwrap_or(Json::Null),
            ),
            (
                "previousOperation",
                previous_operation.map(|d| d.pin).unwrap_or(Json::Null),
            ),
            ("preparedAt", Json::String(self.engine.now())),
        ]);
        self.engine.verify_owner().await?;
        let write = self
            .store
            .write(
                "task-board/scheduler-attempt",
                &next,
                &mutation(
                    &name.clone().into(),
                    &match &attempt {
                        Some(d) => format!(
                            "successor:{}",
                            hash_json(&Json::object([
                                ("pin", d.pin.clone()),
                                ("next", next.clone())
                            ]))?
                        ),
                        None => "prepare".into(),
                    },
                )?,
                &match &attempt {
                    Some(d) => Json::object([
                        ("objectId", d.pin.required("objectId")?.clone()),
                        ("expectedRevision", d.pin.required("revision")?.clone()),
                    ]),
                    None => Json::object([(
                        "create",
                        Json::object([
                            ("parentId", Json::String(task_id.clone())),
                            ("name", name.clone().into()),
                        ]),
                    )]),
                },
            )
            .await;
        match write {
            Ok(pin) => {
                attempt = Some(
                    self.store
                        .read("task-board/scheduler-attempt", &pin, false)
                        .await?,
                )
            }
            Err(error) => {
                if !["mutation_conflict", "revision_conflict"].contains(&error.code.as_str()) {
                    return Err(error);
                }
                attempt = self
                    .store
                    .named("task-board/scheduler-attempt", &name, Some(task_id))
                    .await?;
                require(
                    attempt.is_some(),
                    "task_board_identity_conflict",
                    "The competing prepared scheduler action must remain available.",
                )?;
            }
        }
        let attempt = attempt.unwrap();
        self.verify_attempt(&attempt, &task).await?;
        self.store.complete_local_workflow(
            s(&attempt.pin, "objectId")?,
            s(&attempt.value, "preparedAt")?,
        )?;
        self.engine
            .schedule(attempt.value.required("request")?)
            .await
    }
}
