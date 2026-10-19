//! One bounded reconciliation pass over retained native calls and observations.
//! Current engine admission, prompts, peer and handoff policy remain explicit.
use crate::{
    native_driver::{NativeDriver, NativeDriverConnection, NativeDriverRuntime, NativeDriverStore},
    native_inspector::{Inspection, InspectionContext, InspectionStore, NativeInspector, PageSize},
    native_intent::{NativeSlot, TaskPrompts},
    native_state::{finished_run, project_native_state},
    native_turn_evidence::{TurnEvidence, TurnEvidenceStore},
    operations::{Document, Store, mutation},
    turn_output::{final_output_text, output_params, page_cursor},
};
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString,
    checked_native_contract::CheckedNativeContracts, hash::hash_json, json::canonical,
    limits::MANAGEMENT_FRAME_BYTES,
};
use ivy_sdk::native_observations::NativeObservations;
use std::{collections::HashSet, future::Future};

/// The owning engine must bind its current nativeUpdate, read-cache scope,
/// handoff/claim/peer policy and comment mirroring. There are no policy defaults.
#[allow(async_fn_in_trait)]
pub trait OrchestrationEngine: NativeDriverRuntime {
    fn now_millis(&self) -> f64;
    fn run_handoff(&self, task: &Json, run: &JsString) -> IvyResult<bool>;
    fn claimed_workflow_state(&self, waiting: &Json) -> IvyResult<JsString>;
    async fn peer_state(&self, task: &Json, task_id: &JsString) -> IvyResult<JsString>;
    async fn sync_native_comments(&self, run: &JsString) -> IvyResult<()>;
    async fn with_read_cache<T, F: Future<Output = IvyResult<T>>>(&self, work: F) -> IvyResult<T>;
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
    threads: Vec<(JsString, JsString, JsString)>,
    retries: Vec<(JsString, u8, f64)>,
}
fn field<'a>(v: &'a Json, k: &str) -> &'a Json {
    static NULL: Json = Json::Null;
    v.get(k).unwrap_or(&NULL)
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
fn equal(v: &Json, k: &str, s: &str) -> bool {
    v.get(k) == Some(&Json::from(s))
}
fn string(v: &Json) -> JsString {
    v.as_js_string().cloned().unwrap_or_default()
}
fn s<'a>(v: &'a Json, k: &str) -> IvyResult<&'a JsString> {
    v.required(k)?.as_js_string().ok_or_else(IvyError::internal)
}
fn number(v: &Json, k: &str) -> f64 {
    v.get(k).and_then(Json::as_f64).unwrap_or(f64::NAN)
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
    ) -> Self {
        Self {
            store,
            engine,
            services,
            catalogs,
            threads: vec![],
            retries: vec![],
        }
    }
    pub fn notification(&mut self, value: &Json) -> bool {
        let params = field(value, "params");
        let event = field(params, "payload");
        let body = field(event, "params");
        if ![
            "turn/completed",
            "thread/status/changed",
            "serverRequest/resolved",
        ]
        .iter()
        .any(|v| equal(event, "method", v))
        {
            return false;
        }
        let mut matched = false;
        for (run, node, thread) in &self.threads {
            if params.get("serviceNodeId").and_then(Json::as_js_string) == Some(node)
                && body.get("threadId").and_then(Json::as_js_string) == Some(thread)
            {
                self.retries.retain(|r| &r.0 != run);
                matched = true;
            }
        }
        matched
    }
    fn postpone(&mut self, run: &JsString) {
        let prior = self.retries.iter().find(|v| &v.0 == run);
        let count = (prior.map(|v| v.1).unwrap_or(0) + 1).min(5);
        if self.retries.len() >= 1024 && prior.is_none() {
            self.retries.remove(0);
        }
        let after =
            self.engine.now_millis() + (30000.0 * 2_f64.powi(count as i32 - 1)).min(300000.0);
        if let Some(entry) = self.retries.iter_mut().find(|v| &v.0 == run) {
            *entry = (run.clone(), count, after);
        } else {
            self.retries.push((run.clone(), count, after));
        }
    }
    pub async fn drain(&mut self, run: &JsString) -> IvyResult<bool> {
        if self
            .retries
            .iter()
            .find(|v| &v.0 == run)
            .map(|v| v.2)
            .unwrap_or(0.0)
            > self.engine.now_millis()
        {
            return Ok(false);
        }
        let engine = self.engine;
        engine.with_read_cache(self.drain_pass(run)).await?;
        Ok(true)
    }
    async fn drain_pass(&mut self, run: &JsString) -> IvyResult<()> {
        for _ in 0..8 {
            let before = self
                .store
                .read("task-board/run", &Json::String(run.clone()), true)
                .await?;
            if let Err(error) = self.step(run).await {
                self.postpone(run);
                return Err(error);
            }
            let after = self
                .store
                .read("task-board/run", &Json::String(run.clone()), true)
                .await?;
            let value = &after.value;
            if finished_run(value) {
                self.threads.retain(|v| &v.0 != run);
                self.retries.retain(|v| &v.0 != run);
            } else if truthy(field(value, "primaryResourceRef")) {
                if self.threads.len() >= 1024 {
                    self.threads.remove(0);
                }
                let node = s(value.required("target")?, "serviceNodeId")?.clone();
                let thread = s(value.required("primaryResourceRef")?, "nativeId")?.clone();
                if let Some(entry) = self.threads.iter_mut().find(|v| &v.0 == run) {
                    *entry = (run.clone(), node, thread);
                } else {
                    self.threads.push((run.clone(), node, thread));
                }
            }
            let waiting = if equal(value, "phase", "starting") {
                self.store
                    .read("task-board/task", value.required("taskId")?, true)
                    .await?
                    .value
                    .get("waiting")
                    .cloned()
                    .unwrap_or(Json::Null)
            } else {
                Json::Null
            };
            if equal(value, "phase", "outcome_unknown") || equal(&waiting, "reason", "host") {
                self.postpone(run);
            } else {
                self.retries.retain(|v| &v.0 != run);
            }
            if !["starting", "publishing_result", "cancel_requested"]
                .iter()
                .any(|v| equal(value, "phase", v))
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
        if !truthy(pin) {
            return Ok(false);
        }
        let latest = self
            .store
            .read("task-board/native-call", pin.required("objectId")?, true)
            .await?;
        Ok(number(&latest.pin, "revision") > number(pin, "revision")
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
            !truthy(field(&task.value, "publication"))
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
                    && !truthy(field(&task.value, "waiting")))
        {
            return Ok(());
        }
        let waiting = field(&task.value, "waiting");
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
        if !truthy(field(value, "turnId")) {
            for slot in [NativeSlot::Thread, NativeSlot::Turn] {
                if self.unresolved(run, slot).await? {
                    let result = async {
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
                            || truthy(field(value, "cancellation"))
                            || !truthy(field(calls, "thread"))
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
            if truthy(field(value, "cancellation")) || !active {
                if !truthy(field(calls, "turn")) {
                    self.update(
                        run,
                        task,
                        Json::object([(
                            "kind",
                            if truthy(field(value, "cancellation")) {
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
                    if truthy(field(calls, "thread")) {
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
        let status = string(field(turn, "status"));
        let thread = field(field(field(&metadata, "reply"), "result"), "thread");
        let in_progress = status == JsString::from("inProgress");
        let same_epoch = turns.get("epoch") == value.get("nativeEpoch");
        if in_progress && !same_epoch && self.services.owner_stopped(value).await? {
            return self.update(run, task, Json::object([("kind", "unavailable".into()), ("reason", "external_outcome".into()), ("code", "native_process_lost".into()), ("detail", "The original native process tree stopped; the saved unfinished turn cannot continue in the replacement owner.".into())])).await;
        }
        if in_progress
            && truthy(field(value, "cancellation"))
            && same_epoch
            && equal(field(thread, "status"), "type", "active")
        {
            if !truthy(field(calls, "interrupt")) {
                self.services
                    .prepare(id, NativeSlot::Interrupt, None)
                    .await?;
                return Ok(());
            }
            if self.unresolved(run, NativeSlot::Interrupt).await?
                && truthy(&self.services.advance(id, NativeSlot::Interrupt).await?)
            {
                return Ok(());
            }
        }
        let prior = field(field(value, "externalOutcome"), "evidence");
        let mut snapshot = None;
        if !in_progress
            && truthy(prior)
            && self
                .store
                .local_key(s(prior.required("object")?, "objectId")?)?
                .as_deref()
                == Some("task-board/native-turn-snapshot")
        {
            let saved = self.services.snapshot(&run.pin, prior).await?;
            if saved.get("status").and_then(Json::as_js_string) == Some(&status)
                && saved.get("epoch") == turns.get("epoch")
            {
                snapshot = Some(prior.clone());
            }
        }
        if in_progress {
            if active && !truthy(field(value, "cancellation")) && same_epoch {
                let previous = if truthy(field(calls, "steer")) {
                    self.store
                        .read("task-board/native-call", calls.required("steer")?, true)
                        .await?
                        .value
                        .get("origin")
                        .cloned()
                        .unwrap_or(Json::Null)
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
                let peers = self
                    .engine
                    .peer_state(&task.value, s(&task.pin, "objectId")?)
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
                        ("peersHash", hash_json(&Json::Array(vec![]))?.into()),
                        ("commentIds", Json::Array(vec![])),
                    ])
                };
                if task.value.get("workRevision") != baseline.get("workRevision")
                    || baseline.get("peersHash").and_then(Json::as_js_string) != Some(&peers)
                    || comments
                        .iter()
                        .any(|v| !array(field(&baseline, "commentIds")).contains(v))
                {
                    self.services
                        .prepare(
                            id,
                            NativeSlot::Steer,
                            Some(&Json::object([
                                ("kind", "steer".into()),
                                ("commentIds", Json::Array(comments)),
                                ("workRevision", task.value.required("workRevision")?.clone()),
                                ("peersHash", Json::String(peers)),
                                ("predecessor", field(calls, "steer").clone()),
                            ])),
                        )
                        .await?;
                    return Ok(());
                }
            }
            let evidence = if truthy(prior) {
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
            let waiting = if active {
                field(&task.value, "waiting")
            } else {
                &Json::Null
            };
            let projected_waiting = if self
                .engine
                .claimed_workflow_state(projected.required("waiting")?)?
                == JsString::from("waiting")
            {
                projected.required("waiting")?
            } else {
                &Json::Null
            };
            if projected.get("phase") == value.get("phase")
                && field(&projected, "externalOutcome").get("state")
                    == field(value, "externalOutcome").get("state")
                && field(&projected, "externalOutcome").get("code")
                    == field(value, "externalOutcome").get("code")
                && (!active
                    || field(projected_waiting, "reason") == field(waiting, "reason")
                        && field(projected_waiting, "detail") == field(waiting, "detail")
                        && task.value.get("workflowState").and_then(Json::as_js_string)
                            == Some(
                                &self
                                    .engine
                                    .claimed_workflow_state(projected.required("waiting")?)?,
                            ))
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
