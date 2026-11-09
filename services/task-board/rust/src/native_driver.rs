//! Retain and reconcile attached original native calls. The current engine owns
//! native-update admission; dispatch keeps the original request and domain guards.
use crate::{
    inspection_journal::verify_owner,
    journal_store::{Contracts, JournalStore},
    native_evidence::NativeEvidence,
    native_inspector::management_read,
    native_intent::{NativeIntent, NativeSlot, TaskPrompts},
    native_plan::{catalog, matches_primary, routed_owner, routed_selection, routed_target},
    operations::{Document, Store, mutation},
    read_evidence::ReadEvidenceStore,
};
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString, agent_schema::validate_agent,
    checked_native_contract::CheckedNativeContracts, hash::hash_json, json::canonical_equal,
    limits::MANAGEMENT_FRAME_BYTES,
};
use ivy_sdk::{
    native_evidence::{NativeReadIdentity, validate_native_read, validate_native_status},
    native_observations::NativeObservations,
    native_operation::{
        NativeOperationCall, NativeOperationJournal, NativeReconciliation,
        reconcile_native_operation_local,
    },
    native_owner::NativeOwner,
    operation_ids::scoped_operation_id,
    rpc::{RequestOptions, RpcClient},
    tools::ToolClients,
};
use std::{future::Future, sync::Arc};

#[allow(async_fn_in_trait)]
pub trait NativeDriverRuntime {
    async fn verify_owner(&self) -> IvyResult<()>;
    async fn scoped_operation_id(&self, scope: &Json) -> IvyResult<JsString>;
    async fn native_update(&self, request: &Json) -> IvyResult<Json>;
    fn now(&self) -> JsString;
}
/// Keep native bindings on the same authenticated connection as the journal and
/// the logical-owner check. Preparation alone needs no native transport.
pub trait NativeDriverConnection: NativeDriverRuntime {
    fn tools(&self) -> &Arc<ToolClients>;
    fn principal_id(&self) -> &JsString;
}
/// Preserve the original UTF-16 scoped operation name without routing it through
/// Store's UTF-8 convenience name API.
#[allow(async_fn_in_trait)]
pub trait NativeDriverStore: Store + ReadEvidenceStore {
    async fn named_call(&self, name: &JsString) -> IvyResult<Option<Document>>;
}
impl<R: RpcClient + 'static, P: Contracts, C: Fn() -> String, I: Fn() -> JsString> NativeDriverStore
    for JournalStore<'_, R, P, C, I>
{
    async fn named_call(&self, name: &JsString) -> IvyResult<Option<Document>> {
        self.named_record(
            "task-board/native-call",
            name,
            self.journal.root_object_id.as_ref(),
        )
        .await
    }
}
/// Bind this to the journal's authenticated RPC client and the accepted current
/// engine nativeUpdate path. There is no default update/admission callback.
pub struct DriverRuntime<R, U, C> {
    pub client: Arc<R>,
    pub principal_id: JsString,
    pub service_node_id: JsString,
    pub native_update: U,
    pub now: C,
    tools: Arc<ToolClients>,
}
impl<R: RpcClient + 'static, U, C> DriverRuntime<R, U, C> {
    pub fn new(
        client: Arc<R>,
        principal_id: JsString,
        service_node_id: JsString,
        native_update: U,
        now: C,
    ) -> Self {
        Self {
            tools: Arc::new(ToolClients::new(client.clone())),
            client,
            principal_id,
            service_node_id,
            native_update,
            now,
        }
    }
}
impl<
    R: RpcClient + 'static,
    U: Fn(Json) -> F,
    F: Future<Output = IvyResult<Json>>,
    C: Fn() -> JsString,
> NativeDriverRuntime for DriverRuntime<R, U, C>
{
    async fn verify_owner(&self) -> IvyResult<()> {
        verify_owner(
            self.client.as_ref(),
            &self.principal_id,
            &self.service_node_id,
        )
        .await
    }
    async fn scoped_operation_id(&self, scope: &Json) -> IvyResult<JsString> {
        scoped_operation_id(self.client.clone(), scope).await
    }
    async fn native_update(&self, request: &Json) -> IvyResult<Json> {
        (self.native_update)(request.clone()).await
    }
    fn now(&self) -> JsString {
        (self.now)()
    }
}
impl<
    R: RpcClient + 'static,
    U: Fn(Json) -> F,
    F: Future<Output = IvyResult<Json>>,
    C: Fn() -> JsString,
> NativeDriverConnection for DriverRuntime<R, U, C>
{
    fn tools(&self) -> &Arc<ToolClients> {
        &self.tools
    }
    fn principal_id(&self) -> &JsString {
        &self.principal_id
    }
}
pub struct NativeDriver<'a, S, R, P> {
    pub store: &'a S,
    pub runtime: &'a R,
    pub prompts: &'a P,
    pub catalogs: &'a CheckedNativeContracts,
}
impl<S: NativeDriverStore, R: NativeDriverConnection, P: TaskPrompts> NativeDriver<'_, S, R, P> {
    fn native_owner(&self, task: &Json, run: &Json) -> IvyResult<NativeOwner> {
        Ok(NativeOwner::new(
            self.runtime.tools().clone(),
            self.runtime.principal_id().clone(),
            routed_owner(self.catalogs, task, run)?,
            RequestOptions::default(),
        ))
    }
    pub async fn status(&self, run: &Json, call: &Json) -> IvyResult<Json> {
        let task = self
            .store
            .read_task_header(run.required("taskId")?, true)
            .await?;
        self.status_for_task(&task.value, run, call).await
    }
    async fn status_for_task(&self, task: &Json, run: &Json, call: &Json) -> IvyResult<Json> {
        let target = routed_target(task, run.required("target")?)?;
        // The original reads before selecting the retained catalog. Keep that
        // failure order and its domain-specific readiness error.
        let value = management_read(
            self.runtime.tools(),
            string(&target, "serviceNodeId")?,
            "status",
            ivy_contracts::native_backend::BackendIdentity::select_arguments(
                ivy_contracts::native_backend::BackendIdentity::optional(target.get("backend"))?
                    .as_ref(),
                Json::object([] as [(&str, Json); 0]),
            )?,
        )
        .await?;
        NativeIntent {
            store: self.store,
            catalogs: self.catalogs,
        }
        .call_owner(run, call)?;
        validate_native_status(
            &value,
            self.native_owner(task, run)?.target(),
            Some("task_board_native_unavailable"),
        )?;
        Ok(value)
    }
    /// A replacement owner may reattach the exact saved thread after an
    /// uncertain resume. A started/changed turn is never replay authority.
    pub async fn recover_uncertain_resume(&self, run_id: &JsString) -> IvyResult<bool> {
        let current = self.current(run_id).await?;
        let run = &current.run.value;
        let pin = field(field(run, "calls"), "thread");
        let primary = field(run, "primaryResourceRef");
        let attaching_turn = !field(run, "turnId").is_null()
            && (current.active || !field(run, "cancellation").is_null());
        if ((!current.active || !field(run, "cancellation").is_null()) && !attaching_turn)
            || pin.is_null()
            || primary.is_null()
        {
            return Ok(false);
        }
        let turn_pin = field(field(run, "calls"), "turn");
        if field(run, "turnId").is_null() && !turn_pin.is_null() {
            let turn = Store::read(
                self.store,
                "task-board/native-call",
                turn_pin.required("objectId")?,
                true,
            )
            .await?;
            if Some(turn.pin.revision) != turn_pin.get("revision").and_then(Json::as_f64)
                || field(&turn.value, "state") != &Json::from("prepared")
                || field(&turn.value, "ownerPhase") != &Json::Null
                || field(&turn.value, "evidence") != &Json::Null
            {
                return Ok(false);
            }
        }
        let call = Store::read(self.store, "task-board/native-call", pin, true).await?;
        if field(field(&call.value, "request"), "method") != &Json::from("thread/resume")
            || field(&call.value, "ownerPhase") != &Json::from("outcome_unknown")
            || field(&call.value, "evidence").is_null()
            || field(&call.value, "epoch").is_null()
            || Some(call.pin.revision) != pin.get("revision").and_then(Json::as_f64)
        {
            return Ok(false);
        }
        let identity_matches = matches_primary(run.required("target")?, primary)?;
        let params_match = if identity_matches {
            let request = NativeIntent {
                store: self.store,
                catalogs: self.catalogs,
            }
            .resolve_native_request(call.value.required("request")?)?;
            request.required("params")?.get("threadId") == primary.get("nativeId")
        } else {
            false
        };
        require(
            identity_matches && params_match,
            "task_board_primary_mismatch",
            "The uncertain resume must identify the saved native context.",
        )?;
        let status = self
            .status_for_task(&current.task.value, run, &call.value)
            .await?;
        if status.get("epoch") == call.value.get("epoch") {
            return Ok(false);
        }
        let read = self
            .native_owner(&current.task.value, run)?
            .read(
                &"thread/read".into(),
                Json::object([
                    ("threadId", primary.required("nativeId")?.clone()),
                    ("includeTurns", false.into()),
                ]),
                Some(string(&status, "epoch")?),
                true,
            )
            .await?;
        let result = read.required("reply")?.get("result");
        require(
            result.is_some(),
            "task_board_native_read_failed",
            "The saved native context is unavailable to the replacement owner.",
        )?;
        let thread = field(result.unwrap(), "thread");
        require(
            thread.get("id") == primary.get("nativeId")
                && field(thread, "ephemeral") == &Json::Bool(false),
            "task_board_primary_mismatch",
            "Recovery read a different native context.",
        )?;
        self.prepare(
            run_id,
            NativeSlot::Thread,
            Some(&Json::object([
                ("kind", "reattach".into()),
                ("predecessor", pin.clone()),
                ("preparedEpoch", status.required("epoch")?.clone()),
            ])),
        )
        .await?;
        Ok(true)
    }
    /// Reconcile only the attached original operation. Confirmed owner absence
    /// reaches domain dispatch guards; a missing receipt never authorizes replay.
    pub async fn advance(&self, run_id: &JsString, slot: NativeSlot) -> IvyResult<Json> {
        let current = self.current(run_id).await?;
        let run = &current.run.value;
        let pin = field(field(run, "calls"), slot.as_str());
        require(
            !pin.is_null(),
            "task_board_native_order",
            "Dispatch requires an attached retained NativeCall.",
        )?;
        let call = Store::read(
            self.store,
            "task-board/native-call",
            pin.required("objectId")?,
            true,
        )
        .await?;
        let intent = NativeIntent {
            store: self.store,
            catalogs: self.catalogs,
        };
        let request = call.value.required("request")?;
        let native_request = intent.resolve_native_request(request)?;
        // Publish the exact committed observation after a lost response before
        // making any owner/evidence query, even when that observation concluded.
        if call.pin.revision
            > pin
                .required("revision")?
                .as_f64()
                .ok_or_else(IvyError::internal)?
        {
            return self
                .update(
                    &current,
                    Json::object([
                        ("kind", "observeCall".into()),
                        ("slot", slot.as_str().into()),
                        ("call", call.pin.to_json()),
                    ]),
                )
                .await;
        }
        require(
            !["succeeded", "failed"]
                .iter()
                .any(|v| field(&call.value, "ownerPhase") == &Json::from(*v)),
            "task_board_native_order",
            "The attached call already has its conclusive original observation.",
        )?;
        let evidence = NativeEvidence {
            store: self.store,
            catalogs: self.catalogs,
        };
        let expected = intent.evidence_identity(run, &call.value, self.runtime.principal_id())?;
        let owner = self.native_owner(&current.task.value, run)?;
        let original_owner = intent.call_owner(run, &call.value)?;
        let original_reader = owner.original_operations(original_owner.clone());
        let previous = if !field(&call.value, "evidence").is_null() {
            Some(
                evidence
                    .read(
                        &call.pin.to_json(),
                        call.value.required("evidence")?,
                        &expected,
                    )
                    .await?,
            )
        } else {
            None
        };
        let original = NativeOperationCall {
            operation_id: string(&call.value, "operationId")?.clone(),
            method: string(request, "method")?.clone(),
            params: native_request.required("params")?.clone(),
            definition_hash: string(&call.value, "expectedDefinitionHash")?.clone(),
        };
        let current_ref = &current;
        let call_ref = &call;
        let expected_ref = &expected;
        let evidence_ref = &evidence;
        let owner_ref = &owner;
        let native_request_ref = &native_request;
        let reconciled = reconcile_native_operation_local(
            &original_reader,
            &original,
            NativeOperationJournal {
                current: Json::Null,
                previous,
                was_observed: field(&call.value, "state") != &Json::from("prepared")
                    || field(&call.value, "ownerPhase") != &Json::Null,
                retain: |observed: Json| async move {
                    let artifact = evidence_ref.save(&call_ref.pin.to_json(), &observed, expected_ref).await?;
                    let mut next = call_ref.value.clone();
                    next.insert("state", if field(&observed,"phase")==&Json::from("outcome_unknown") {
                        "outcome_unknown".into()
                    } else { "observed".into() });
                    for (target, source) in [("ownerPhase","phase"),("observedAt","updatedAt"),("epoch","epoch"),("code","code")] {
                        next.insert(target, observed.required(source)?.clone());
                    }
                    next.insert("evidence", artifact);
                    let saved = Store::write(self.store,"task-board/native-call",&next,
                        &mutation(&call_ref.pin.object_id, &format!("observe:{}",hash_json(&observed)?))?,
                        &Json::object([
                            ("objectId",Json::String(call_ref.pin.object_id.clone())),
                            ("expectedRevision",Json::Number(call_ref.pin.revision)),
                        ]),
                    ).await?;
                    self.update(current_ref,Json::object([
                        ("kind","observeCall".into()),("slot",slot.as_str().into()),("call",saved),
                    ])).await
                },
            },
            Some(move |original: NativeOperationCall, absence: Json| async move {
                let owner = owner_ref;
                let current = current_ref;
                let call = call_ref;
                let native_request = native_request_ref;
                require(owner.target() == &original_owner,
                    "task_board_native_route_changed", "A different current route cannot dispatch an original call. Reconcile its retained owner without replay.")?;
                let status = self.status_for_task(&current.task.value,run,&call.value).await?;
                require(absence.get("epoch")==status.get("epoch"),
                    "task_board_native_epoch_changed", "Absence and current native status must come from the same epoch.")?;
                let thread_pin = field(field(run,"calls"),"thread");
                let primary_failed = if slot==NativeSlot::Turn && !thread_pin.is_null() {
                    let thread = Store::read(self.store,"task-board/native-call",thread_pin,true).await?;
                    field(&thread.value,"ownerPhase")==&Json::from("failed")
                } else { false };
                let attaching_turn = slot==NativeSlot::Thread && !field(run,"turnId").is_null()
                    && (current.active || !field(run,"cancellation").is_null());
                if (!current.active || !field(run,"cancellation").is_null() || primary_failed
                    || slot==NativeSlot::Steer && status.get("epoch")!=run.get("nativeEpoch"))
                    && slot!=NativeSlot::Interrupt && !attaching_turn
                {
                    let prevented = owner.management("prevent",Json::object([
                        ("operationId",call.value.required("operationId")?.clone()),
                        ("nativeVersion",request.required("nativeVersion")?.clone()),
                        ("method",request.required("method")?.clone()),
                        ("params",native_request.required("params")?.clone()),
                        ("expectedDefinitionHash",call.value.required("expectedDefinitionHash")?.clone()),
                    ]),Some(string(&call.value,"operationId")?.clone())).await?;
                    validate_agent("Operation",&prevented)?;
                    return Ok(Some(prevented));
                }
                if slot==NativeSlot::Turn {
                    require(status.get("epoch")==run.get("nativeEpoch"),
                        "task_board_reattach_required", "The saved primary must be confirmed in the current native epoch before turn dispatch.")?;
                    require(!thread_pin.is_null(),"task_board_native_order","A turn needs its retained thread receipt.")?;
                    let thread = Store::read(self.store,"task-board/native-call",thread_pin,true).await?;
                    require(field(&thread.value,"ownerPhase")==&Json::from("succeeded")
                        && thread.value.get("epoch")==status.get("epoch"),
                        "task_board_native_order","The latest primary attachment must be confirmed in this native epoch before dispatch.")?;
                    let primary = run.required("primaryResourceRef")?;
                    let params = Json::object([
                        ("threadId",primary.required("nativeId")?.clone()),("includeTurns",false.into()),
                    ]);
                    let snapshot = management_read(self.runtime.tools(),&owner.target().service_node_id,
                        "read",ivy_contracts::native_backend::BackendIdentity::select_arguments(owner.target().backend.as_ref(),Json::object([
                            ("nativeVersion",request.required("nativeVersion")?.clone()),
                            ("method","thread/read".into()),("params",params.clone()),
                        ]))?,
                    ).await?;
                    validate_native_read(&snapshot,&NativeReadIdentity {
                        owner:owner.target().clone(),caller_principal_id:self.runtime.principal_id().clone(),
                        epoch:string(&status,"epoch")?.clone(),method:"thread/read".into(),params,
                    },Some("task_board_native_unavailable"))?;
                    let result = snapshot.required("reply")?.get("result");
                    require(result.is_some(),"task_board_native_unavailable",
                        "Turn dispatch requires a successful fresh native primary observation.")?;
                    let source = catalog(self.catalogs, request)?;
                    let state = NativeObservations::new(&source).native_thread_state(result.unwrap())?;
                    require(state.thread.get("id")==primary.get("nativeId")
                        && field(&state.thread,"ephemeral")==&Json::Bool(false) && state.can_start_turn,
                        "task_board_native_not_idle",
                        "The native primary must be currently attached, accept input and have no active turn before its turn starts.")?;
                }
                if slot==NativeSlot::Interrupt {
                    require(status.get("epoch")==run.get("nativeEpoch"),"task_board_native_epoch_changed",
                        "An old turn cannot be interrupted as if the replacement owner controlled it.")?;
                }
                owner.dispatch_local(&original,|| async {
                    let fresh = self.current(run_id).await?;
                    require(fresh.run.pin == current.run.pin
                        && fresh.task.pin == current.task.pin,
                        "revision_conflict","Task or Run changed before native dispatch; reconcile it before sending.")?;
                    self.runtime.verify_owner().await
                }).await
            }),
            Some("task_board_native_absence_conflict"),
        ).await?;
        match reconciled {
            NativeReconciliation::Observed { value, .. } => Ok(value),
            NativeReconciliation::Absent { .. } => Err(IvyError::new(
                "task_board_native_outcome_unavailable",
                "No original native operation was observed after dispatch; do not invent a receipt.",
            )),
        }
    }
}
struct Current {
    run: Document,
    task: Document,
    active: bool,
}
fn field<'a>(v: &'a Json, key: &str) -> &'a Json {
    static NULL: Json = Json::Null;
    v.get(key).unwrap_or(&NULL)
}
fn require(ok: bool, code: &str, message: &str) -> IvyResult<()> {
    if ok {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn string<'a>(v: &'a Json, key: &str) -> IvyResult<&'a JsString> {
    v.required(key)?
        .as_js_string()
        .ok_or_else(IvyError::internal)
}
impl<S: NativeDriverStore, R: NativeDriverRuntime, P: TaskPrompts> NativeDriver<'_, S, R, P> {
    async fn current(&self, run_id: &JsString) -> IvyResult<Current> {
        self.runtime.verify_owner().await?;
        let run = Store::read(
            self.store,
            "task-board/run",
            &Json::String(run_id.clone()),
            true,
        )
        .await?;
        let task = self
            .store
            .read_task_header(run.value.required("taskId")?, true)
            .await?;
        require(
            !["completed", "failed", "cancelled"]
                .iter()
                .any(|s| field(&run.value, "phase") == &Json::from(*s)),
            "task_board_native_order",
            "Native execution requires an unfinished Run.",
        )?;
        require(
            field(&task.value, "publication").is_null(),
            "task_board_publication_busy",
            "Native execution waits for the current task publication.",
        )?;
        let claim_run = field(field(&task.value, "claim"), "run");
        let active = claim_run.get("objectId").and_then(Json::as_js_string) == Some(run_id);
        require(
            !active || canonical_equal(claim_run, &run.pin.to_json(), MANAGEMENT_FRAME_BYTES)?,
            "revision_conflict",
            "Native execution requires the exact current claimed Run revision.",
        )?;
        Ok(Current { run, task, active })
    }
    async fn update(&self, current: &Current, change: Json) -> IvyResult<Json> {
        let scope = Json::object([
            ("task", current.task.pin.to_json()),
            ("run", current.run.pin.to_json()),
            ("change", change.clone()),
        ]);
        let id = mutation(
            &current.run.pin.object_id,
            &format!("native-update:{}", hash_json(&scope)?),
        )?;
        self.runtime
            .native_update(&Json::object([
                ("action", "nativeUpdate".into()),
                ("operationId", id.into()),
                ("taskId", Json::String(current.task.pin.object_id.clone())),
                ("expectedRevision", Json::Number(current.task.pin.revision)),
                ("run", current.run.pin.to_json()),
                ("change", change),
            ]))
            .await
    }
    pub async fn prepare(
        &self,
        run_id: &JsString,
        slot: NativeSlot,
        origin: Option<&Json>,
    ) -> IvyResult<Json> {
        let initial = Json::object([("kind", "initial".into())]);
        let origin = origin.unwrap_or(&initial);
        let current = self.current(run_id).await?;
        let attaching_turn = slot == NativeSlot::Thread
            && !field(&current.run.value, "turnId").is_null()
            && (current.active || !field(&current.run.value, "cancellation").is_null());
        require(
            if slot == NativeSlot::Interrupt {
                !field(&current.run.value, "cancellation").is_null()
            } else {
                attaching_turn
                    || current.active && field(&current.run.value, "cancellation").is_null()
            },
            "task_board_claim_inactive",
            "Only an active claim may prepare execution; retained turns may reattach for observation or cancellation.",
        )?;
        let mut scope = vec![
            "task-board-native-call".into(),
            current.run.value.required("operationId")?.clone(),
            slot.as_str().into(),
            origin.clone(),
        ];
        let original_target = current.run.value.required("target")?;
        let target = routed_target(&current.task.value, original_target)?;
        if !canonical_equal(&target, original_target, MANAGEMENT_FRAME_BYTES)? {
            // A prepared, unattached call at the old address cannot become a
            // new call at another owner merely because preparation is retried.
            scope.push(target);
        }
        let selection = routed_selection(&current.task.value, &current.run.value)?;
        let plan = current.run.value.required("plan")?;
        if selection.get("nativeVersion") != plan.get("nativeVersion")
            || selection.get("catalogSourceHash") != plan.get("catalogSourceHash")
        {
            scope.push(selection.clone());
        }
        let operation_id = self
            .runtime
            .scoped_operation_id(&Json::Array(scope))
            .await?;
        let mut name = JsString::from("Native call ");
        name.0.extend_from_slice(&operation_id.0);
        let mut call = self.store.named_call(&name).await?;
        if call.is_none() {
            let intent = NativeIntent {
                store: self.store,
                catalogs: self.catalogs,
            };
            let mut value = intent
                .intended_call_for_task(
                    &current.run.value,
                    slot,
                    Some(origin),
                    self.prompts,
                    Some(&current.task.value),
                )
                .await?;
            let request =
                intent.retain_native_request(value.required("request")?, &operation_id)?;
            for (k, v) in [
                ("request", request),
                ("origin", origin.clone()),
                ("operationId", Json::String(operation_id.clone())),
                ("state", "prepared".into()),
                ("preparedAt", Json::String(self.runtime.now())),
                ("observedAt", Json::Null),
                ("ownerPhase", Json::Null),
                ("epoch", Json::Null),
                ("evidence", Json::Null),
                ("code", Json::Null),
            ] {
                value.insert(k, v);
            }
            let saved = async {
                let pin = Store::write(
                    self.store,
                    "task-board/native-call",
                    &value,
                    &mutation(&operation_id, "prepare")?,
                    &Json::object([(
                        "create",
                        Json::object([
                            (
                                "parentId",
                                self.store
                                    .root_object_id()
                                    .map(Json::String)
                                    .unwrap_or(Json::Null),
                            ),
                            ("name", Json::String(name.clone())),
                        ]),
                    )]),
                )
                .await?;
                Store::read(self.store, "task-board/native-call", &pin, true).await
            }
            .await;
            call=Some(match saved {
                Ok(call)=>call,
                Err(e) if matches!(e.code.as_str(),"mutation_conflict"|"revision_conflict")=>{
                    self.store.named_call(&name).await?.ok_or_else(||IvyError::new("task_board_identity_conflict","The competing NativeCall must remain available under its original identity."))?
                }
                Err(e)=>return Err(e),
            });
        }
        let call = call.unwrap();
        if field(field(&current.run.value, "calls"), slot.as_str())
            .get("objectId")
            .and_then(Json::as_js_string)
            == Some(&call.pin.object_id)
        {
            return Ok(call.pin.to_json());
        }
        let mut change = Json::object([
            (
                "kind",
                if field(origin, "kind") == &Json::from("reattach") {
                    "reattachCall".into()
                } else {
                    "attachCall".into()
                },
            ),
            ("call", call.pin.to_json()),
        ]);
        if field(origin, "kind") != &Json::from("reattach") {
            change.insert("slot", slot.as_str().into());
        }
        self.update(&current, change).await?;
        Ok(call.pin.to_json())
    }
}
