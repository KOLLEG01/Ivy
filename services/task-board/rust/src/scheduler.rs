//! Retain an automatic request before invoking it. Only conclusive unapplied
//! failure permits a successor; current user/coordination policy is mandatory.
use crate::{
    allocation::{AllocationRuntime, AllocationStore, Allocator},
    inspection_journal::verify_owner,
    native_intent::TaskPrompts,
    operations::{Document, Operations, Store, mutation},
};
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString, hash::hash_json, json::canonical,
    limits::MANAGEMENT_FRAME_BYTES,
};
use ivy_sdk::{
    clock::parse_date, operation_ids::scoped_operation_id, rpc::RpcClient, tools::ToolClients,
};
use std::{cmp::Ordering, sync::Arc};
#[allow(async_fn_in_trait)]
pub trait SchedulerStore: AllocationStore {
    fn free_bytes(&self) -> IvyResult<Option<f64>>;
}
#[allow(async_fn_in_trait)]
pub trait SchedulerAllocation {
    async fn allocate(
        &self,
        task: &Document,
        operation: &JsString,
        feedback: Option<&JsString>,
    ) -> IvyResult<Json>;
}
impl<S: AllocationStore, E: AllocationRuntime> SchedulerAllocation for Allocator<'_, S, E> {
    async fn allocate(&self, t: &Document, o: &JsString, f: Option<&JsString>) -> IvyResult<Json> {
        Allocator::allocate(self, t, o, f).await
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
    fn enabled(&self) -> bool;
    fn interval_ms(&self) -> f64;
    fn now(&self) -> JsString;
    fn compare_default(&self, left: &JsString, right: &JsString) -> Ordering;
    fn coordination_pending(&self, task: &Json) -> IvyResult<bool>;
    fn peer_comment(&self, comment: &Json) -> IvyResult<bool>;
    async fn forward_delegated_review(&self, task: &Document) -> IvyResult<()>;
    async fn unstarted_missing_context(&self, task: &Document) -> IvyResult<bool>;
    async fn unstarted_archived_context(&self, task: &Document) -> IvyResult<bool>;
    async fn failed_rate_limit_retry_at(&self, task: &Document) -> IvyResult<Option<JsString>>;
    async fn check_dependencies(
        &self,
        fields: &Json,
        task: &JsString,
        require_done: bool,
    ) -> IvyResult<()>;
    async fn check_blocker(&self, blocker: &Json) -> IvyResult<()>;
    async fn schedule(&self, request: &Json) -> IvyResult<Json>;
    async fn recover_operation(&self, id: &JsString) -> IvyResult<Json>;
}
#[allow(async_fn_in_trait)]
pub trait SchedulerEngine: AllocationRuntime + SchedulerPolicies {
    fn principal_id(&self) -> &JsString;
    async fn verify_owner(&self) -> IvyResult<()>;
    async fn scoped_operation_id(&self, scope: &Json) -> IvyResult<JsString>;
}
/// One admitted connection and explicit current engine policies; no default
/// question, prompt, child handoff, blocker, check or automatic retry policy.
pub struct SchedulerRuntime<R, P> {
    pub client: Arc<R>,
    pub principal_id: JsString,
    pub service_node_id: JsString,
    pub policies: P,
    tools: ToolClients,
}
impl<R: RpcClient + 'static, P> SchedulerRuntime<R, P> {
    pub fn new(
        client: Arc<R>,
        principal_id: JsString,
        service_node_id: JsString,
        policies: P,
    ) -> Self {
        Self {
            tools: ToolClients::new(client.clone()),
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
    fn tools(&self) -> &ToolClients {
        &self.tools
    }
    fn service_node_id(&self) -> &JsString {
        &self.service_node_id
    }
    fn compare_default(&self, a: &JsString, b: &JsString) -> Ordering {
        self.policies.compare_default(a, b)
    }
}
impl<R: RpcClient + 'static, P: SchedulerPolicies> SchedulerPolicies for SchedulerRuntime<R, P> {
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
    fn coordination_pending(&self, t: &Json) -> IvyResult<bool> {
        self.policies.coordination_pending(t)
    }
    fn peer_comment(&self, c: &Json) -> IvyResult<bool> {
        self.policies.peer_comment(c)
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
    async fn check_dependencies(&self, f: &Json, t: &JsString, d: bool) -> IvyResult<()> {
        self.policies.check_dependencies(f, t, d).await
    }
    async fn check_blocker(&self, b: &Json) -> IvyResult<()> {
        self.policies.check_blocker(b).await
    }
    async fn schedule(&self, r: &Json) -> IvyResult<Json> {
        self.policies.schedule(r).await
    }
    async fn recover_operation(&self, id: &JsString) -> IvyResult<Json> {
        self.policies.recover_operation(id).await
    }
}
impl<R: RpcClient + 'static, P: SchedulerPolicies> SchedulerEngine for SchedulerRuntime<R, P> {
    fn principal_id(&self) -> &JsString {
        &self.principal_id
    }
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
fn n(v: &Json, k: &str) -> f64 {
    v.get(k).and_then(Json::as_f64).unwrap_or(f64::NAN)
}
fn eq(v: &Json, k: &str, t: &str) -> bool {
    v.get(k) == Some(&Json::from(t))
}
fn truthy(v: &Json) -> bool {
    match v {
        Json::Null => false,
        Json::Bool(v) => *v,
        Json::String(v) => !v.0.is_empty(),
        Json::Number(v) => *v != 0.0 && !v.is_nan(),
        _ => true,
    }
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
fn delay(interval: f64) -> f64 {
    if interval.is_nan() {
        interval
    } else {
        interval.max(30000.0)
    }
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
        let fields = value.required("fields")?;
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
        if recover {
            let mut r = base;
            r.insert("action", "recoverThread".into());
            r.insert("run", value.required("lastRun")?.clone());
            return Ok(Some(r));
        }
        if let Some(retry) = retry.filter(|t| **t > self.engine.now()) {
            let next = fields
                .get("nextReviewAt")
                .and_then(Json::as_js_string)
                .filter(|t| !t.0.is_empty() && *t > retry)
                .unwrap_or(retry);
            let mut r = base;
            r.insert("action", "defer".into());
            r.insert("reason", "time".into());
            r.insert("nextReviewAt", Json::String(next.clone()));
            r.insert("detail", Json::String(rate_limit_waiting_detail(next)));
            return Ok(Some(r));
        }
        let coordination = self.engine.coordination_pending(value)?;
        let mut comments = vec![];
        for delivery in arr(f(value, "commentDeliveries")) {
            if !eq(delivery, "state", "queued") {
                continue;
            }
            let id = delivery.required("commentId")?;
            let mut peer = false;
            if coordination {
                for c in arr(f(value, "comments")) {
                    if c.get("commentId") == Some(id) && self.engine.peer_comment(c)? {
                        peer = true;
                        break;
                    }
                }
            }
            if !coordination || peer {
                comments.push(id.clone());
            }
        }
        let feedback = self.engine.task_prompt(
            value,
            self.engine.service_node_id(),
            Some(&Json::Bool(coordination)),
        )?;
        let allocation: IvyResult<Json> = async {
            if !coordination {
                require(
                    !truthy(f(fields, "nextReviewAt"))
                        || s(fields, "nextReviewAt")? <= &self.engine.now(),
                    "task_board_time_waiting",
                    "The task is deferred until its next review time.",
                )?;
                self.engine
                    .check_dependencies(fields, s(&task.pin, "objectId")?, true)
                    .await?;
                self.engine.check_blocker(f(value, "blocker")).await?;
            }
            self.allocator
                .allocate(
                    task,
                    s(&base, "operationId")?,
                    truthy(f(value, "primaryResourceRef")).then_some(&feedback),
                )
                .await
        }
        .await;
        let allocation = match allocation {
            Ok(a) => a,
            Err(error) => {
                let reason = match error.code.as_str() {
                    "task_board_time_waiting" => "time",
                    "task_board_dependency_waiting" => "dependency",
                    "task_board_capability_unavailable" => "capability",
                    "task_board_target_unavailable"
                    | "task_board_model_unavailable"
                    | "native_catalog_mismatch"
                    | "service_not_ready"
                    | "service_unavailable" => "host",
                    "not_found"
                    | "target_conflict"
                    | "task_board_project_unavailable"
                    | "task_board_project_mismatch"
                    | "task_board_workspace_reserved"
                    | "task_board_workspace_mismatch" => "workspace",
                    "task_board_input_required" => "user",
                    _ => return Err(error),
                };
                let mut detail = error.message_js();
                detail.0.truncate(2048);
                if ![
                    "task_board_model_unavailable",
                    "task_board_capability_unavailable",
                ]
                .contains(&error.code.as_str())
                    && reason == "time"
                    && truthy(f(fields, "nextReviewAt"))
                {
                    let at = s(fields, "nextReviewAt")?;
                    let retained = rate_limit_waiting_detail(at);
                    detail = if f(f(value, "waiting"), "detail") == &Json::String(retained.clone())
                    {
                        retained
                    } else {
                        concat("Deferred until ", at, ".")
                    };
                }
                let blocker = if reason == "workspace" {
                    error
                        .details
                        .as_ref()
                        .and_then(|v| v.get("blockerTaskId"))
                        .and_then(Json::as_js_string)
                        .map(|id| {
                            Json::object([
                                ("taskId", Json::String(id.clone())),
                                ("until", "idle".into()),
                            ])
                        })
                } else {
                    None
                };
                if coordination {
                    return Ok(None);
                }
                if eq(value, "workflowState", "waiting")
                    && eq(f(value, "waiting"), "reason", reason)
                    && f(f(value, "waiting"), "detail") == &Json::String(detail.clone())
                    && same(
                        f(value, "blocker"),
                        blocker.as_ref().unwrap_or(f(value, "blocker")),
                    )?
                {
                    return Ok(None);
                }
                let mut r = base;
                r.insert("action", "defer".into());
                r.insert("reason", reason.into());
                r.insert("detail", Json::String(detail));
                r.insert("nextReviewAt", fields.required("nextReviewAt")?.clone());
                if let Some(b) = blocker {
                    r.insert("blocker", b);
                }
                return Ok(Some(r));
            }
        };
        require(
            truthy(&allocation),
            "task_board_target_unavailable",
            "No resolved execution allocation is available.",
        )?;
        let mut request = base;
        request.insert(
            "action",
            if truthy(f(value, "primaryResourceRef")) {
                "continue"
            } else {
                "start"
            }
            .into(),
        );
        if truthy(f(value, "primaryResourceRef")) {
            request.insert("feedback", Json::String(feedback));
        }
        for (k, v) in allocation.as_object().unwrap_or(&[]) {
            request.insert(k.clone(), v.clone());
        }
        request.insert("commentIds", Json::Array(comments));
        if truthy(f(value, "primaryResourceRef")) && coordination {
            request.insert("coordinationOnly", true.into());
        }
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
        if n(value, "sequence") > 1.0 {
            require(
                truthy(f(value, "previous")) && truthy(f(value, "previousOperation")),
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
                    && n(&previous.pin, "revision") < n(&document.pin, "revision")
                    && n(&previous.value, "sequence") + 1.0 == n(value, "sequence")
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
                parse_date(s(value, "preparedAt")?)
                    >= parse_date(s(&operation.value, "updatedAt")?)
                        + delay(self.engine.interval_ms()),
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
        let task = self
            .store
            .read("task-board/task", &Json::String(task_id.clone()), true)
            .await?;
        let value = &task.value;
        self.engine.forward_delegated_review(&task).await?;
        if !eq(f(value, "fields"), "control", "agent")
            || truthy(f(value, "claim"))
            || truthy(f(value, "publication"))
            || (!["todo", "waiting"]
                .iter()
                .any(|s| eq(value, "workflowState", s))
                && !self.engine.coordination_pending(value)?)
            || (truthy(f(value, "waiting"))
                && !self.engine.coordination_pending(value)?
                && ![
                    "user",
                    "time",
                    "dependency",
                    "host",
                    "capability",
                    "workspace",
                ]
                .iter()
                .any(|s| eq(f(value, "waiting"), "reason", s)))
        {
            return Ok(Json::Null);
        }
        let fields = value.required("fields")?;
        let recovery_allowed = !truthy(f(value, "blocker"))
            && (!truthy(f(fields, "nextReviewAt"))
                || s(fields, "nextReviewAt")? <= &self.engine.now())
            && (!truthy(f(value, "waiting")) || eq(f(value, "waiting"), "reason", "user"));
        let recover = recovery_allowed && self.engine.unstarted_missing_context(&task).await?;
        let restore = recovery_allowed
            && !recover
            && eq(f(value, "waiting"), "reason", "user")
            && self.engine.unstarted_archived_context(&task).await?;
        let retry = if recovery_allowed
            && !recover
            && !restore
            && eq(f(value, "waiting"), "reason", "user")
        {
            self.engine.failed_rate_limit_retry_at(&task).await?
        } else {
            None
        };
        if recover || restore {
            let run = self
                .store
                .read("task-board/run", value.required("lastRun")?, true)
                .await?;
            if parse_date(&self.engine.now())
                < parse_date(s(&run.value, "finishedAt")?) + delay(self.engine.interval_ms())
            {
                return Ok(Json::Null);
            }
        } else if eq(f(value, "waiting"), "reason", "user")
            && !self.engine.coordination_pending(value)?
            && retry.is_none()
        {
            return Ok(Json::Null);
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
            n(&task.pin, "revision")
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
            if parse_date(&self.engine.now())
                < parse_date(s(&operation.value, "updatedAt")?) + delay(self.engine.interval_ms())
            {
                return Ok(Json::Null);
            }
            previous_operation = Some(operation);
        }
        let sequence = attempt
            .as_ref()
            .map(|d| n(&d.value, "sequence"))
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
