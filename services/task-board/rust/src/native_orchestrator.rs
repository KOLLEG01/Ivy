//! One bounded reconciliation pass over retained native calls and observations.
//! Current engine admission, prompts and handoff policy remain explicit.
use crate::{
    coordination::recovery_retry_delay_ms,
    engine_read::peer_state,
    journal_store::ReadScopes,
    native_driver::{NativeDriver, NativeDriverConnection, NativeDriverRuntime, NativeDriverStore},
    native_inspector::{Inspection, InspectionContext, InspectionStore, NativeInspector, PageSize},
    native_intent::{NativeSlot, TaskPrompts},
    native_lifecycle::LifecycleStore,
    native_progress::NativeProgress,
    native_state::{finished_run, project_native_state},
    native_turn_evidence::{TurnEvidence, TurnEvidenceStore},
    operations::{Document, Store, mutation},
    turn_output::{final_output_text, output_params, page_cursor},
    wake::{WakeStore, dependencies_changed, dependency_snapshot},
};
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString,
    checked_native_contract::CheckedNativeContracts, hash::hash_json, json::canonical,
    limits::MANAGEMENT_FRAME_BYTES,
};
use ivy_sdk::native_observations::NativeObservations;
use std::{cell::RefCell, collections::HashSet, future::Future};

/// The owning engine must bind its current nativeUpdate, read-cache scope,
/// peer observation and current policy. The runtime below supplies the concrete
/// journal/read-scope bindings without introducing policy defaults.
#[allow(async_fn_in_trait)]
pub trait OrchestrationEngine: NativeDriverRuntime {
    fn now_millis(&self) -> f64;
    fn run_handoff(&self, task: &Json, run: &JsString) -> IvyResult<bool>;
    fn claimed_workflow_state(&self, waiting: &Json) -> IvyResult<JsString>;
    async fn peer_state(&self, task: &Json, task_id: &JsString) -> IvyResult<JsString>;
    async fn dependency_snapshot(&self, task: &Json) -> IvyResult<Json>;
    async fn recover_pending_native_operation(
        &self,
        task: &JsString,
        run: &JsString,
    ) -> IvyResult<bool>;
    async fn sync_native_comments(&self, run: &JsString) -> IvyResult<()>;
    async fn with_read_cache<T, F: Future<Output = IvyResult<T>>>(&self, work: F) -> IvyResult<T>;
}
/// These decisions belong to the current engine. In particular, native comment
/// mirroring must use its accepted current question/answer admission path.
#[allow(async_fn_in_trait)]
pub trait OrchestrationPolicies {
    async fn recover_pending_native_operation(
        &self,
        task: &JsString,
        run: &JsString,
    ) -> IvyResult<bool>;
    async fn sync_native_comments(&self, run: &JsString) -> IvyResult<()>;
}
/// Borrow the authenticated driver and the owner's lifecycle journal. Bind the
/// journal's cache to these same read scopes when constructing the owner graph.
pub struct OrchestrationRuntime<'a, D, S, P, C> {
    pub driver: &'a D,
    pub store: &'a S,
    pub policies: &'a P,
    pub read_scopes: &'a ReadScopes,
    pub now_millis: C,
}
impl<D: NativeDriverRuntime, S, P, C> NativeDriverRuntime for OrchestrationRuntime<'_, D, S, P, C> {
    async fn verify_owner(&self) -> IvyResult<()> {
        self.driver.verify_owner().await
    }
    async fn scoped_operation_id(&self, scope: &Json) -> IvyResult<JsString> {
        self.driver.scoped_operation_id(scope).await
    }
    async fn native_update(&self, request: &Json) -> IvyResult<Json> {
        self.driver.native_update(request).await
    }
    fn now(&self) -> JsString {
        self.driver.now()
    }
}
impl<D: NativeDriverConnection, S, P, C> NativeDriverConnection
    for OrchestrationRuntime<'_, D, S, P, C>
{
    fn tools(&self) -> &std::sync::Arc<ivy_sdk::tools::ToolClients> {
        self.driver.tools()
    }
    fn principal_id(&self) -> &JsString {
        self.driver.principal_id()
    }
}
impl<
    D: NativeDriverRuntime,
    S: LifecycleStore + WakeStore,
    P: OrchestrationPolicies,
    C: Fn() -> f64,
> OrchestrationEngine for OrchestrationRuntime<'_, D, S, P, C>
{
    fn now_millis(&self) -> f64 {
        (self.now_millis)()
    }
    fn run_handoff(&self, task: &Json, run: &JsString) -> IvyResult<bool> {
        Ok(crate::engine_read::conversation::run_handoff(task, run).is_some())
    }
    fn claimed_workflow_state(&self, waiting: &Json) -> IvyResult<JsString> {
        Ok(crate::engine_read::condition_policy::claimed_workflow_state(waiting))
    }
    async fn peer_state(&self, task: &Json, task_id: &JsString) -> IvyResult<JsString> {
        peer_state(self.store, task, task_id).await
    }
    async fn dependency_snapshot(&self, task: &Json) -> IvyResult<Json> {
        dependency_snapshot(self.store, task).await
    }
    async fn recover_pending_native_operation(
        &self,
        task: &JsString,
        run: &JsString,
    ) -> IvyResult<bool> {
        self.policies
            .recover_pending_native_operation(task, run)
            .await
    }
    async fn sync_native_comments(&self, run: &JsString) -> IvyResult<()> {
        self.policies.sync_native_comments(run).await
    }
    async fn with_read_cache<T, F: Future<Output = IvyResult<T>>>(&self, work: F) -> IvyResult<T> {
        self.read_scopes.with_read_cache(work).await
    }
}
/// Component boundaries retain the original ordering. The concrete adapter
/// below uses the existing driver, inspector and immutable evidence verifier.
#[allow(async_fn_in_trait)]
pub trait OrchestrationServices {
    async fn recover_uncertain_resume(&self, run: &JsString) -> IvyResult<bool>;
    async fn prepare(
        &self,
        run: &JsString,
        slot: NativeSlot,
        origin: Option<&Json>,
    ) -> IvyResult<Json>;
    async fn advance(&self, run: &JsString, slot: NativeSlot) -> IvyResult<Json>;
    async fn status(&self, run: &Json, call: &Json) -> IvyResult<Json>;
    async fn find(&self, run: &Json) -> IvyResult<Inspection>;
    async fn owner_stopped(&self, run: &Json) -> IvyResult<bool>;
    async fn context(&self, run: &Json) -> IvyResult<InspectionContext>;
    async fn read_output(&self, context: &InspectionContext, params: Json) -> IvyResult<Json>;
    async fn snapshot(&self, run: &Json, artifact: &Json) -> IvyResult<Json>;
    async fn save_snapshot(&self, run: &Json, metadata: &Json, turns: &Json) -> IvyResult<Json>;
    async fn save_output(
        &self,
        run: &Json,
        snapshot: &Json,
        cursor: Option<&JsString>,
        observed: &Json,
    ) -> IvyResult<Json>;
}
/// Keep these components on the journal's admitted authenticated connection.
pub struct NativeServices<'a, D, I, R, P> {
    pub driver: NativeDriver<'a, D, R, P>,
    pub inspector: NativeInspector<'a, I>,
    pub evidence: TurnEvidence<'a, D>,
}
impl<
    D: NativeDriverStore + TurnEvidenceStore,
    I: InspectionStore,
    R: NativeDriverConnection,
    P: TaskPrompts,
> OrchestrationServices for NativeServices<'_, D, I, R, P>
{
    async fn recover_uncertain_resume(&self, r: &JsString) -> IvyResult<bool> {
        self.driver.recover_uncertain_resume(r).await
    }
    async fn prepare(&self, r: &JsString, s: NativeSlot, o: Option<&Json>) -> IvyResult<Json> {
        self.driver.prepare(r, s, o).await
    }
    async fn advance(&self, r: &JsString, s: NativeSlot) -> IvyResult<Json> {
        self.driver.advance(r, s).await
    }
    async fn status(&self, r: &Json, c: &Json) -> IvyResult<Json> {
        self.driver.status(r, c).await
    }
    async fn find(&self, r: &Json) -> IvyResult<Inspection> {
        self.inspector.find(r, 8.0, PageSize::Hundred).await
    }
    async fn owner_stopped(&self, r: &Json) -> IvyResult<bool> {
        self.inspector.owner_stopped(r).await
    }
    async fn context(&self, r: &Json) -> IvyResult<InspectionContext> {
        self.inspector.context(r).await
    }
    async fn read_output(&self, c: &InspectionContext, p: Json) -> IvyResult<Json> {
        self.inspector
            .read(c, &"thread/items/list".into(), p, true)
            .await
    }
    async fn snapshot(&self, r: &Json, a: &Json) -> IvyResult<Json> {
        Ok(self.evidence.snapshot(r, a).await?.document.value)
    }
    async fn save_snapshot(&self, r: &Json, m: &Json, t: &Json) -> IvyResult<Json> {
        self.evidence.save_snapshot(r, m, t).await
    }
    async fn save_output(
        &self,
        r: &Json,
        s: &Json,
        c: Option<&JsString>,
        o: &Json,
    ) -> IvyResult<Json> {
        self.evidence.save_output(r, s, c, o).await
    }
}
pub struct NativeOrchestrator<'a, S, E, V> {
    pub store: &'a S,
    pub engine: &'a E,
    pub services: &'a V,
    pub catalogs: &'a CheckedNativeContracts,
    progress: NativeProgress,
    threads: RefCell<Vec<(JsString, JsString, JsString)>>,
    retries: RefCell<Vec<(JsString, f64)>>,
}
fn field<'a>(v: &'a Json, k: &str) -> &'a Json {
    static NULL: Json = Json::Null;
    v.get(k).unwrap_or(&NULL)
}
fn equal(v: &Json, k: &str, s: &str) -> bool {
    v.get(k) == Some(&Json::from(s))
}
fn s<'a>(v: &'a Json, k: &str) -> IvyResult<&'a JsString> {
    v.required(k)?.as_js_string().ok_or_else(IvyError::internal)
}
fn array(v: &Json) -> &[Json] {
    v.as_array().unwrap_or(&[])
}
fn same(a: &Json, b: &Json) -> IvyResult<bool> {
    Ok(canonical(a, MANAGEMENT_FRAME_BYTES)? == canonical(b, MANAGEMENT_FRAME_BYTES)?)
}
fn require(ok: bool, code: &str, message: &str) -> IvyResult<()> {
    if ok {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
impl<'a, S: Store, E: OrchestrationEngine, V: OrchestrationServices>
    NativeOrchestrator<'a, S, E, V>
{
    pub fn new(
        store: &'a S,
        engine: &'a E,
        services: &'a V,
        catalogs: &'a CheckedNativeContracts,
        progress: NativeProgress,
    ) -> Self {
        Self {
            store,
            engine,
            services,
            catalogs,
            progress,
            threads: RefCell::default(),
            retries: RefCell::default(),
        }
    }
    pub fn notification(&self, value: &Json) -> bool {
        let params = field(value, "params");
        let event = field(params, "payload");
        let body = field(event, "params");
        let relevant = [
            "turn/completed",
            "thread/status/changed",
            "serverRequest/resolved",
        ]
        .iter()
        .any(|v| equal(event, "method", v));
        let activity = field(event, "method").as_js_string().is_some_and(|method| {
            method.0.starts_with(&[
                b'i' as u16,
                b't' as u16,
                b'e' as u16,
                b'm' as u16,
                b'/' as u16,
            ])
        }) || ["turn/started", "turn/completed", "serverRequest/resolved"]
            .iter()
            .any(|method| equal(event, "method", method));
        if !relevant && !activity {
            return false;
        }
        let mut matched = false;
        for (run, node, thread) in self.threads.borrow().iter() {
            if params.get("serviceNodeId").and_then(Json::as_js_string) == Some(node)
                && body.get("threadId").and_then(Json::as_js_string) == Some(thread)
            {
                if activity {
                    self.progress.activity(run, self.engine.now_millis());
                }
                if relevant {
                    self.retries.borrow_mut().retain(|r| &r.0 != run);
                    matched = true;
                }
            }
        }
        matched
    }
    pub fn diagnostics(&self) -> IvyResult<Vec<Json>> {
        self.progress.diagnostics(self.engine.now_millis())
    }
    fn postpone(&self, run: &JsString, code: Option<&str>) {
        let mut retries = self.retries.borrow_mut();
        let prior = retries.iter().find(|v| &v.0 == run);
        if retries.len() >= 1024 && prior.is_none() {
            retries.remove(0);
        }
        let after = self.engine.now_millis() + recovery_retry_delay_ms(code) as f64;
        if let Some(entry) = retries.iter_mut().find(|v| &v.0 == run) {
            *entry = (run.clone(), after);
        } else {
            retries.push((run.clone(), after));
        }
    }
    pub async fn drain(&self, run: &JsString) -> IvyResult<bool> {
        if self
            .retries
            .borrow()
            .iter()
            .find(|v| &v.0 == run)
            .map(|v| v.1)
            .unwrap_or(0.0)
            > self.engine.now_millis()
        {
            return Ok(false);
        }
        let engine = self.engine;
        engine.with_read_cache(self.drain_pass(run)).await?;
        Ok(true)
    }
    async fn drain_pass(&self, run: &JsString) -> IvyResult<()> {
        for _ in 0..8 {
            let before = self
                .store
                .read("task-board/run", &Json::String(run.clone()), true)
                .await?;
            if let Err(error) = self.step(run).await {
                self.postpone(run, Some(error.code.as_str()));
                return Err(error);
            }
            let after = self
                .store
                .read("task-board/run", &Json::String(run.clone()), true)
                .await?;
            let value = &after.value;
            if finished_run(value) {
                self.threads.borrow_mut().retain(|v| &v.0 != run);
                self.retries.borrow_mut().retain(|v| &v.0 != run);
            } else if field(value, "primaryResourceRef").as_object().is_some() {
                let node = s(value.required("target")?, "serviceNodeId")?.clone();
                let thread = s(value.required("primaryResourceRef")?, "nativeId")?.clone();
                let mut threads = self.threads.borrow_mut();
                if threads.len() >= 1024 {
                    threads.remove(0);
                }
                if let Some(entry) = threads.iter_mut().find(|v| &v.0 == run) {
                    *entry = (run.clone(), node, thread);
                } else {
                    threads.push((run.clone(), node, thread));
                }
            }
            let waiting = if equal(value, "phase", "starting") {
                self.store
                    .read("task-board/task", value.required("taskId")?, true)
                    .await?
                    .value
                    .get("runtimeWait")
                    .cloned()
                    .unwrap_or(Json::Null)
            } else {
                Json::Null
            };
            if equal(value, "phase", "outcome_unknown") || equal(&waiting, "reason", "host") {
                let code = field(field(value, "externalOutcome"), "code")
                    .as_js_string()
                    .map(JsString::to_utf8)
                    .transpose()?;
                self.postpone(run, code.as_deref());
            } else {
                self.retries.borrow_mut().retain(|v| &v.0 != run);
            }
            if !["starting", "publishing_result", "cancel_requested"]
                .iter()
                .any(|v| equal(value, "phase", v))
                && !self.unresolved(&after, NativeSlot::Steer).await?
                && !self.unresolved(&after, NativeSlot::Thread).await?
                || same(&before.pin, &after.pin)?
            {
                break;
            }
        }
        Ok(())
    }
    async fn final_output(&self, run: &Document, snapshot: &Json) -> IvyResult<Option<Json>> {
        let context = self.services.context(&run.pin).await?;
        let mut visited = HashSet::new();
        let mut cursor: Option<JsString> = None;
        for _ in 0..3 {
            require(
                visited.insert(cursor.clone()),
                "task_board_native_cursor_loop",
                "The native output search repeated a cursor.",
            )?;
            let observed = self
                .services
                .read_output(
                    &context,
                    output_params(&context.thread_id, &context.turn_id, cursor.as_ref()),
                )
                .await?;
            let reply = observed.required("reply")?;
            if field(reply, "error").get("code").and_then(Json::as_f64) == Some(-32601.0) {
                return Ok(None);
            }
            require(
                reply.get("result").is_some(),
                "task_board_native_read_failed",
                "The final output read returned its original error.",
            )?;
            let contract = self.catalogs.checked_native_contract(
                &context.native_version,
                Some(&Json::object([(
                    "catalogHash",
                    observed.required("catalogHash")?.clone(),
                )])),
            )?;
            let items = NativeObservations::new(&contract)
                .native_turn_items(reply.required("result")?, &context.turn_id)?;
            require(
                items.len() <= 1,
                "task_board_native_page_limit",
                "The final output read exceeds its one-item bound.",
            )?;
            if items
                .first()
                .is_some_and(|v| !final_output_text(v).0.is_empty())
            {
                return Ok(Some(
                    self.services
                        .save_output(&run.pin, snapshot, cursor.as_ref(), &observed)
                        .await?,
                ));
            }
            cursor = page_cursor(reply.required("result")?.get("nextCursor"))?;
            if cursor.is_none() {
                return Ok(None);
            }
        }
        Ok(None)
    }
    async fn update(&self, run: &Document, task: &Document, change: Json) -> IvyResult<()> {
        let operation = mutation(
            s(&run.pin, "objectId")?,
            &format!(
                "reconcile:{}",
                hash_json(&Json::object([
                    ("run", run.pin.clone()),
                    ("task", task.pin.clone()),
                    ("change", change.clone())
                ]))?
            ),
        )?;
        self.engine
            .native_update(&Json::object([
                ("action", "nativeUpdate".into()),
                ("operationId", operation.into()),
                ("taskId", task.pin.required("objectId")?.clone()),
                ("expectedRevision", task.pin.required("revision")?.clone()),
                ("run", run.pin.clone()),
                ("change", change),
            ]))
            .await?;
        Ok(())
    }
    async fn unresolved(&self, run: &Document, slot: NativeSlot) -> IvyResult<bool> {
        let pin = field(field(&run.value, "calls"), slot.as_str());
        if pin.is_null() {
            return Ok(false);
        }
        let latest = self
            .store
            .read("task-board/native-call", pin.required("objectId")?, true)
            .await?;
        let revision = |pin: &Json| {
            pin.required("revision")?
                .as_f64()
                .ok_or_else(IvyError::internal)
        };
        Ok(revision(&latest.pin)? > revision(pin)?
            || !["succeeded", "failed"]
                .iter()
                .any(|v| equal(&latest.value, "ownerPhase", v)))
    }
    pub async fn step(&self, run_id: &JsString) -> IvyResult<()> {
        self.engine.verify_owner().await?;
        let run = self
            .store
            .read("task-board/run", &Json::String(run_id.clone()), true)
            .await?;
        if finished_run(&run.value) {
            self.progress.forget(run_id);
            return Ok(());
        }
        // A partially published native update owns its original Task/Run pins.
        // Recover it before reading the Task or preparing another update.
        if self
            .engine
            .recover_pending_native_operation(s(&run.value, "taskId")?, run_id)
            .await?
        {
            return Ok(());
        }
        let task = self
            .store
            .read("task-board/task", run.value.required("taskId")?, true)
            .await?;
        let active = field(field(&task.value, "claim"), "run")
            .get("objectId")
            .and_then(Json::as_js_string)
            == Some(run_id);
        require(
            field(&task.value, "publication").is_null()
                && (!active || same(field(field(&task.value, "claim"), "run"), &run.pin)?),
            "revision_conflict",
            "Reconciliation requires current Task/Run publication state.",
        )?;
        let result = self.step_run(run_id, &run, &task, active).await;
        let Err(error) = result else {
            return Ok(());
        };
        if ![
            "not_found",
            "service_not_ready",
            "service_unavailable",
            "native_unavailable",
            "task_board_native_unavailable",
            "task_board_native_outcome_unavailable",
            "task_board_turn_unavailable",
            "task_board_native_read_failed",
            "task_board_native_epoch_changed",
            "task_board_native_search_limit",
            "task_board_native_collection_limit",
        ]
        .contains(&error.code.as_str())
        {
            return Err(error);
        }
        let mut detail = JsString::from("Native reconciliation is waiting: ");
        detail.0.extend(error.code_js().0);
        detail.0.extend(". ".encode_utf16());
        detail.0.extend(error.message_js().0.into_iter().take(2048));
        let outcome = field(&run.value, "externalOutcome");
        if !active
            && ["succeeded", "failed", "cancelled"]
                .iter()
                .any(|v| equal(outcome, "state", v))
        {
            return Err(error);
        }
        if equal(outcome, "state", "unknown")
            && outcome.get("code") == Some(&Json::String(error.code_js()))
            && (!active
                || equal(&task.value, "workflowState", "in_progress")
                    && equal(field(&task.value, "claim"), "phase", "outcome_unknown")
                    && field(&task.value, "runtimeWait").is_null())
        {
            return Ok(());
        }
        let waiting = field(&task.value, "runtimeWait");
        if waiting.get("detail").and_then(Json::as_js_string) == Some(&detail)
            && (!active
                || task.value.get("workflowState").and_then(Json::as_js_string)
                    == Some(&self.engine.claimed_workflow_state(waiting)?))
        {
            return Ok(());
        }
        self.update(
            &run,
            &task,
            Json::object([
                ("kind", "unavailable".into()),
                (
                    "reason",
                    if [
                        "service_not_ready",
                        "service_unavailable",
                        "native_unavailable",
                        "task_board_native_unavailable",
                    ]
                    .contains(&error.code.as_str())
                    {
                        "host"
                    } else {
                        "external_outcome"
                    }
                    .into(),
                ),
                ("code", Json::String(error.code_js())),
                ("detail", Json::String(detail)),
            ]),
        )
        .await
    }
    async fn step_run(
        &self,
        id: &JsString,
        run: &Document,
        task: &Document,
        active: bool,
    ) -> IvyResult<()> {
        let value = &run.value;
        let calls = field(value, "calls");
        if field(value, "turnId").is_null() {
            for slot in [NativeSlot::Thread, NativeSlot::Turn] {
                if self.unresolved(run, slot).await? {
                    let result: IvyResult<()> = async {
                        if slot == NativeSlot::Thread
                            && self.services.recover_uncertain_resume(id).await?
                        {
                            return Ok(());
                        }
                        self.services.advance(id, slot).await?;
                        Ok(())
                    }
                    .await;
                    if let Err(error) = result {
                        if error.code != "task_board_reattach_required"
                            || slot != NativeSlot::Turn
                            || !active
                            || !field(value, "cancellation").is_null()
                            || field(calls, "thread").is_null()
                        {
                            return Err(error);
                        }
                        let predecessor = self
                            .store
                            .read("task-board/native-call", calls.required("thread")?, true)
                            .await?;
                        let status = self.services.status(value, &predecessor.value).await?;
                        self.services
                            .prepare(
                                id,
                                NativeSlot::Thread,
                                Some(&Json::object([
                                    ("kind", "reattach".into()),
                                    ("predecessor", predecessor.pin),
                                    ("preparedEpoch", status.required("epoch")?.clone()),
                                ])),
                            )
                            .await?;
                    }
                    return Ok(());
                }
            }
            if !field(value, "cancellation").is_null() || !active {
                if field(calls, "turn").is_null() {
                    self.update(
                        run,
                        task,
                        Json::object([(
                            "kind",
                            if !field(value, "cancellation").is_null() {
                                "cancelUnstarted"
                            } else {
                                "retireUnstarted"
                            }
                            .into(),
                        )]),
                    )
                    .await?;
                }
                return Ok(());
            }
            self.services
                .prepare(
                    id,
                    if !field(calls, "thread").is_null() {
                        NativeSlot::Turn
                    } else {
                        NativeSlot::Thread
                    },
                    None,
                )
                .await?;
            return Ok(());
        }
        if self.unresolved(run, NativeSlot::Steer).await? {
            self.services.advance(id, NativeSlot::Steer).await?;
            return Ok(());
        }
        if self.unresolved(run, NativeSlot::Thread).await? {
            if !self.services.recover_uncertain_resume(id).await? {
                self.services.advance(id, NativeSlot::Thread).await?;
            }
            return Ok(());
        }
        let (metadata, turns) = match self.services.find(&run.pin).await? {
            Inspection::Pending { .. } => return Ok(()),
            Inspection::Found { metadata, turns } => (metadata, turns),
            Inspection::Absent => {
                return Err(IvyError::new(
                    "task_board_turn_unavailable",
                    "The original native turn is currently unavailable; its identity and outcome remain unresolved.",
                ));
            }
        };
        require(
            field(&turns, "reply").get("result").is_some()
                && field(&metadata, "reply").get("result").is_some(),
            "task_board_native_read_failed",
            "Successful native read bodies are required.",
        )?;
        let turn = array(field(field(field(&turns, "reply"), "result"), "data"))
            .iter()
            .find(|v| v.get("id") == value.get("turnId"))
            .ok_or_else(IvyError::internal)?;
        let status = s(turn, "status")?.clone();
        let thread = field(field(field(&metadata, "reply"), "result"), "thread");
        let in_progress = status == JsString::from("inProgress");
        if !in_progress {
            self.progress.forget(id);
        }
        let same_epoch = turns.get("epoch") == value.get("nativeEpoch");
        if in_progress && !same_epoch && self.services.owner_stopped(value).await? {
            return self.update(run, task, Json::object([("kind", "unavailable".into()), ("reason", "external_outcome".into()), ("code", "native_process_lost".into()), ("detail", "The original native process tree stopped; the saved unfinished turn cannot continue in the replacement owner.".into())])).await;
        }
        // History is not an attachment to the live thread. Reuse the retained
        // resume path before adopting an epoch or controlling its saved turn.
        if in_progress
            && equal(field(thread, "status"), "type", "active")
            && (active || !field(value, "cancellation").is_null())
        {
            let attachment = if field(calls, "thread").is_null() {
                None
            } else {
                Some(
                    self.store
                        .read("task-board/native-call", calls.required("thread")?, true)
                        .await?,
                )
            };
            if attachment.as_ref().is_none_or(|call| {
                call.value.get("epoch") != turns.get("epoch")
                    || equal(&call.value, "ownerPhase", "failed")
            }) {
                let origin = attachment
                    .map(|call| {
                        Json::object([
                            ("kind", "reattach".into()),
                            ("predecessor", call.pin),
                            ("preparedEpoch", field(&turns, "epoch").clone()),
                        ])
                    })
                    .unwrap_or_else(|| Json::object([("kind", "initial".into())]));
                self.services
                    .prepare(id, NativeSlot::Thread, Some(&origin))
                    .await?;
                return Ok(());
            }
        }
        if in_progress
            && !field(value, "cancellation").is_null()
            && same_epoch
            && equal(field(thread, "status"), "type", "active")
        {
            if field(calls, "interrupt").is_null() {
                self.services
                    .prepare(id, NativeSlot::Interrupt, None)
                    .await?;
                return Ok(());
            }
            if self.unresolved(run, NativeSlot::Interrupt).await?
                && self
                    .services
                    .advance(id, NativeSlot::Interrupt)
                    .await?
                    .as_object()
                    .is_some()
            {
                return Ok(());
            }
        }
        let prior = field(field(value, "externalOutcome"), "evidence");
        let mut snapshot = None;
        if !in_progress
            && prior.as_object().is_some()
            && self
                .store
                .local_key(s(prior.required("object")?, "objectId")?)?
                .as_deref()
                == Some("task-board/native-turn-snapshot")
        {
            match self.services.snapshot(&run.pin, prior).await {
                Ok(saved) => {
                    if saved.get("status").and_then(Json::as_js_string) == Some(&status)
                        && saved.get("epoch") == turns.get("epoch")
                    {
                        snapshot = Some(prior.clone());
                    }
                }
                // A retained observation can outlive its original Run revision.
                // Only pruning permits replacement by fresh verified evidence.
                Err(error) if error.code == "revision_pruned" => {}
                Err(error) => return Err(error),
            }
        }
        if in_progress {
            if active && field(value, "cancellation").is_null() && same_epoch {
                let previous = if !field(calls, "steer").is_null() {
                    let call = self
                        .store
                        .read("task-board/native-call", calls.required("steer")?, true)
                        .await?;
                    if equal(&call.value, "ownerPhase", "succeeded") {
                        call.value.required("origin")?.clone()
                    } else {
                        Json::Null
                    }
                } else {
                    Json::Null
                };
                let initial = self
                    .store
                    .read(
                        "task-board/task",
                        &Json::object([
                            ("objectId", value.required("taskId")?.clone()),
                            (
                                "revision",
                                value
                                    .required("originalRequest")?
                                    .required("expectedRevision")?
                                    .clone(),
                            ),
                        ]),
                        true,
                    )
                    .await?;
                let original_comments = array(field(field(value, "originalRequest"), "commentIds"));
                let comments: Vec<Json> = array(field(&task.value, "commentDeliveries"))
                    .iter()
                    .filter(|v| {
                        equal(v, "state", "queued")
                            && !original_comments.contains(field(v, "commentId"))
                    })
                    .map(|v| field(v, "commentId").clone())
                    .collect();
                let baseline = if equal(&previous, "kind", "steer") {
                    previous
                } else {
                    Json::object([
                        (
                            "workRevision",
                            initial.value.required("workRevision")?.clone(),
                        ),
                        ("commentIds", Json::Array(vec![])),
                    ])
                };
                let observed_dependencies = self.engine.dependency_snapshot(&task.value).await?;
                let at = self.engine.now();
                let wake_at = field(&task.value, "wakeAt")
                    .as_js_string()
                    .filter(|wake| *wake <= &at)
                    .cloned()
                    .map(Json::String)
                    .unwrap_or(Json::Null);
                let execution_change =
                    field(&task.value, "executionChangeRequested").as_bool() == Some(true);
                if task.value.get("workRevision") != baseline.get("workRevision")
                    || comments
                        .iter()
                        .any(|v| !array(field(&baseline, "commentIds")).contains(v))
                    || dependencies_changed(&task.value, &observed_dependencies)?
                    || !wake_at.is_null()
                    || execution_change
                        && field(&baseline, "executionChangeRequested").as_bool() != Some(true)
                {
                    let mut origin = Json::object([
                        ("kind", "steer".into()),
                        ("commentIds", Json::Array(comments)),
                        ("observedDependencies", observed_dependencies),
                        ("wakeAt", wake_at),
                        ("workRevision", task.value.required("workRevision")?.clone()),
                        ("peersHash", hash_json(&Json::Array(vec![]))?.into()),
                        ("predecessor", field(calls, "steer").clone()),
                    ]);
                    if execution_change {
                        origin.insert("executionChangeRequested", true.into());
                    }
                    self.services
                        .prepare(id, NativeSlot::Steer, Some(&origin))
                        .await?;
                    return Ok(());
                }
            }
            let evidence = if prior.as_object().is_some() {
                prior.clone()
            } else {
                Json::object([
                    ("object", run.pin.clone()),
                    ("label", "Current Run".into()),
                    ("contentHash", run.content_hash.clone().into()),
                    ("mediaType", "application/json".into()),
                ])
            };
            let projected = project_native_state(
                value,
                &status,
                field(&turns, "epoch"),
                thread,
                s(&turns, "observedAt")?,
                &evidence,
            )?;
            self.progress.observe(
                id,
                s(value, "taskId")?,
                equal(&projected, "phase", "running") && field(value, "cancellation").is_null(),
                hash_json(&Json::object([
                    ("turn", turn.clone()),
                    ("status", field(thread, "status").clone()),
                ]))?
                .into(),
                self.engine.now_millis(),
            );
            let waiting = if active {
                field(&task.value, "runtimeWait")
            } else {
                &Json::Null
            };
            let projected_waiting = projected.required("waiting")?;
            let workflow_state = if equal(&projected, "phase", "waiting_input") {
                "waiting".into()
            } else {
                self.engine.claimed_workflow_state(projected_waiting)?
            };
            if (!equal(field(&projected, "externalOutcome"), "state", "active") || same_epoch)
                && projected.get("phase") == value.get("phase")
                && field(&projected, "externalOutcome").get("state")
                    == field(value, "externalOutcome").get("state")
                && field(&projected, "externalOutcome").get("code")
                    == field(value, "externalOutcome").get("code")
                && (!active
                    || field(projected_waiting, "reason") == field(waiting, "reason")
                        && field(projected_waiting, "detail") == field(waiting, "detail")
                        && task.value.get("workflowState").and_then(Json::as_js_string)
                            == Some(&workflow_state))
            {
                return Ok(());
            }
        }
        let Some(snapshot) = snapshot else {
            let snapshot = self
                .services
                .save_snapshot(&run.pin, &metadata, &turns)
                .await?;
            return self
                .update(
                    run,
                    task,
                    Json::object([
                        ("kind", "readSnapshot".into()),
                        ("snapshot", snapshot),
                        (
                            "notificationCursor",
                            value.required("notificationCursor")?.clone(),
                        ),
                    ]),
                )
                .await;
        };
        if active && status == JsString::from("completed") {
            self.engine.sync_native_comments(id).await?;
        }
        let current_task = self
            .store
            .read("task-board/task", task.pin.required("objectId")?, true)
            .await?;
        let output = if self.engine.run_handoff(&current_task.value, id)? {
            None
        } else {
            self.final_output(run, &snapshot).await?
        };
        let fresh = self
            .store
            .read("task-board/task", task.pin.required("objectId")?, true)
            .await?;
        self.update(
            run,
            &fresh,
            Json::object([
                ("kind", "completion".into()),
                ("snapshot", snapshot),
                ("output", output.unwrap_or(Json::Null)),
                (
                    "notificationCursor",
                    value.required("notificationCursor")?.clone(),
                ),
            ]),
        )
        .await
    }
}
