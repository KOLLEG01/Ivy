//! Retained native context changes, independent of native Run dispatch.
//! Allocation, aggregate admission and missing-context proof remain explicit.
use crate::{
    inspection_journal::verify_owner,
    native_inspector::management_read,
    native_plan::read_plan,
    native_recovery::missing_native_thread,
    operations::{Document, Operations, Store},
    publication::Publication,
};
use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString, Outcome, agent_schema::validate_agent,
    checked_native_contract::CheckedNativeContracts, hash::hash_json, json::canonical,
    limits::MANAGEMENT_FRAME_BYTES,
};
use ivy_sdk::{
    native_evidence::NativeOwnerIdentity,
    native_operation::{
        NativeOperationCall, NativeOperationJournal, NativeReconciliation,
        reconcile_native_operation_local,
    },
    native_owner::NativeOwner,
    operation_ids::scoped_operation_id,
    rpc::{RequestOptions, RpcClient},
    tools::ToolClients,
};
use std::sync::Arc;

pub struct LifecycleDocument {
    pub document: Document,
    pub metadata: Json,
}
/// Page and archival operations use the admitted current store and its exact
/// readable versions. No local/remote paging or archive policy is guessed here.
#[allow(async_fn_in_trait)]
pub trait LifecycleStore: Store {
    async fn lifecycle_read(
        &self,
        key: &str,
        address: &Json,
        primary: bool,
        archived: bool,
    ) -> IvyResult<LifecycleDocument>;
    async fn page(&self, key: &str, options: &Json) -> IvyResult<Json>;
    async fn archive(&self, params: &Json) -> IvyResult<Json>;
    async fn stat(&self, id: &JsString) -> IvyResult<Json>;
    fn release_workspace(&self, id: &JsString) -> IvyResult<()>;
}
#[allow(async_fn_in_trait)]
pub trait LifecyclePolicies {
    fn scheduler_enabled(&self) -> bool;
    async fn aggregate(&self, task: &Json, id: &JsString) -> IvyResult<()>;
    async fn allocate(&self, task: &Document, operation_id: &JsString) -> IvyResult<Json>;
    async fn unstarted_missing_context(&self, task: &Document) -> IvyResult<bool>;
}
#[allow(async_fn_in_trait)]
pub trait LifecycleEngine: LifecyclePolicies {
    fn client(&self) -> &dyn RpcClient;
    fn tools(&self) -> &Arc<ToolClients>;
    fn principal_id(&self) -> &JsString;
    async fn verify_owner(&self) -> IvyResult<()>;
    async fn scoped_operation_id(&self, scope: &Json) -> IvyResult<JsString>;
}
/// Bind the same authenticated connection as the journal and explicit current
/// engine policies. This adapter supplies no prompt, question or allocation default.
pub struct LifecycleRuntime<R, P> {
    pub client: Arc<R>,
    pub principal_id: JsString,
    pub service_node_id: JsString,
    pub policies: P,
    tools: Arc<ToolClients>,
}
impl<R: RpcClient + 'static, P> LifecycleRuntime<R, P> {
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
impl<R: RpcClient + 'static, P: LifecyclePolicies> LifecyclePolicies for LifecycleRuntime<R, P> {
    fn scheduler_enabled(&self) -> bool {
        self.policies.scheduler_enabled()
    }
    async fn aggregate(&self, t: &Json, id: &JsString) -> IvyResult<()> {
        self.policies.aggregate(t, id).await
    }
    async fn allocate(&self, t: &Document, id: &JsString) -> IvyResult<Json> {
        self.policies.allocate(t, id).await
    }
    async fn unstarted_missing_context(&self, t: &Document) -> IvyResult<bool> {
        self.policies.unstarted_missing_context(t).await
    }
}
impl<R: RpcClient + 'static, P: LifecyclePolicies> LifecycleEngine for LifecycleRuntime<R, P> {
    fn client(&self) -> &dyn RpcClient {
        self.client.as_ref()
    }
    fn tools(&self) -> &Arc<ToolClients> {
        &self.tools
    }
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
#[allow(async_fn_in_trait)]
pub trait LifecycleOperations {
    async fn prepare_native(&self, id: &JsString, call: &Json) -> IvyResult<Json>;
    async fn observe_native(
        &self,
        id: &JsString,
        key: &JsString,
        observed: &Json,
    ) -> IvyResult<Json>;
    async fn archive_identity(&self, id: &JsString) -> IvyResult<JsString>;
}
impl<S: Store, C: Fn() -> String> LifecycleOperations for Operations<S, C> {
    async fn prepare_native(&self, id: &JsString, c: &Json) -> IvyResult<Json> {
        Operations::prepare_native(self, id, c).await
    }
    async fn observe_native(&self, id: &JsString, k: &JsString, o: &Json) -> IvyResult<Json> {
        Operations::observe_native(self, id, k, o).await
    }
    async fn archive_identity(&self, id: &JsString) -> IvyResult<JsString> {
        Operations::archive_identity(self, id).await
    }
}
#[allow(async_fn_in_trait)]
pub trait LifecyclePublication {
    fn pin(&self) -> IvyResult<Json>;
    async fn write(&mut self, key: &str, value: &Json, destination: &Json) -> IvyResult<Json>;
    async fn create(&mut self, key: &str, value: &Json, label: &JsString) -> IvyResult<Json>;
}
impl<S: Store, C: Fn() -> String> LifecyclePublication for Publication<'_, S, C> {
    fn pin(&self) -> IvyResult<Json> {
        Publication::pin(self)
    }
    async fn write(&mut self, k: &str, v: &Json, d: &Json) -> IvyResult<Json> {
        Publication::write(self, k, v, d).await
    }
    async fn create(&mut self, k: &str, v: &Json, l: &JsString) -> IvyResult<Json> {
        Publication::create(self, k, v, l).await
    }
}
pub struct NativeLifecycle<'a, S, E, O> {
    pub store: &'a S,
    pub engine: &'a E,
    pub operations: &'a O,
    pub catalogs: &'a CheckedNativeContracts,
}
fn field<'a>(v: &'a Json, k: &str) -> &'a Json {
    static NULL: Json = Json::Null;
    v.get(k).unwrap_or(&NULL)
}
fn s<'a>(v: &'a Json, k: &str) -> IvyResult<&'a JsString> {
    v.required(k)?.as_js_string().ok_or_else(IvyError::internal)
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
fn eq(v: &Json, k: &str, t: &str) -> bool {
    v.get(k) == Some(&Json::from(t))
}
fn array(v: &Json) -> &[Json] {
    v.as_array().unwrap_or(&[])
}
fn require(ok: bool, code: &str, message: &str) -> IvyResult<()> {
    if ok {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn same(a: &Json, b: &Json) -> IvyResult<bool> {
    Ok(canonical(a, MANAGEMENT_FRAME_BYTES)? == canonical(b, MANAGEMENT_FRAME_BYTES)?)
}
fn object(v: &Json) -> Json {
    if v.as_object().is_some() {
        v.clone()
    } else {
        Json::Object(vec![])
    }
}
fn string(v: &Json) -> JsString {
    v.as_js_string().cloned().unwrap_or_default()
}
fn join(prefix: &str, v: &JsString) -> JsString {
    let mut s = JsString::from(prefix);
    s.0.extend(&v.0);
    s
}
fn call_key(method: &str, primary: &Json) -> IvyResult<JsString> {
    Ok(format!("{method}:{}", &hash_json(primary)?[7..]).into())
}
fn primary(v: &Json) -> IvyResult<&Json> {
    require(
        eq(v, "namespace", "codex") && eq(v, "kind", "thread"),
        "task_board_primary_mismatch",
        "The saved native context must be a Codex thread.",
    )?;
    Ok(v)
}
fn path_has(path: &JsString, segment: &str) -> bool {
    let p: Vec<u16> = segment.encode_utf16().collect();
    path.0.windows(p.len() + 2).any(|w| {
        [47, 92].contains(&w[0]) && w[1..w.len() - 1] == p && [47, 92].contains(w.last().unwrap())
    })
}
fn missing(reply: &Json, thread: &JsString) -> IvyResult<bool> {
    if missing_native_thread(reply, thread)? {
        return Ok(true);
    }
    if !truthy(reply) || reply.get("error").is_none() {
        return Ok(false);
    }
    let mut message = s(reply.required("error")?, "message")?.clone();
    message.0.reverse();
    message = message.trim_start();
    message.0.reverse();
    message = message.trim_start();
    Ok(message.to_lowercase() == join("unknown thread: ", thread).to_lowercase())
}
fn identity(v: &Json) -> IvyResult<NativeOwnerIdentity> {
    Ok(NativeOwnerIdentity {
        service_node_id: s(v, "serviceNodeId")?.clone(),
        host_id: v.get("hostId").and_then(Json::as_js_string).cloned(),
        native_version: s(v, "nativeVersion")?.clone(),
        native_executable_hash: s(v, "nativeExecutableHash")?.clone(),
        catalog_hash: s(v, "catalogHash")?.clone(),
    })
}
fn identity_value(v: &NativeOwnerIdentity) -> Json {
    Json::object([
        ("serviceNodeId", Json::String(v.service_node_id.clone())),
        (
            "hostId",
            v.host_id.clone().map(Json::String).unwrap_or(Json::Null),
        ),
        ("nativeVersion", Json::String(v.native_version.clone())),
        (
            "nativeExecutableHash",
            Json::String(v.native_executable_hash.clone()),
        ),
        ("catalogHash", Json::String(v.catalog_hash.clone())),
    ])
}
impl<S: LifecycleStore, E: LifecycleEngine, O: LifecycleOperations> NativeLifecycle<'_, S, E, O> {
    async fn retained(&self, operation: &Document, key: &JsString) -> IvyResult<Option<Json>> {
        let current = self
            .store
            .read(
                "task-board/operation",
                operation.pin.required("objectId")?,
                true,
            )
            .await?;
        Ok(array(field(&current.value, "nativeCalls"))
            .iter()
            .find(|c| c.get("key").and_then(Json::as_js_string) == Some(key))
            .cloned())
    }
    async fn owner(&self, node: &JsString) -> IvyResult<NativeOwner> {
        let registration = self
            .engine
            .client()
            .request(
                "serviceNodes.get",
                Json::object([("serviceNodeId", Json::String(node.clone()))]),
                RequestOptions::default(),
            )
            .await?;
        let status =
            management_read(self.engine.tools(), node, "status", Json::Object(vec![])).await?;
        validate_agent("Status", &status)?;
        require(
            eq(&registration, "serviceName", "agent-manager")
                && status.get("serviceNodeId").and_then(Json::as_js_string) == Some(node)
                && status.get("hostId") == registration.get("hostId")
                && status.get("nativeVersion") == registration.get("nativeVersion")
                && eq(&status, "state", "ready")
                && truthy(field(&status, "epoch")),
            "task_board_native_unavailable",
            "The native owner must be ready before changing its ticket context.",
        )?;
        Ok(NativeOwner::new(
            self.engine.tools().clone(),
            self.engine.principal_id().clone(),
            identity(&status)?,
            RequestOptions::default(),
        ))
    }
    fn saved_owner(&self, saved: &Json) -> IvyResult<NativeOwner> {
        Ok(NativeOwner::new(
            self.engine.tools().clone(),
            self.engine.principal_id().clone(),
            identity(saved.required("owner")?)?,
            RequestOptions::default(),
        ))
    }
    async fn call<F, Fut>(
        &self,
        operation: &Document,
        key: &JsString,
        prepare: F,
    ) -> IvyResult<Json>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = IvyResult<(NativeOwner, JsString, Json)>>,
    {
        let saved = match self.retained(operation, key).await? {
            Some(saved) => saved,
            None => {
                let (owner, method, params) = prepare().await?;
                let binding = owner.binding(&method, None).await?;
                require(
                    binding
                        .value()
                        .required("definition")?
                        .get("interfaceVersion")
                        .and_then(Json::as_js_string)
                        == Some(&owner.target().native_version),
                    "task_board_native_intent_mismatch",
                    "The lifecycle method must belong to the selected native version.",
                )?;
                let operation_id = self
                    .engine
                    .scoped_operation_id(&Json::Array(vec![
                        "task-board-lifecycle".into(),
                        operation.pin.required("objectId")?.clone(),
                        Json::String(key.clone()),
                    ]))
                    .await?;
                self.operations
                    .prepare_native(
                        s(&operation.pin, "objectId")?,
                        &Json::object([
                            ("key", Json::String(key.clone())),
                            ("owner", identity_value(owner.target())),
                            ("operationId", Json::String(operation_id)),
                            ("method", Json::String(method)),
                            ("params", params),
                            (
                                "definitionHash",
                                binding.value().required("definitionHash")?.clone(),
                            ),
                            ("observation", Json::Null),
                        ]),
                    )
                    .await?
            }
        };
        let owner = self.saved_owner(&saved)?;
        let call = NativeOperationCall {
            operation_id: s(&saved, "operationId")?.clone(),
            method: s(&saved, "method")?.clone(),
            params: saved.required("params")?.clone(),
            definition_hash: s(&saved, "definitionHash")?.clone(),
        };
        let owner_ref = &owner;
        let result = reconcile_native_operation_local(
            &owner,
            &call,
            NativeOperationJournal {
                current: saved.clone(),
                previous: saved.get("observation").filter(|v| !v.is_null()).cloned(),
                was_observed: field(&saved, "observation") != &Json::Null,
                retain: |observed: Json| async move {
                    self.operations
                        .observe_native(s(&operation.pin, "objectId")?, key, &observed)
                        .await
                },
            },
            Some(
                move |original: NativeOperationCall, absence: Json| async move {
                    let status = owner_ref.status().await?;
                    require(
                        status.get("epoch") == absence.get("epoch"),
                        "task_board_native_epoch_changed",
                        "Native absence and readiness must identify the same owner epoch.",
                    )?;
                    owner_ref
                        .dispatch_local(&original, || async {
                            self.engine.verify_owner().await?;
                            let request = operation.value.required("request")?;
                            require(
                                request.get("taskId").is_some(),
                                "task_board_native_intent_mismatch",
                                "Native lifecycle work requires its original ticket.",
                            )?;
                            let current = self
                                .store
                                .lifecycle_read(
                                    "task-board/task",
                                    request.required("taskId")?,
                                    true,
                                    true,
                                )
                                .await?;
                            require(
                                current.document.pin.get("revision")
                                    == request.get("expectedRevision")
                                    && !truthy(field(&current.document.value, "claim"))
                                    && !truthy(field(&current.document.value, "publication")),
                                "revision_conflict",
                                "The ticket changed before native lifecycle dispatch.",
                            )
                        })
                        .await
                },
            ),
            None,
        )
        .await?;
        if let NativeReconciliation::Observed { observation, .. } = result {
            if eq(&observation, "phase", "succeeded") || eq(&observation, "phase", "failed") {
                return Ok(observation);
            }
        }
        Err(IvyError::new("task_board_native_outcome_unresolved","The original native lifecycle outcome is pending; no replacement task is authorized yet.").with_outcome(Outcome::Unknown))
    }
    async fn archive_thread(
        &self,
        operation: &Document,
        p: &Json,
        archived: bool,
        only_archived: bool,
    ) -> IvyResult<bool> {
        primary(p)?;
        let method = if archived {
            "thread/archive"
        } else {
            "thread/unarchive"
        };
        let key = call_key(method, p)?;
        let mut selected = None;
        if self.retained(operation, &key).await?.is_none() {
            let owner = self.owner(s(p, "serviceNodeId")?).await?;
            let read = owner
                .read(
                    &"thread/read".into(),
                    Json::object([
                        ("threadId", p.required("nativeId")?.clone()),
                        ("includeTurns", false.into()),
                    ]),
                    None,
                    true,
                )
                .await?;
            let reply = read.required("reply")?;
            if let Some(result) = reply.get("result") {
                let thread = field(result, "thread");
                require(
                    thread.get("id") == p.get("nativeId")
                        && field(thread, "ephemeral") == &false.into(),
                    "task_board_primary_mismatch",
                    "The native observation must identify the saved durable thread.",
                )?;
                let path = string(field(thread, "path"));
                if only_archived && !path_has(&path, "archived_sessions") {
                    return Ok(true);
                }
                require(
                    !eq(field(thread, "status"), "type", "active"),
                    "task_board_native_not_idle",
                    "Finish the active Codex turn before changing this ticket context.",
                )?;
                if archived && path_has(&path, "archived_sessions")
                    || !archived && path_has(&path, "sessions")
                {
                    return Ok(true);
                }
            } else if missing(reply, s(p, "nativeId")?)? {
                return Ok(archived || only_archived);
            } else {
                return Err(IvyError::new_js(
                    "task_board_native_read_failed".into(),
                    s(reply.required("error")?, "message")?.clone(),
                ));
            }
            selected = Some(owner);
        }
        let observed = self
            .call(operation, &key, || async {
                let owner = match selected {
                    Some(owner) => owner,
                    None => self.owner(s(p, "serviceNodeId")?).await?,
                };
                Ok((
                    owner,
                    method.into(),
                    Json::object([("threadId", p.required("nativeId")?.clone())]),
                ))
            })
            .await?;
        if eq(&observed, "phase", "succeeded") {
            if !archived {
                let thread = field(field(field(&observed, "reply"), "result"), "thread");
                require(
                    thread.get("id") == p.get("nativeId")
                        && field(thread, "ephemeral") == &false.into(),
                    "task_board_primary_mismatch",
                    "Native restoration must confirm the original durable thread.",
                )?;
            }
            return Ok(true);
        }
        if archived && missing(field(&observed, "reply"), s(p, "nativeId")?)? {
            return Ok(true);
        }
        if !archived {
            return Ok(false);
        }
        Err(IvyError::new_js(
            "task_board_native_archive_failed".into(),
            field(&observed, "reply")
                .get("error")
                .map(|e| s(e, "message").cloned())
                .transpose()?
                .unwrap_or_else(|| "The Codex task could not be archived.".into()),
        ))
    }
    pub async fn restore_for_execution(
        &self,
        operation: &Document,
        task: &Document,
    ) -> IvyResult<()> {
        let request = operation.value.required("request")?;
        let p = field(&task.value, "primaryResourceRef");
        require(
            eq(request, "action", "continue")
                && truthy(p)
                && p.get("serviceNodeId") == field(request, "target").get("serviceNodeId"),
            "task_board_primary_mismatch",
            "Continuation must retain its saved native owner and context.",
        )?;
        require(
            self.archive_thread(operation, p, false, true).await?,
            "task_board_native_restore_failed",
            "The archived native context could not be restored; no replacement was created.",
        )
    }
    async fn contexts(&self, task: &Document, history: bool) -> IvyResult<Vec<Json>> {
        let mut unique = Vec::<(String, Json)>::new();
        let add = |v: &Json, unique: &mut Vec<(String, Json)>| -> IvyResult<()> {
            if truthy(v) {
                let hash = hash_json(v)?;
                primary(v)?;
                if let Some(item) = unique.iter_mut().find(|v| v.0 == hash) {
                    item.1 = v.clone()
                } else {
                    unique.push((hash, v.clone()))
                }
            }
            Ok(())
        };
        add(field(&task.value, "primaryResourceRef"), &mut unique)?;
        for key in if history {
            vec!["task-board/run", "task-board/history"]
        } else {
            vec!["task-board/run"]
        } {
            let mut cursor = None;
            loop {
                let mut options = Json::object([
                    ("limit", Json::Number(100.0)),
                    (
                        "where",
                        Json::object([
                            ("op", "eq".into()),
                            ("field", "data:/taskId".into()),
                            ("value", task.pin.required("objectId")?.clone()),
                        ]),
                    ),
                ]);
                if let Some(cursor) = cursor {
                    options.insert("cursor", Json::String(cursor));
                }
                let page = self.store.page(key, &options).await?;
                for item in array(field(&page, "items")) {
                    let address = Json::object([
                        ("objectId", item.required("objectId")?.clone()),
                        ("revision", item.required("revision")?.clone()),
                    ]);
                    let d = self.store.read(key, &address, true).await?;
                    if key == "task-board/run" {
                        require(
                            ["completed", "failed", "cancelled"]
                                .iter()
                                .any(|v| eq(&d.value, "phase", v)),
                            "task_board_execution_unresolved",
                            "Resolve every native execution attempt before changing the ticket context.",
                        )?;
                        add(field(&d.value, "primaryResourceRef"), &mut unique)?;
                    } else {
                        let previous = field(&d.value, "previousTask");
                        if !truthy(previous) {
                            continue;
                        }
                        require(
                            previous.get("objectId") == task.pin.get("objectId"),
                            "task_board_history_mismatch",
                            "Previous native contexts must belong to this ticket.",
                        )?;
                        match self
                            .store
                            .lifecycle_read("task-board/task", previous, true, true)
                            .await
                        {
                            Ok(d) => {
                                add(field(&d.document.value, "primaryResourceRef"), &mut unique)?
                            }
                            Err(e) if e.code == "revision_pruned" => {}
                            Err(e) => return Err(e),
                        }
                    }
                }
                cursor = field(&page, "nextCursor")
                    .as_js_string()
                    .filter(|s| !s.0.is_empty())
                    .cloned();
                if cursor.is_none() {
                    break;
                }
            }
        }
        Ok(unique.into_iter().map(|v| v.1).collect())
    }
    async fn plan(
        &self,
        operation: &Document,
        task: &Document,
        previous: Option<&Json>,
    ) -> IvyResult<(NativeOwner, Json)> {
        let id = self
            .engine
            .scoped_operation_id(&Json::Array(vec![
                "task-board-new-thread-plan".into(),
                operation.pin.required("objectId")?.clone(),
            ]))
            .await?;
        let allocation = self.engine.allocate(task, &id).await?;
        if let Some(previous) = previous {
            require(
                same(allocation.required("target")?, previous.required("target")?)?
                    && field(allocation.required("workspace")?, "canonicalCwd")
                        == field(previous.required("workspace")?, "canonicalCwd")
                    && field(allocation.required("workspace")?, "intendedPath")
                        == field(previous.required("workspace")?, "intendedPath"),
                "task_board_workspace_mismatch",
                "Automatic context recovery must retain the original owner and workspace.",
            )?;
        }
        let plan = read_plan(self.store, self.catalogs, allocation.required("intent")?).await?;
        let owner = self
            .owner(s(allocation.required("target")?, "serviceNodeId")?)
            .await?;
        require(
            plan.get("nativeVersion").and_then(Json::as_js_string)
                == Some(&owner.target().native_version),
            "task_board_native_epoch_changed",
            "The new native task must use its freshly allocated plan version.",
        )?;
        self.store.complete_local_workflow(
            s(allocation.required("intent")?, "objectId")?,
            s(&operation.value, "createdAt")?,
        )?;
        Ok((owner, plan))
    }
    async fn new_thread(&self, operation: &Document, task: &Document) -> IvyResult<Json> {
        let observed = self
            .call(operation, &"new-thread".into(), || async {
                let (owner, plan) = self.plan(operation, task, None).await?;
                Ok((
                    owner,
                    "thread/start".into(),
                    plan.required("threadStart")?.clone(),
                ))
            })
            .await?;
        if !eq(&observed, "phase", "succeeded") || field(&observed, "reply").get("result").is_none()
        {
            return Err(IvyError::new_js(
                "task_board_new_thread_failed".into(),
                field(&observed, "reply")
                    .get("error")
                    .map(|e| s(e, "message").cloned())
                    .transpose()?
                    .unwrap_or_else(|| "The new Codex task could not be started.".into()),
            ));
        }
        let thread = field(field(field(&observed, "reply"), "result"), "thread");
        let id = string(field(thread, "id"));
        require(
            !id.0.is_empty()
                && field(thread, "ephemeral") == &false.into()
                && thread.get("id") != field(&task.value, "primaryResourceRef").get("nativeId"),
            "task_board_primary_mismatch",
            "The new native task must return a distinct durable thread identity.",
        )?;
        Ok(Json::object([
            ("serviceNodeId", observed.required("serviceNodeId")?.clone()),
            ("namespace", "codex".into()),
            ("kind", "thread".into()),
            ("nativeId", Json::String(id)),
        ]))
    }
    async fn recover_thread(&self, operation: &Document, task: &Document) -> IvyResult<Json> {
        let request = operation.value.required("request")?;
        let t = &task.value;
        require(
            eq(request, "action", "recoverThread")
                && eq(field(&operation.value, "actor"), "source", "scheduler")
                && operation
                    .value
                    .get("callerPrincipalId")
                    .and_then(Json::as_js_string)
                    == Some(self.engine.principal_id())
                && eq(field(t, "fields"), "control", "agent")
                && ["todo", "waiting"]
                    .iter()
                    .any(|s| eq(t, "workflowState", s))
                && (!truthy(field(t, "waiting")) || eq(field(t, "waiting"), "reason", "user"))
                && !truthy(field(t, "blocker"))
                && same(field(request, "run"), field(t, "lastRun"))?,
            "task_board_forbidden",
            "Only the scheduler can recover this exact failed, agent-controlled attempt.",
        )?;
        if array(field(&operation.value, "nativeCalls")).is_empty() {
            require(
                self.engine.unstarted_missing_context(task).await?,
                "task_board_execution_unresolved",
                "Every attempt in the missing context must be conclusively unstarted.",
            )?;
        }
        let p = t.required("primaryResourceRef")?;
        let run = self
            .store
            .read("task-board/run", request.required("run")?, true)
            .await?;
        let observed = self
            .call(operation, &call_key("thread/resume", p)?, || async {
                let (owner, plan) = self.plan(operation, task, Some(&run.value)).await?;
                let mut params = object(plan.required("threadResume")?);
                params.insert("threadId", p.required("nativeId")?.clone());
                params.insert("excludeTurns", true.into());
                Ok((owner, "thread/resume".into(), params))
            })
            .await?;
        if eq(&observed, "phase", "succeeded") {
            let thread = field(field(field(&observed, "reply"), "result"), "thread");
            require(
                thread.get("id") == p.get("nativeId")
                    && field(thread, "ephemeral") == &false.into(),
                "task_board_primary_mismatch",
                "Recovery must confirm the original durable thread before reusing it.",
            )?;
            return Ok(p.clone());
        }
        require(
            missing_native_thread(field(&observed, "reply"), s(p, "nativeId")?)?,
            "task_board_native_resume_failed",
            "The original context was not confirmed missing; no replacement is authorized.",
        )?;
        Ok(Json::Null)
    }
    async fn membership(
        &self,
        operation: &Document,
        task: &Document,
        archived: bool,
    ) -> IvyResult<Json> {
        let current = self
            .store
            .lifecycle_read(
                "task-board/task",
                task.pin.required("objectId")?,
                true,
                true,
            )
            .await?;
        let op = self
            .store
            .read(
                "task-board/operation",
                operation.pin.required("objectId")?,
                true,
            )
            .await?;
        let latest = array(field(&op.value, "writes"))
            .iter()
            .rev()
            .find(|w| {
                eq(w, "contractKey", "task-board/task")
                    && field(w, "outcome").get("objectId") == task.pin.get("objectId")
            })
            .map(|w| field(w, "outcome"));
        require(
            current.document.pin.get("revision")
                == latest
                    .and_then(|v| v.get("revision"))
                    .or(task.pin.get("revision"))
                || field(&current.document.value, "publication").get("objectId")
                    == operation.pin.get("objectId"),
            "revision_conflict",
            "The ticket changed before its archive membership was saved.",
        )?;
        if (field(&current.metadata, "archivedAt") != &Json::Null) == archived
            && field(&current.metadata, "effectivelyArchived") == &Json::Bool(archived)
        {
            return Ok(current.metadata);
        }
        let result = self
            .store
            .archive(&Json::object([
                ("objectId", task.pin.required("objectId")?.clone()),
                ("archived", archived.into()),
                (
                    "expectedRevision",
                    current.document.pin.required("revision")?.clone(),
                ),
                (
                    "mutationId",
                    Json::String(
                        self.operations
                            .archive_identity(s(&operation.pin, "objectId")?)
                            .await?,
                    ),
                ),
            ]))
            .await?;
        require(
            result.get("id") == task.pin.get("objectId")
                && result.get("parentId")
                    == Some(
                        &self
                            .store
                            .root_object_id()
                            .map(Json::String)
                            .unwrap_or(Json::Null),
                    )
                && result.get("effectivelyArchived") == Some(&Json::Bool(archived)),
            "task_board_contract_mismatch",
            "Hive returned a different ticket archive outcome.",
        )?;
        Ok(result)
    }
    async fn replace<P: LifecyclePublication>(
        &self,
        operation: &Document,
        steps: &mut P,
        outcome: &mut Json,
        task: &Document,
        p: Json,
        start: bool,
    ) -> IvyResult<()> {
        let at = operation.value.required("createdAt")?;
        let request = operation.value.required("request")?;
        let mut value = task.value.clone();
        value.insert("primaryResourceRef", p.clone());
        value.insert("publication", steps.pin()?);
        value.insert("updatedAt", at.clone());
        if start {
            value.insert("workflowState", "todo".into());
            value.insert("waiting", Json::Null);
            value.insert("acceptedReview", Json::Null);
            let mut fields = value.required("fields")?.clone();
            fields.insert("control", "agent".into());
            fields.insert("nextReviewAt", Json::Null);
            value.insert("fields", fields);
        }
        let reserved = steps
            .write(
                "task-board/task",
                &value,
                &Json::object([
                    ("objectId", task.pin.required("objectId")?.clone()),
                    ("expectedRevision", task.pin.required("revision")?.clone()),
                ]),
            )
            .await?;
        let detail = if eq(request, "action", "newThread") {
            join(
                "Work explicitly requested in a new Codex task. ",
                s(request, "reason")?,
            )
        } else if eq(request, "action", "recoverThread") {
            if same(&p, field(&task.value, "primaryResourceRef"))? {
                "Automatically restored the unstarted native context; queued ticket input was retained.".into()
            } else {
                "Retired the missing, conclusively unstarted native context and queued a fresh Run; the owner, workspace and queued ticket input were retained.".into()
            }
        } else {
            "The original Codex task could not be restored. A new native task was created; the ticket workflow state was retained.".into()
        };
        let history = steps
            .create(
                "task-board/history",
                &Json::object([
                    ("schemaVersion", Json::Number(1.0)),
                    ("taskId", task.pin.required("objectId")?.clone()),
                    (
                        "operationId",
                        operation.value.required("operationId")?.clone(),
                    ),
                    ("actor", operation.value.required("actor")?.clone()),
                    ("createdAt", at.clone()),
                    (
                        "kind",
                        if start && !eq(request, "action", "recoverThread") {
                            "edited"
                        } else {
                            "recovered"
                        }
                        .into(),
                    ),
                    ("detail", Json::String(detail)),
                    ("previousTask", task.pin.clone()),
                    (
                        "previousHistory",
                        task.value.required("lastHistory")?.clone(),
                    ),
                    ("fromTarget", Json::Null),
                    ("toTarget", Json::Null),
                    ("priorRun", task.value.required("lastRun")?.clone()),
                    ("result", Json::Null),
                    ("review", Json::Null),
                ]),
                &"History".into(),
            )
            .await?;
        outcome.insert("history", history.clone());
        value.insert("lastHistory", history);
        value.insert("publication", Json::Null);
        let pin = steps
            .write(
                "task-board/task",
                &value,
                &Json::object([
                    ("objectId", task.pin.required("objectId")?.clone()),
                    ("expectedRevision", reserved.required("revision")?.clone()),
                ]),
            )
            .await?;
        outcome.insert("task", pin);
        Ok(())
    }
    pub async fn publish<P: LifecyclePublication>(
        &self,
        operation: &Document,
        steps: &mut P,
        mut outcome: Json,
    ) -> IvyResult<Json> {
        let request = operation.value.required("request")?;
        require(
            ["archive", "newThread", "recoverThread"]
                .iter()
                .any(|v| eq(request, "action", v)),
            "task_board_native_intent_mismatch",
            "Only retained ticket lifecycle requests reach this program.",
        )?;
        let task = self
            .store
            .lifecycle_read(
                "task-board/task",
                &Json::object([
                    ("objectId", request.required("taskId")?.clone()),
                    ("revision", request.required("expectedRevision")?.clone()),
                ]),
                true,
                true,
            )
            .await?;
        if array(field(&operation.value, "writes")).is_empty() {
            require(
                task.metadata.get("currentRevision") == request.get("expectedRevision"),
                "revision_conflict",
                "Reload the ticket before changing its native context.",
            )?;
            require(
                !truthy(field(&task.document.value, "claim"))
                    && !truthy(field(&task.document.value, "publication")),
                "task_board_execution_unresolved",
                "Finish the current execution and publication before changing the ticket context.",
            )?;
            require(
                eq(request, "action", "archive")
                    || !truthy(field(&task.metadata, "effectivelyArchived")),
                "task_board_record_archived",
                "Restore the ticket before requesting new work.",
            )?;
            self.engine
                .aggregate(&task.document.value, s(&task.document.pin, "objectId")?)
                .await?;
        }
        let task = &task.document;
        let result: IvyResult<()> = async {
            if eq(request, "action", "recoverThread") {
                let p = self.recover_thread(operation, task).await?;
                self.replace(operation, steps, &mut outcome, task, p, true)
                    .await?;
                return Ok(());
            }
            let archived = truthy(field(request, "archived"));
            let contexts = self
                .contexts(task, eq(request, "action", "archive") && archived)
                .await?;
            if eq(request, "action", "newThread") {
                require(
                    self.engine.scheduler_enabled(),
                    "task_board_scheduler_disabled",
                    "New Codex work requires enabled TaskBoard scheduling.",
                )?;
                let p = self.new_thread(operation, task).await?;
                self.replace(operation, steps, &mut outcome, task, p, true)
                    .await?;
            } else {
                let mut replacement = None;
                if archived {
                    for p in contexts {
                        self.archive_thread(operation, &p, true, false).await?;
                    }
                } else if truthy(field(&task.value, "primaryResourceRef"))
                    && !self
                        .archive_thread(
                            operation,
                            task.value.required("primaryResourceRef")?,
                            false,
                            false,
                        )
                        .await?
                {
                    replacement = Some(self.new_thread(operation, task).await?);
                }
                self.membership(operation, task, archived).await?;
                if let Some(p) = replacement {
                    self.replace(operation, steps, &mut outcome, task, p, false)
                        .await?;
                }
                if field(&outcome, "task") == &Json::Null {
                    outcome.insert("task", task.pin.clone());
                }
                outcome.insert("archive", self.store.stat(s(&task.pin, "objectId")?).await?);
                if archived {
                    self.store.release_workspace(s(&task.pin, "objectId")?)?;
                }
            }
            Ok(())
        }
        .await;
        if let Err(error) = result {
            if [
                "service_not_ready",
                "service_unavailable",
                "task_board_native_unavailable",
                "task_board_target_unavailable",
                "task_board_native_outcome_unresolved",
                "task_board_native_epoch_changed",
            ]
            .contains(&error.code.as_str())
            {
                return Err(error.with_outcome(Outcome::Unknown));
            }
            return Err(error);
        }
        Ok(outcome)
    }
}
