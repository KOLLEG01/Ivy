//! Retain native calls and recover an uncertain input-free resume. The current
//! engine owns native-update admission; neither operation dispatches a turn.
use crate::{
    inspection_journal::verify_owner,
    journal_store::{Contracts, JournalStore},
    native_inspector::management_read,
    native_intent::{NativeIntent, NativeSlot, TaskPrompts},
    native_plan::catalog,
    operations::{Document, Store, mutation},
    read_evidence::ReadEvidenceStore,
};
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString,
    checked_native_contract::CheckedNativeContracts, hash::hash_json, json::canonical,
    limits::MANAGEMENT_FRAME_BYTES,
};
use ivy_sdk::{
    native_evidence::{NativeOwnerIdentity, validate_native_status},
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
    fn native_owner(&self, run: &Json, call: &Json) -> IvyResult<NativeOwner> {
        let request = call.required("request")?;
        let source = catalog(self.catalogs, request)?;
        let target = run.required("target")?;
        Ok(NativeOwner::new(
            self.runtime.tools().clone(),
            self.runtime.principal_id().clone(),
            NativeOwnerIdentity {
                service_node_id: string(target, "serviceNodeId")?.clone(),
                host_id: target.get("hostId").and_then(Json::as_js_string).cloned(),
                native_version: string(request, "nativeVersion")?.clone(),
                native_executable_hash: string(source.catalog(), "nativeExecutableHash")?.clone(),
                catalog_hash: source.catalog_hash().into(),
            },
            RequestOptions::default(),
        ))
    }
    pub async fn status(&self, run: &Json, call: &Json) -> IvyResult<Json> {
        // The original reads before selecting the retained catalog. Keep that
        // failure order and its domain-specific readiness error.
        let value = management_read(
            self.runtime.tools(),
            string(run.required("target")?, "serviceNodeId")?,
            "status",
            Json::object([] as [(&str, Json); 0]),
        )
        .await?;
        validate_native_status(
            &value,
            self.native_owner(run, call)?.target(),
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
        if !current.active
            || truthy(field(run, "cancellation"))
            || truthy(field(run, "turnId"))
            || !truthy(pin)
            || !truthy(primary)
        {
            return Ok(false);
        }
        let turn_pin = field(field(run, "calls"), "turn");
        if truthy(turn_pin) {
            let turn = Store::read(
                self.store,
                "task-board/native-call",
                turn_pin.required("objectId")?,
                true,
            )
            .await?;
            if turn.pin.get("revision") != turn_pin.get("revision")
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
            || !truthy(field(&call.value, "evidence"))
            || !truthy(field(&call.value, "epoch"))
            || call.pin.get("revision") != pin.get("revision")
        {
            return Ok(false);
        }
        let identity_matches = field(primary, "namespace") == &Json::from("codex")
            && field(primary, "kind") == &Json::from("thread")
            && primary.get("serviceNodeId") == run.required("target")?.get("serviceNodeId");
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
        let status = self.status(run, &call.value).await?;
        if status.get("epoch") == call.value.get("epoch") {
            return Ok(false);
        }
        let read = self
            .native_owner(run, &call.value)?
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
fn truthy(v: &Json) -> bool {
    match v {
        Json::Null => false,
        Json::Bool(v) => *v,
        Json::Number(v) => *v != 0.0 && !v.is_nan(),
        Json::String(v) => !v.0.is_empty(),
        _ => true,
    }
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
        let task = Store::read(
            self.store,
            "task-board/task",
            run.value.required("taskId")?,
            true,
        )
        .await?;
        require(
            !["completed", "failed", "cancelled"]
                .iter()
                .any(|s| field(&run.value, "phase") == &Json::from(*s))
                && !truthy(field(&task.value, "publication")),
            "task_board_native_order",
            "Native execution requires an unfinished Run and no pending task publication.",
        )?;
        let claim_run = field(field(&task.value, "claim"), "run");
        let active = claim_run.get("objectId").and_then(Json::as_js_string) == Some(run_id);
        require(
            !active
                || canonical(claim_run, MANAGEMENT_FRAME_BYTES)?
                    == canonical(&run.pin, MANAGEMENT_FRAME_BYTES)?,
            "revision_conflict",
            "Native execution requires the exact current claimed Run revision.",
        )?;
        Ok(Current { run, task, active })
    }
    async fn update(&self, current: &Current, change: Json) -> IvyResult<Json> {
        let scope = Json::object([
            ("task", current.task.pin.clone()),
            ("run", current.run.pin.clone()),
            ("change", change.clone()),
        ]);
        let id = mutation(
            string(&current.run.pin, "objectId")?,
            &format!("native-update:{}", hash_json(&scope)?),
        )?;
        self.runtime
            .native_update(&Json::object([
                ("action", "nativeUpdate".into()),
                ("operationId", id.into()),
                ("taskId", current.task.pin.required("objectId")?.clone()),
                (
                    "expectedRevision",
                    current.task.pin.required("revision")?.clone(),
                ),
                ("run", current.run.pin.clone()),
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
        require(
            if slot == NativeSlot::Interrupt {
                truthy(field(&current.run.value, "cancellation"))
            } else {
                current.active && !truthy(field(&current.run.value, "cancellation"))
            },
            "task_board_claim_inactive",
            "Only an active uncancelled claim may prepare execution; interruption requires cancellation.",
        )?;
        let operation_id = self
            .runtime
            .scoped_operation_id(&Json::Array(vec![
                "task-board-native-call".into(),
                current.run.value.required("operationId")?.clone(),
                slot.as_str().into(),
                origin.clone(),
            ]))
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
                .intended_call(&current.run.value, slot, Some(origin), self.prompts)
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
        if field(field(&current.run.value, "calls"), slot.as_str()).get("objectId")
            == call.pin.get("objectId")
        {
            return Ok(call.pin);
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
            ("call", call.pin.clone()),
        ]);
        if field(origin, "kind") != &Json::from("reattach") {
            change.insert("slot", slot.as_str().into());
        }
        self.update(&current, change).await?;
        Ok(call.pin)
    }
}
