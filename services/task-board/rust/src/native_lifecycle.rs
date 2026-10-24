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
    IvyError, IvyJson as Json, IvyResult, JsString, agent_schema::validate_agent,
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
fn join(prefix: &str, v: &JsString) -> JsString {
    let mut s = JsString::from(prefix);
    s.0.extend(&v.0);
    s
}
fn call_key(method: &str, primary: &Json) -> IvyResult<JsString> {
    Ok(format!("{method}:{}", &hash_json(primary)?[7..]).into())
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
    let Some(error) = reply.get("error") else {
        return Ok(false);
    };
    let message = s(error, "message")?.trim();
    Ok(ivy_sdk::lowercase_utf16(&message.0)
        == ivy_sdk::lowercase_utf16(&join("unknown thread: ", thread).0))
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
                && status
                    .get("epoch")
                    .and_then(Json::as_js_string)
                    .is_some_and(|epoch| !epoch.0.is_empty()),
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
        let saved = if let Some(saved) = self.retained(operation, key).await? {
            saved
        } else {
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
            let target = identity_value(owner.target());
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
                        ("owner", target),
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
        };
        let owner = self.saved_owner(&saved)?;
        let call = NativeOperationCall {
            operation_id: s(&saved, "operationId")?.clone(),
            method: s(&saved, "method")?.clone(),
            params: saved.required("params")?.clone(),
            definition_hash: s(&saved, "definitionHash")?.clone(),
        };
        let native_owner = &owner;
        let result = reconcile_native_operation_local(
            native_owner,
            &call,
            NativeOperationJournal {
                current: saved.clone(),
                previous: saved
                    .get("observation")
                    .filter(|value| !value.is_null())
                    .cloned(),
                was_observed: !field(&saved, "observation").is_null(),
                retain: |observed: Json| async move {
                    self.operations
                        .observe_native(s(&operation.pin, "objectId")?, key, &observed)
                        .await
                },
            },
            Some(
                move |original: NativeOperationCall, absence: Json| async move {
                    let status = native_owner.status().await?;
                    require(
                        status.get("epoch") == absence.get("epoch"),
                        "task_board_native_epoch_changed",
                        "Native absence and readiness must identify the same owner epoch.",
                    )?;
                    native_owner
                        .dispatch_local(&original, || self.dispatch_guard(operation))
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
        Err(IvyError::new(
            "task_board_native_outcome_unresolved",
            "The original native lifecycle outcome is pending; no replacement task is authorized yet.",
        ).with_outcome(ivy_contracts::Outcome::Unknown))
    }
    async fn dispatch_guard(&self, operation: &Document) -> IvyResult<()> {
        self.engine.verify_owner().await?;
        let request = operation.value.required("request")?;
        require(
            request.get("taskId").is_some(),
            "task_board_native_intent_mismatch",
            "Native lifecycle work requires its original ticket.",
        )?;
        let current = self
            .store
            .lifecycle_read("task-board/task", request.required("taskId")?, true, true)
            .await?
            .document;
        require(
            field(&current.pin, "revision") == field(request, "expectedRevision")
                && field(&current.value, "claim").is_null()
                && field(&current.value, "publication").is_null(),
            "revision_conflict",
            "The ticket changed before native lifecycle dispatch.",
        )
    }
    async fn archive_thread(
        &self,
        operation: &Document,
        primary: &Json,
        archived: bool,
        only_archived: bool,
    ) -> IvyResult<bool> {
        require(
            eq(primary, "namespace", "codex") && eq(primary, "kind", "thread"),
            "task_board_primary_mismatch",
            "The saved native context must be a Codex thread.",
        )?;
        let method = if archived {
            "thread/archive"
        } else {
            "thread/unarchive"
        };
        let key = call_key(method, primary)?;
        let mut selected = None;
        if self.retained(operation, &key).await?.is_none() {
            let owner = self.owner(s(primary, "serviceNodeId")?).await?;
            let read = owner
                .read(
                    &"thread/read".into(),
                    Json::object([
                        ("threadId", primary.required("nativeId")?.clone()),
                        ("includeTurns", false.into()),
                    ]),
                    None,
                    true,
                )
                .await?;
            let reply = read.required("reply")?;
            if reply.get("result").is_some() {
                let thread = field(field(reply, "result"), "thread");
                require(
                    field(thread, "id") == field(primary, "nativeId")
                        && field(thread, "ephemeral").as_bool() == Some(false),
                    "task_board_primary_mismatch",
                    "The native observation must identify the saved durable thread.",
                )?;
                let path = field(thread, "path")
                    .as_js_string()
                    .cloned()
                    .unwrap_or_default();
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
            } else if missing(reply, s(primary, "nativeId")?)? {
                return Ok(archived || only_archived);
            } else {
                let message = reply.required("error")?.required("message")?;
                let message = if message.is_null() {
                    JsString::default()
                } else {
                    message
                        .as_js_string()
                        .cloned()
                        .ok_or_else(IvyError::internal)?
                };
                return Err(IvyError::new_js(
                    "task_board_native_read_failed".into(),
                    message,
                ));
            }
            selected = Some(owner);
        }
        let observed = self
            .call(operation, &key, || async {
                let owner = match selected {
                    Some(owner) => owner,
                    None => self.owner(s(primary, "serviceNodeId")?).await?,
                };
                Ok((
                    owner,
                    method.into(),
                    Json::object([("threadId", primary.required("nativeId")?.clone())]),
                ))
            })
            .await?;
        let reply = field(&observed, "reply");
        if eq(&observed, "phase", "succeeded") {
            if !archived {
                let thread = field(field(reply, "result"), "thread");
                require(
                    field(thread, "id") == field(primary, "nativeId")
                        && field(thread, "ephemeral").as_bool() == Some(false),
                    "task_board_primary_mismatch",
                    "Native restoration must confirm the original durable thread.",
                )?;
            }
            return Ok(true);
        }
        if archived && missing(reply, s(primary, "nativeId")?)? {
            return Ok(true);
        }
        if !archived {
            return Ok(false);
        }
        let message = if reply.get("error").is_some() {
            let message = reply.required("error")?.required("message")?;
            if message.is_null() {
                JsString::default()
            } else {
                message
                    .as_js_string()
                    .cloned()
                    .ok_or_else(IvyError::internal)?
            }
        } else {
            "The Codex task could not be archived.".into()
        };
        Err(IvyError::new_js(
            "task_board_native_archive_failed".into(),
            message,
        ))
    }
    pub async fn restore_for_execution(
        &self,
        operation: &Document,
        task: &Document,
    ) -> IvyResult<()> {
        let request = operation.value.required("request")?;
        let primary = field(&task.value, "primaryResourceRef");
        require(
            eq(request, "action", "continue")
                && !primary.is_null()
                && field(primary, "serviceNodeId")
                    == field(field(request, "target"), "serviceNodeId"),
            "task_board_primary_mismatch",
            "Continuation must retain its saved native owner and context.",
        )?;
        require(
            self.archive_thread(operation, primary, false, true).await?,
            "task_board_native_restore_failed",
            "The archived native context could not be restored; no replacement was created.",
        )
    }
    async fn contexts(&self, task: &Document, history: bool) -> IvyResult<Vec<Json>> {
        let mut unique = Vec::<(String, Json)>::new();
        let mut add = |primary: &Json| -> IvyResult<()> {
            if !primary.is_null() {
                let hash = hash_json(primary)?;
                require(
                    eq(primary, "namespace", "codex") && eq(primary, "kind", "thread"),
                    "task_board_primary_mismatch",
                    "The saved native context must be a Codex thread.",
                )?;
                if let Some((_, previous)) = unique.iter_mut().find(|(key, _)| key == &hash) {
                    *previous = primary.clone();
                } else {
                    unique.push((hash, primary.clone()));
                }
            }
            Ok(())
        };
        add(field(&task.value, "primaryResourceRef"))?;
        for key in ["task-board/run", "task-board/history"] {
            if key == "task-board/history" && !history {
                break;
            }
            let mut cursor = None::<JsString>;
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
                if let Some(cursor) = cursor.take() {
                    options.insert("cursor", Json::String(cursor));
                }
                let page = self.store.page(key, &options).await?;
                for item in array(field(&page, "items")) {
                    let pin = Json::object([
                        ("objectId", field(item, "objectId").clone()),
                        ("revision", field(item, "revision").clone()),
                    ]);
                    let document = self.store.read(key, &pin, true).await?;
                    if key == "task-board/run" {
                        require(
                            ["completed", "failed", "cancelled"]
                                .iter()
                                .any(|phase| eq(&document.value, "phase", phase)),
                            "task_board_execution_unresolved",
                            "Resolve every native execution attempt before changing the ticket context.",
                        )?;
                        add(field(&document.value, "primaryResourceRef"))?;
                    } else {
                        let previous = field(&document.value, "previousTask");
                        if previous.is_null() {
                            continue;
                        }
                        require(
                            field(previous, "objectId") == field(&task.pin, "objectId"),
                            "task_board_history_mismatch",
                            "Previous native contexts must belong to this ticket.",
                        )?;
                        let result = async {
                            let old = self
                                .store
                                .lifecycle_read("task-board/task", previous, true, true)
                                .await?;
                            add(field(&old.document.value, "primaryResourceRef"))
                        }
                        .await;
                        if let Err(error) = result {
                            if error.code != "revision_pruned" {
                                return Err(error);
                            }
                        }
                    }
                }
                cursor = field(&page, "nextCursor")
                    .as_js_string()
                    .filter(|text| !text.0.is_empty())
                    .cloned();
                if cursor.is_none() {
                    break;
                }
            }
        }
        Ok(unique.into_iter().map(|(_, primary)| primary).collect())
    }
    async fn plan(
        &self,
        operation: &Document,
        task: &Document,
        previous: Option<&Json>,
    ) -> IvyResult<(NativeOwner, Json)> {
        let operation_id = self
            .engine
            .scoped_operation_id(&Json::Array(vec![
                "task-board-new-thread-plan".into(),
                operation.pin.required("objectId")?.clone(),
            ]))
            .await?;
        let allocation = self.engine.allocate(task, &operation_id).await?;
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
            field(&plan, "nativeVersion").as_js_string() == Some(&owner.target().native_version),
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
        let reply = field(&observed, "reply");
        if !eq(&observed, "phase", "succeeded") || reply.is_null() || reply.get("result").is_none()
        {
            let message = if !reply.is_null() && reply.get("error").is_some() {
                let error = field(reply, "error");
                match field(error, "message") {
                    Json::Null => JsString::default(),
                    value => value
                        .as_js_string()
                        .cloned()
                        .ok_or_else(IvyError::internal)?,
                }
            } else {
                "The new Codex task could not be started.".into()
            };
            return Err(IvyError::new_js(
                "task_board_new_thread_failed".into(),
                message,
            ));
        }
        let result = object(field(reply, "result"));
        let thread = object(field(&result, "thread"));
        let id = field(&thread, "id")
            .as_js_string()
            .cloned()
            .unwrap_or_default();
        let previous = field(field(&task.value, "primaryResourceRef"), "nativeId");
        require(
            field(&thread, "ephemeral") == &Json::Bool(false) && field(&thread, "id") != previous,
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
        let request = field(&operation.value, "request");
        let waiting = field(&task.value, "waiting");
        require(
            eq(request, "action", "recoverThread")
                && eq(field(&operation.value, "actor"), "source", "scheduler")
                && field(&operation.value, "callerPrincipalId").as_js_string()
                    == Some(self.engine.principal_id())
                && eq(field(&task.value, "fields"), "control", "agent")
                && (eq(&task.value, "workflowState", "todo")
                    || eq(&task.value, "workflowState", "waiting"))
                && (waiting.is_null() || eq(waiting, "reason", "user"))
                && field(&task.value, "blocker").is_null()
                && same(field(request, "run"), field(&task.value, "lastRun"))?,
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
        let primary = field(&task.value, "primaryResourceRef");
        let run = self
            .store
            .read("task-board/run", request.required("run")?, true)
            .await?;
        let key = call_key("thread/resume", primary)?;
        // Planning and primary admission belong inside the retained-call owner:
        // a saved terminal call must not allocate or prepare another request.
        let observed = self
            .call(operation, &key, || async {
                let (owner, plan) = self.plan(operation, task, Some(&run.value)).await?;
                let mut params = object(plan.required("threadResume")?);
                params.insert("threadId", primary.required("nativeId")?.clone());
                params.insert("excludeTurns", true.into());
                Ok((owner, "thread/resume".into(), params))
            })
            .await?;
        let reply = field(&observed, "reply");
        if eq(&observed, "phase", "succeeded") {
            let result = object(field(reply, "result"));
            let thread = object(field(&result, "thread"));
            require(
                field(&thread, "id") == field(primary, "nativeId")
                    && field(&thread, "ephemeral") == &Json::Bool(false),
                "task_board_primary_mismatch",
                "Recovery must confirm the original durable thread before reusing it.",
            )?;
            return Ok(primary.clone());
        }
        require(
            missing_native_thread(reply, s(primary, "nativeId")?)?,
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
        let id = field(&task.pin, "objectId");
        let current = self
            .store
            .lifecycle_read("task-board/task", id, true, true)
            .await?;
        let saved = self
            .store
            .read(
                "task-board/operation",
                field(&operation.pin, "objectId"),
                true,
            )
            .await?;
        let mut latest = &Json::Null;
        for write in array(field(&saved.value, "writes")) {
            let outcome = field(write, "outcome");
            if eq(write, "contractKey", "task-board/task") && field(outcome, "objectId") == id {
                latest = outcome;
            }
        }
        let revision = latest
            .get("revision")
            .filter(|value| !value.is_null())
            .unwrap_or(field(&task.pin, "revision"));
        require(
            field(&current.document.pin, "revision") == revision
                || field(field(&current.document.value, "publication"), "objectId")
                    == field(&operation.pin, "objectId"),
            "revision_conflict",
            "The ticket changed before its archive membership was saved.",
        )?;
        let metadata = current.metadata;
        if !field(&metadata, "archivedAt").is_null() == archived
            && field(&metadata, "effectivelyArchived") == &Json::Bool(archived)
        {
            return Ok(metadata);
        }
        let revision = field(&current.document.pin, "revision").clone();
        let mutation = self
            .operations
            .archive_identity(s(&operation.pin, "objectId")?)
            .await?;
        let result = self
            .store
            .archive(&Json::object([
                ("objectId", id.clone()),
                ("archived", archived.into()),
                ("expectedRevision", revision),
                ("mutationId", Json::String(mutation)),
            ]))
            .await?;
        require(
            field(&result, "id") == id
                && field(&result, "parentId")
                    == &self
                        .store
                        .root_object_id()
                        .map(Json::String)
                        .unwrap_or(Json::Null)
                && field(&result, "effectivelyArchived") == &Json::Bool(archived),
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
        primary: Json,
        start: bool,
    ) -> IvyResult<()> {
        let at = field(&operation.value, "createdAt");
        let request = field(&operation.value, "request");
        let mut value = object(&task.value);
        value.insert("primaryResourceRef", primary.clone());
        value.insert("publication", steps.pin()?);
        value.insert("updatedAt", at.clone());
        if start {
            value.insert("workflowState", "todo".into());
            value.insert("waiting", Json::Null);
            value.insert("acceptedReview", Json::Null);
            let mut fields = object(task.value.required("fields")?);
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
            if same(&primary, field(&task.value, "primaryResourceRef"))? {
                "Automatically restored the unstarted native context; queued ticket input was retained.".into()
            } else {
                "Retired the missing, conclusively unstarted native context and queued a fresh Run; the owner, workspace and queued ticket input were retained.".into()
            }
        } else {
            "The original Codex task could not be restored. A new native task was created; the ticket workflow state was retained.".into()
        };
        let history = Json::object([
            ("schemaVersion", Json::Number(1.0)),
            ("taskId", task.pin.required("objectId")?.clone()),
            (
                "operationId",
                field(&operation.value, "operationId").clone(),
            ),
            ("actor", field(&operation.value, "actor").clone()),
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
        ]);
        outcome.insert(
            "history",
            steps
                .create("task-board/history", &history, &"History".into())
                .await?,
        );
        value.insert("lastHistory", field(outcome, "history").clone());
        value.insert("publication", Json::Null);
        outcome.insert(
            "task",
            steps
                .write(
                    "task-board/task",
                    &value,
                    &Json::object([
                        ("objectId", task.pin.required("objectId")?.clone()),
                        ("expectedRevision", reserved.required("revision")?.clone()),
                    ]),
                )
                .await?,
        );
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
                .any(|action| eq(request, "action", action)),
            "task_board_native_intent_mismatch",
            "Only retained ticket lifecycle requests reach this program.",
        )?;
        let address = Json::object([
            ("objectId", request.required("taskId")?.clone()),
            ("revision", request.required("expectedRevision")?.clone()),
        ]);
        let current = self
            .store
            .lifecycle_read("task-board/task", &address, true, true)
            .await?;
        let task = current.document;
        if array(field(&operation.value, "writes")).is_empty() {
            require(
                field(&current.metadata, "currentRevision") == field(request, "expectedRevision"),
                "revision_conflict",
                "Reload the ticket before changing its native context.",
            )?;
            require(
                field(&task.value, "claim").is_null()
                    && field(&task.value, "publication").is_null(),
                "task_board_execution_unresolved",
                "Finish the current execution and publication before changing the ticket context.",
            )?;
            require(
                eq(request, "action", "archive")
                    || field(&current.metadata, "effectivelyArchived") != &Json::Bool(true),
                "task_board_record_archived",
                "Restore the ticket before requesting new work.",
            )?;
            self.engine
                .aggregate(&task.value, s(&task.pin, "objectId")?)
                .await?;
        }
        // Admission and aggregate errors remain outside the native-outcome catch.
        let result: IvyResult<()> = async {
            if eq(request, "action", "recoverThread") {
                let primary = self.recover_thread(operation, &task).await?;
                self.replace(operation, steps, &mut outcome, &task, primary, true)
                    .await?;
                return Ok(());
            }
            let archived = field(request, "archived") == &Json::Bool(true);
            let contexts = self
                .contexts(&task, eq(request, "action", "archive") && archived)
                .await?;
            if eq(request, "action", "newThread") {
                require(
                    self.engine.scheduler_enabled(),
                    "task_board_scheduler_disabled",
                    "New Codex work requires enabled TaskBoard scheduling.",
                )?;
                let primary = self.new_thread(operation, &task).await?;
                self.replace(operation, steps, &mut outcome, &task, primary, true)
                    .await?;
            } else {
                let primary = field(&task.value, "primaryResourceRef");
                let mut replacement = Json::Null;
                if archived {
                    for context in contexts {
                        self.archive_thread(operation, &context, true, false)
                            .await?;
                    }
                } else if !primary.is_null()
                    && !self
                        .archive_thread(operation, primary, false, false)
                        .await?
                {
                    replacement = self.new_thread(operation, &task).await?;
                }
                self.membership(operation, &task, archived).await?;
                if !replacement.is_null() {
                    self.replace(operation, steps, &mut outcome, &task, replacement, false)
                        .await?;
                }
                if field(&outcome, "task").is_null() {
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
                return Err(error.with_outcome(ivy_contracts::Outcome::Unknown));
            }
            return Err(error);
        }
        Ok(outcome)
    }
}
