//! Task-owned native MCP contexts. No model turn is started by this transport.
use crate::{
    mcp_bridge::{LocalBridge, TaskMcp},
    mcp_context::{Context, Contexts, text},
    mcp_result::tool_result,
    model,
    runner::{McpBridge, McpBridgeFactory},
    store_owner::StoreOwner,
};
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString, Outcome, agent_schema::validate_agent,
    native_backend::BackendIdentity,
};
use ivy_sdk::{
    native_evidence::NativeOwnerIdentity,
    native_operation::NativeOperationCall,
    native_owner::{GuardFuture, NativeDispatchGuard, NativeOwner},
    operation_ids::new_operation_id,
    rpc::{RequestOptions, RpcClient},
    service::ServiceFuture,
    tools::{InterfaceRequirement, ToolCallOptions, ToolClients},
};
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::sync::Mutex as AsyncMutex;
use tokio_util::sync::CancellationToken;

fn object() -> IvyJson {
    model::object()
}
fn check(signal: &CancellationToken) -> IvyResult<()> {
    if signal.is_cancelled() {
        Err(IvyError::internal())
    } else {
        Ok(())
    }
}
struct Dispatch {
    operation_id: JsString,
    attempted: AtomicBool,
}
impl Dispatch {
    fn new(call: &NativeOperationCall) -> Self {
        Self {
            operation_id: call.operation_id.clone(),
            attempted: AtomicBool::new(false),
        }
    }
}
struct Guard<'a> {
    signal: &'a CancellationToken,
    dispatch: &'a Dispatch,
}
impl NativeDispatchGuard for Guard<'_> {
    fn check(&self) -> GuardFuture<'_> {
        Box::pin(async {
            check(self.signal)?;
            // The session lock keeps this proof alive if its caller is dropped.
            // Once permitted, a send remains possible until a definitive rejection.
            self.dispatch.attempted.store(true, Ordering::Release);
            Ok(())
        })
    }
}
fn absolute(path: &JsString) -> bool {
    #[cfg(windows)]
    {
        let p = &path.0;
        let separator = |c| c == 47 || c == 92;
        p.first().is_some_and(|c| separator(*c))
            || (p.len() >= 3
                && ((65..=90).contains(&p[0]) || (97..=122).contains(&p[0]))
                && p[1] == 58
                && separator(p[2]))
    }
    #[cfg(not(windows))]
    {
        path.0.first() == Some(&47)
    }
}
#[derive(Default)]
struct State {
    context: Option<Context>,
    closed: bool,
    dispatch: Option<Dispatch>,
}
struct Inner {
    client: Arc<dyn RpcClient>,
    tools: Arc<ToolClients>,
    configuration: IvyJson,
    backend: Option<BackendIdentity>,
    signal: CancellationToken,
    closed: CancellationToken,
    state: Arc<AsyncMutex<State>>,
    contexts: Contexts,
    task_id: Option<String>,
}
#[derive(Clone)]
pub struct NativeMcpSession(Arc<Inner>);
#[cfg(test)]
#[path = "../../../../tests/rust/data-collector-native-mcp.rs"]
mod tests;
impl NativeMcpSession {
    fn new(
        client: Arc<dyn RpcClient>,
        tools: Arc<ToolClients>,
        configuration: IvyJson,
        signal: CancellationToken,
        contexts: Contexts,
    ) -> IvyResult<Self> {
        let backend = BackendIdentity::optional(configuration.get("backend"))?;
        if !absolute(&text(&configuration, "cwd")?) {
            return Err(IvyError::new(
                "configuration_invalid",
                "MCP working directory must be absolute.",
            ));
        }
        Ok(Self(Arc::new(Inner {
            client,
            tools,
            configuration,
            backend,
            signal,
            closed: CancellationToken::new(),
            state: Arc::new(AsyncMutex::new(State::default())),
            contexts,
            task_id: None,
        })))
    }
    fn for_run(&self, signal: CancellationToken, task_id: String) -> Self {
        Self(Arc::new(Inner {
            client: self.0.client.clone(),
            tools: self.0.tools.clone(),
            configuration: self.0.configuration.clone(),
            backend: self.0.backend.clone(),
            signal,
            closed: self.0.closed.clone(),
            state: self.0.state.clone(),
            contexts: self.0.contexts.clone(),
            task_id: Some(task_id),
        }))
    }
    fn options(signal: &CancellationToken) -> RequestOptions {
        RequestOptions {
            cancellation: signal.clone(),
            cancel_error: IvyError::internal(),
            ..Default::default()
        }
    }
    async fn initialize(&self, state: &mut State) -> IvyResult<()> {
        if let Some(context) = &mut state.context {
            let native = owner(context, self.0.tools.clone(), &self.0.signal);
            let status = native.status().await?;
            if status.get("epoch").and_then(IvyJson::as_js_string) != Some(&context.epoch)
                || context.pending.is_some()
            {
                if self.0.configuration.get("readOnly") != Some(&true.into()) {
                    self.0.closed.cancel();
                    return Err(unresolved());
                }
                let _cleanup = self.0.contexts.cleanup.lock().await;
                self.0.contexts.mark_closing(context).await?;
                if let Some(mut saved) = self.0.contexts.read(context.key()?).await? {
                    retire_context(
                        &self.0.contexts,
                        &mut saved,
                        self.0.client.clone(),
                        self.0.tools.clone(),
                        &mut state.dispatch,
                    )
                    .await?;
                }
                state.context = None;
                state.dispatch = None;
            } else {
                if context.thread.is_none() {
                    let operation = native.operation(&context.start).await?;
                    context.thread = Some(started(result(context, &operation)?)?);
                    self.0.contexts.save(context).await?;
                }
                return Ok(());
            }
        }
        let service_node_id = text(&self.0.configuration, "serviceNodeId")?;
        let backend = self.0.backend.clone();
        let service = self.0.tools.service_tools(
            service_node_id.clone(),
            &[InterfaceRequirement {
                namespace: "agent".into(),
                interface_version: "1.0.0".into(),
            }],
        )?;
        let status = service
            .call(
                &"agent.status".into(),
                BackendIdentity::select_arguments(backend.as_ref(), object())?,
                None,
                ToolCallOptions::default(),
            )
            .await?;
        validate_agent("Status", &status)?;
        if status.get("serviceNodeId") != Some(&IvyJson::String(service_node_id))
            || !BackendIdentity::matches(&status, backend.as_ref())
        {
            return Err(IvyError::new(
                "native_owner_mismatch",
                "The MCP status must come from the selected service, backend and account.",
            ));
        }
        if model::optional_text(&status, "state").as_deref() != Some("ready")
            || status.get("epoch").is_none_or(|v| v.is_null())
        {
            return Err(IvyError::new(
                "provider_unavailable",
                "The MCP native owner is not ready.",
            ));
        }
        let identity = self
            .0
            .client
            .request("system.status", object(), RequestOptions::default())
            .await?;
        let target = NativeOwnerIdentity {
            backend,
            service_node_id: text(&status, "serviceNodeId")?,
            host_id: Some(text(&status, "hostId")?),
            native_version: text(&status, "nativeVersion")?,
            native_executable_hash: text(&status, "nativeExecutableHash")?,
            catalog_hash: text(&status, "catalogHash")?,
        };
        let owner = NativeOwner::new(
            self.0.tools.clone(),
            text(&identity, "callerPrincipalId")?,
            target.clone(),
            Self::options(&self.0.signal),
        );
        let contract = owner.selected_contract().await?;
        for method in [
            "config/read",
            "thread/start",
            "mcpServer/tool/call",
            "thread/unsubscribe",
        ] {
            contract.definition(&method.into())?;
        }
        let cwd = self.0.configuration.required("cwd")?.clone();
        let config = {
            // The management selector fences account changes between status,
            // discovery and this read. It creates no durable operation receipt.
            let read = prepare(
                &owner,
                self.0.client.clone(),
                "config/read",
                IvyJson::object([("cwd", cwd.clone()), ("includeLayers", false.into())]),
            )
            .await?;
            let observed = owner.interact(&read, None).await?;
            let effective = observed.required("reply")?.get("result").ok_or_else(|| {
                IvyError::new(
                    "native_read_failed",
                    "The selected backend configuration read failed.",
                )
            })?;
            let servers = effective.required("config")?.get("mcp_servers");
            let mut overrides = object();
            for name in ["node_repl", "cua_repl"] {
                // An override for an absent server creates an incomplete transport.
                if servers.and_then(|servers| servers.get(name)).is_some() {
                    overrides.insert(format!("mcp_servers.{name}.enabled"), false.into());
                }
            }
            overrides
        };
        let params = owner
            .thread_start_params(&IvyJson::object([
                ("cwd", cwd),
                ("ephemeral", true.into()),
                ("permissions", ":read-only".into()),
                ("approvalPolicy", "never".into()),
                ("config", config),
            ]))
            .await?;
        let start = prepare(&owner, self.0.client.clone(), "thread/start", params).await?;
        state.dispatch = Some(Dispatch::new(&start));
        // Retain identity in both the session and SQLite before dispatch can begin.
        state.context = Some(Context {
            revision: 0,
            session_id: self.0.contexts.session_id.clone(),
            caller: owner.caller().clone(),
            owner: target,
            epoch: text(&status, "epoch")?,
            start,
            thread: None,
            pending: None,
            unsubscribe: None,
            closing: false,
            task_id: self.0.task_id.clone(),
        });
        let context = state.context.as_mut().unwrap();
        self.0.contexts.insert(context, &self.0.closed).await?;
        let operation = dispatch(
            &owner,
            &context.start,
            &self.0.signal,
            state.dispatch.as_ref().unwrap(),
        )
        .await?;
        context.thread = Some(started(result(context, &operation)?)?);
        self.0.contexts.save(context).await?;
        Ok(())
    }
}
fn unresolved() -> IvyError {
    IvyError::new(
        "provider_unavailable",
        "The original MCP operation has not completed on the selected owner.",
    )
    .with_outcome(Outcome::Unknown)
}
fn owner(context: &Context, tools: Arc<ToolClients>, signal: &CancellationToken) -> NativeOwner {
    NativeOwner::new(
        tools,
        context.caller.clone(),
        context.owner.clone(),
        NativeMcpSession::options(signal),
    )
}
async fn prepare(
    owner: &NativeOwner,
    client: Arc<dyn RpcClient>,
    method: &str,
    params: IvyJson,
) -> IvyResult<NativeOperationCall> {
    let method = JsString::from(method);
    let contract = owner.selected_contract().await?;
    let expected = JsString::from(contract.definition_hash(&method)?);
    let binding = owner.binding(&method, Some(&expected)).await?;
    Ok(NativeOperationCall {
        operation_id: new_operation_id(client, None).await?,
        method,
        params,
        definition_hash: text(binding.value(), "definitionHash")?,
    })
}
async fn dispatch(
    owner: &NativeOwner,
    call: &NativeOperationCall,
    signal: &CancellationToken,
    admission: &Dispatch,
) -> IvyResult<IvyJson> {
    check(signal)?;
    let response = owner
        .dispatch(
            call,
            &Guard {
                signal,
                dispatch: admission,
            },
        )
        .await;
    if response
        .as_ref()
        .is_err_and(|error| error.outcome == Outcome::NotExecuted)
    {
        admission.attempted.store(false, Ordering::Release);
    }
    let mut operation = response?;
    if let Some(observation) = &operation {
        owner.check_operation(observation, call)?;
    }
    loop {
        if let Some(observation) = operation {
            match model::optional_text(&observation, "phase").as_deref() {
                Some("succeeded" | "failed" | "outcome_unknown") => return Ok(observation),
                Some("accepted" | "dispatched") => (),
                _ => return Err(unresolved()),
            }
        }
        tokio::select! {biased; _=signal.cancelled()=>return Err(unresolved()), _=tokio::time::sleep(Duration::from_millis(100))=>{}}
        operation = Some(owner.operation(call).await?);
    }
}
// NativeOwner::operation validates its receipt; dispatch validates the initial
// admission above. Only the selected epoch and domain outcome remain to check.
fn settled(context: &Context, operation: &IvyJson) -> IvyResult<()> {
    if operation.get("epoch").and_then(IvyJson::as_js_string) != Some(&context.epoch)
        || !matches!(
            model::optional_text(operation, "phase").as_deref(),
            Some("succeeded" | "failed")
        )
    {
        return Err(unresolved());
    }
    Ok(())
}
fn result<'a>(context: &Context, operation: &'a IvyJson) -> IvyResult<&'a IvyJson> {
    settled(context, operation)?;
    if model::optional_text(operation, "phase").as_deref() != Some("succeeded") {
        return Err(unresolved());
    }
    operation
        .get("reply")
        .and_then(|reply| reply.get("result"))
        .ok_or_else(unresolved)
}
fn started(value: &IvyJson) -> IvyResult<JsString> {
    let thread = value.required("thread")?;
    if thread.get("ephemeral").and_then(IvyJson::as_bool) != Some(true) {
        return Err(unresolved());
    }
    text(thread, "id")
}
async fn same_epoch(owner: &NativeOwner, epoch: &JsString) -> IvyResult<()> {
    if owner
        .status()
        .await?
        .get("epoch")
        .and_then(IvyJson::as_js_string)
        == Some(epoch)
    {
        Ok(())
    } else {
        Err(unresolved())
    }
}
/// A live caller can discard its own unsent intent. Missing receipts and records
/// recovered after a process restart carry no such authority.
async fn discard_unsent(
    contexts: &Contexts,
    context: &mut Context,
    admission: Option<&Dispatch>,
) -> IvyResult<bool> {
    let Some(admission) = admission.filter(|proof| !proof.attempted.load(Ordering::Acquire)) else {
        return Ok(false);
    };
    // Fence a save whose blocking store command outlived its canceled waiter.
    contexts.mark_closing(context).await?;
    let Some(mut saved) = contexts.read(context.key()?).await? else {
        return Ok(true);
    };
    if saved.thread.is_none() && saved.start.operation_id == admission.operation_id {
        contexts.retire(&saved).await?;
        return Ok(true);
    }
    let mut changed = false;
    for call in [&mut saved.pending, &mut saved.unsubscribe] {
        if call
            .as_ref()
            .is_some_and(|call| call.operation_id == admission.operation_id)
        {
            *call = None;
            changed = true;
        }
    }
    if changed {
        contexts.save(&mut saved).await?;
    }
    *context = saved;
    Ok(false)
}
async fn uncertain_read(
    contexts: &Contexts,
    context: &Context,
    owner: &NativeOwner,
) -> IvyResult<bool> {
    if let Some(call) = &context.pending {
        let operation = owner.operation(call).await?;
        if operation.get("epoch").and_then(IvyJson::as_js_string) != Some(&context.epoch) {
            return Err(unresolved());
        }
        if model::optional_text(&operation, "phase").as_deref() == Some("outcome_unknown") {
            let id = context.task_id.clone().ok_or_else(unresolved)?;
            let row = contexts
                .store
                .call(move |s| s.get(&id, false))
                .await?
                .ok_or_else(unresolved)?;
            let mcp = row.required("task")?.required("mcp")?;
            // This explicit operator contract also covers recovery of an already
            // recorded read. Filesystem permissions alone never imply safe MCP reads.
            if mcp.get("readOnly") != Some(&true.into())
                || mcp.get("serviceNodeId").and_then(IvyJson::as_js_string)
                    != Some(&context.owner.service_node_id)
                || !BackendIdentity::matches(mcp, context.owner.backend.as_ref())
                || call.method != JsString::from("mcpServer/tool/call")
                || call.params.get("threadId").and_then(IvyJson::as_js_string)
                    != context.thread.as_ref()
                || call.params.get("server") != mcp.get("server")
                || !mcp
                    .get("tools")
                    .and_then(IvyJson::as_array)
                    .is_some_and(|tools| tools.iter().any(|t| Some(t) == call.params.get("tool")))
            {
                return Err(unresolved());
            }
            return Ok(true);
        } else {
            settled(context, &operation)?;
        }
    }
    Ok(false)
}
/// A manager epoch change is not a native process stop. In particular, an
/// external server can still contain the old context and its outstanding work.
async fn retired_epoch(
    contexts: &Contexts,
    context: &Context,
    owner: &NativeOwner,
    client: Arc<dyn RpcClient>,
) -> IvyResult<bool> {
    let status = owner.status().await?;
    let epoch = text(&status, "epoch")?;
    if epoch == context.epoch {
        return Ok(false);
    }
    if status.get("state") != Some(&"ready".into()) {
        return Err(unresolved());
    }
    let thread = context.thread.as_ref().ok_or_else(unresolved)?;
    let mut uncertain = uncertain_read(contexts, context, owner).await?;
    if context.unsubscribe.is_some() {
        // Even uncertain local cleanup keeps its original identity. It cannot
        // affect an external system, but is never resent into the new epoch.
        uncertain = true;
    }
    let stopped = status.get("connection").is_some_and(|connection| {
        connection.get("ownsServer") == Some(&true.into())
            && connection.get("mode") == Some(&"owned-stdio".into())
    }) && status
        .get("processStops")
        .and_then(IvyJson::as_array)
        .is_some_and(|stops| {
            stops.iter().any(|stop| {
                stop.get("epoch").and_then(IvyJson::as_js_string) == Some(&context.epoch)
            })
        });
    let mut proof = IvyJson::object([
        ("epoch", IvyJson::String(epoch.clone())),
        ("observedAt", status.required("observedAt")?.clone()),
        (
            "reason",
            if stopped {
                "native_process_stopped"
            } else {
                "context_absent"
            }
            .into(),
        ),
    ]);
    if !stopped {
        let call = prepare(
            owner,
            client,
            "thread/loaded/list",
            IvyJson::object([("limit", 100.0.into())]),
        )
        .await?;
        same_epoch(owner, &epoch).await?;
        let observation = owner.interact(&call, None).await?;
        if observation.get("epoch").and_then(IvyJson::as_js_string) != Some(&epoch) {
            return Err(unresolved());
        }
        let value = observation.required("reply")?.required("result")?;
        // A partial page or an ambiguous reply does not prove absence.
        if value
            .get("nextCursor")
            .is_some_and(|cursor| !cursor.is_null())
            || value
                .required("data")?
                .as_array()
                .ok_or_else(unresolved)?
                .iter()
                .any(|id| id.as_js_string() == Some(thread))
        {
            return Err(unresolved());
        }
        same_epoch(owner, &epoch).await?;
        proof.insert("operationId", IvyJson::String(call.operation_id));
    }
    if uncertain {
        contexts.quarantine(context, proof).await?;
    } else {
        contexts.retire(context).await?;
    }
    Ok(true)
}
/// Only this context's original receipts authorize retirement. Unknown outcomes
/// remain in SQLite; neither starts nor uncertain unsubscriptions are replayed.
async fn retire_context(
    contexts: &Contexts,
    context: &mut Context,
    client: Arc<dyn RpcClient>,
    tools: Arc<ToolClients>,
    admission: &mut Option<Dispatch>,
) -> IvyResult<()> {
    let owner = owner(context, tools, &CancellationToken::new());
    if retired_epoch(contexts, context, &owner, client.clone()).await? {
        return Ok(());
    }
    if context.thread.is_none() {
        let operation = owner.operation(&context.start).await?;
        if model::optional_text(&operation, "phase").as_deref() == Some("failed") {
            settled(context, &operation)?;
            // A confirmed start failure has no successful context to unload.
            // Its original failure receipt remains with the native owner.
            return contexts.retire(context).await;
        }
        context.thread = Some(started(result(context, &operation)?)?);
        contexts.save(context).await?;
    }
    let uncertain = uncertain_read(contexts, context, &owner).await?;
    if let Some(call) = &context.unsubscribe {
        let operation = owner.operation(call).await?;
        result(context, &operation)?;
    } else {
        same_epoch(&owner, &context.epoch).await?;
        let call = prepare(
            &owner,
            client,
            "thread/unsubscribe",
            IvyJson::object([("threadId", IvyJson::String(context.thread.clone().unwrap()))]),
        )
        .await?;
        *admission = Some(Dispatch::new(&call));
        context.unsubscribe = Some(call);
        contexts.save(context).await?;
        let call = context.unsubscribe.as_ref().unwrap();
        // Recheck after discovery and immediately before every permitted send.
        let proof = admission.as_ref().unwrap();
        let response = owner
            .dispatch_local(call, || async {
                same_epoch(&owner, &context.epoch).await?;
                proof.attempted.store(true, Ordering::Release);
                Ok(())
            })
            .await;
        if response
            .as_ref()
            .is_err_and(|error| error.outcome == Outcome::NotExecuted)
        {
            proof.attempted.store(false, Ordering::Release);
        }
        let operation = response?;
        let operation = match operation {
            Some(value) => {
                owner.check_operation(&value, call)?;
                value
            }
            None => owner.operation(call).await?,
        };
        result(context, &operation)?;
    }
    if uncertain {
        contexts
            .quarantine(
                context,
                IvyJson::object([
                    ("reason", "read_context_unsubscribed".into()),
                    ("epoch", IvyJson::String(context.epoch.clone())),
                    (
                        "operationId",
                        IvyJson::String(context.unsubscribe.as_ref().unwrap().operation_id.clone()),
                    ),
                ]),
            )
            .await
    } else {
        contexts.retire(context).await
    }
}
impl TaskMcp for NativeMcpSession {
    fn call(&self, tool: JsString, args: IvyJson) -> ServiceFuture<IvyJson> {
        let this = self.clone();
        Box::pin(async move {
            if !this
                .0
                .configuration
                .get("tools")
                .and_then(IvyJson::as_array)
                .is_some_and(|tools| tools.iter().any(|t| t.as_js_string() == Some(&tool)))
            {
                return Err(IvyError::new(
                    "configuration_invalid",
                    "The task has not been granted this MCP tool.",
                ));
            }
            tokio::select! {biased;
                _=this.0.closed.cancelled()=>Err(unresolved()),
                _=this.0.signal.cancelled()=>Err(unresolved()),
                result=async {
                    let mut state=this.0.state.lock().await;
                    if state.closed{return Err(IvyError::internal());}
                    this.initialize(&mut state).await?;
                    let context=state.context.as_mut().unwrap();
                    if context.pending.is_some() { return Err(unresolved()); }
                    let owner=owner(context,this.0.tools.clone(),&this.0.signal);
                    let params=IvyJson::object([("threadId",IvyJson::String(context.thread.clone().unwrap())),("server",this.0.configuration.required("server")?.clone()),("tool",IvyJson::String(tool)),("arguments",args)]);
                    let call=prepare(&owner,this.0.client.clone(),"mcpServer/tool/call",params).await?;
                    state.dispatch=Some(Dispatch::new(&call));
                    let State {context,dispatch:admission,..}=&mut *state;
                    let context=context.as_mut().unwrap();
                    context.pending=Some(call);
                    this.0.contexts.save(context).await?;
                    let call=context.pending.as_ref().unwrap();
                    let operation=dispatch(&owner,call,&this.0.signal,admission.as_ref().unwrap()).await?;
                    let value=tool_result(result(context,&operation)?);
                    // A completed native reply remains at its original operation.
                    context.pending=None;
                    this.0.contexts.save(context).await?;
                    value
                }=>result
            }
        })
    }
    fn close(&self) -> ServiceFuture<()> {
        let this = self.clone();
        Box::pin(async move {
            this.0.closed.cancel();
            let mut state = this.0.state.lock().await;
            state.closed = true;
            let State {
                context, dispatch, ..
            } = &mut *state;
            let Some(context) = context else {
                return Ok(());
            };
            let _cleanup = this.0.contexts.cleanup.lock().await;
            this.0.contexts.mark_closing(context).await?;
            // Reload after acquiring the cleanup owner: a previous close may
            // already have completed, or saved its unsubscribe before timing out.
            let Some(mut saved) = this.0.contexts.read(context.key()?).await? else {
                return Ok(());
            };
            if discard_unsent(&this.0.contexts, &mut saved, dispatch.as_ref()).await? {
                return Ok(());
            }
            let _ = tokio::time::timeout(Duration::from_secs(5), async {
                retire_context(
                    &this.0.contexts,
                    &mut saved,
                    this.0.client.clone(),
                    this.0.tools.clone(),
                    dispatch,
                )
                .await
            })
            .await;
            discard_unsent(&this.0.contexts, &mut saved, dispatch.as_ref()).await?;
            Ok(())
        })
    }
}

pub type CurrentClient = Arc<dyn Fn() -> IvyResult<Arc<dyn RpcClient>> + Send + Sync>;
struct Slot {
    busy: AtomicBool,
    retained: Mutex<Option<Retained>>,
}
struct Retained {
    session: NativeMcpSession,
    selection: IvyJson,
    until: tokio::time::Instant,
}
struct Lease {
    session: NativeMcpSession,
    slot: Arc<Slot>,
    keep: bool,
    retention: Duration,
    finished: AtomicBool,
}
impl Drop for Lease {
    fn drop(&mut self) {
        if !self.finished.swap(true, Ordering::AcqRel) {
            let session = self.session.clone();
            let slot = self.slot.clone();
            session.0.signal.cancel();
            tokio::spawn(async move {
                let _ = session.close().await;
                slot.busy.store(false, Ordering::Release);
            });
        }
    }
}
struct SlotClaim(Option<Arc<Slot>>);
impl Drop for SlotClaim {
    fn drop(&mut self) {
        if let Some(slot) = &self.0 {
            slot.busy.store(false, Ordering::Release);
        }
    }
}
impl TaskMcp for Lease {
    fn call(&self, tool: JsString, args: IvyJson) -> ServiceFuture<IvyJson> {
        self.session.call(tool, args)
    }
    fn close(&self) -> ServiceFuture<()> {
        let session = self.session.clone();
        let slot = self.slot.clone();
        if self.finished.swap(true, Ordering::AcqRel) {
            return Box::pin(async { Ok(()) });
        }
        let keep = self.keep;
        let retention = self.retention;
        Box::pin(async move {
            let _claim = SlotClaim(Some(slot.clone()));
            session.0.signal.cancel();
            let healthy = {
                let state = session.0.state.lock().await;
                !state.closed
                    && state
                        .context
                        .as_ref()
                        .is_none_or(|c| c.pending.is_none() && c.thread.is_some())
            };
            if !keep || !healthy {
                session.close().await?;
                slot.retained.lock().unwrap().take();
            } else if let Some(retained) = slot.retained.lock().unwrap().as_mut() {
                retained.until = tokio::time::Instant::now() + retention;
            }
            Ok(())
        })
    }
}
fn selection(row: &IvyJson) -> IvyJson {
    IvyJson::object([
        ("task", row.get("task").cloned().unwrap_or(IvyJson::Null)),
        (
            "definition",
            row.get("definition").cloned().unwrap_or(IvyJson::Null),
        ),
        (
            "revision",
            row.get("revision").cloned().unwrap_or(IvyJson::Null),
        ),
    ])
}
/// Reuse tool bindings for the same live RPC connection, across task runs.
pub struct NativeMcpFactory {
    client: CurrentClient,
    cached: Mutex<Option<(Arc<dyn RpcClient>, Arc<ToolClients>)>>,
    contexts: Contexts,
    slots: Arc<Mutex<HashMap<String, Arc<Slot>>>>,
}
impl NativeMcpFactory {
    pub fn new(client: CurrentClient, store: StoreOwner) -> Self {
        Self {
            client,
            cached: Mutex::new(None),
            contexts: Contexts::new(store),
            slots: Arc::new(Mutex::new(HashMap::new())),
        }
    }
    fn selected(&self) -> IvyResult<(Arc<dyn RpcClient>, Arc<ToolClients>)> {
        let client = (self.client)()?;
        let mut cached = self.cached.lock().unwrap();
        let tools = match cached.as_ref() {
            Some((previous, tools)) if Arc::ptr_eq(previous, &client) => tools.clone(),
            _ => {
                let tools = Arc::new(ToolClients::new(client.clone()));
                *cached = Some((client.clone(), tools.clone()));
                tools
            }
        };
        Ok((client, tools))
    }
    pub async fn recover(&self) -> IvyResult<()> {
        let (client, tools) = self.selected()?;
        // Reuse follows accepted local ownership, including disable/delete/adoption.
        // No Hive fetch, model turn, or background pool is required.
        let slots: Vec<_> = self
            .slots
            .lock()
            .unwrap()
            .iter()
            .map(|(id, slot)| (id.clone(), slot.clone()))
            .collect();
        for (id, slot) in slots {
            if slot
                .busy
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
            {
                continue;
            }
            let claim = SlotClaim(Some(slot.clone()));
            let lookup = id.clone();
            let row = self
                .contexts
                .store
                .call(move |s| s.get(&lookup, false))
                .await;
            let retired = {
                let mut retained = slot.retained.lock().unwrap();
                if retained.as_ref().is_some_and(|r| {
                    tokio::time::Instant::now() >= r.until
                        || !Arc::ptr_eq(&r.session.0.client, &client)
                        || row
                            .as_ref()
                            .ok()
                            .and_then(Option::as_ref)
                            .is_none_or(|row| {
                                !model::truth(row.get("task").unwrap_or(&IvyJson::Null), "enabled")
                                    || selection(row) != r.selection
                            })
                }) {
                    retained.take()
                } else {
                    None
                }
            };
            if let Some(retired) = retired {
                let _ = retired.session.close().await;
            }
            drop(claim);
            if slot.retained.lock().unwrap().is_none()
                && !self.contexts.task_pending(id.clone()).await?
            {
                let mut current = self.slots.lock().unwrap();
                if !slot.busy.load(Ordering::Acquire) && Arc::strong_count(&slot) == 2 {
                    current.remove(&id);
                }
            }
        }
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        let mut cursor = tokio::time::timeout_at(deadline, self.contexts.cleanup.lock())
            .await
            .map_err(|_| unresolved())?;
        let mut saved = None;
        let mut admission = None;
        let outcome = tokio::time::timeout_at(deadline, async {
            if let Some(key) = self.contexts.next(cursor.clone()).await? {
                *cursor = key.clone();
                saved = self.contexts.read(key).await?;
                if let Some(context) = &mut saved {
                    retire_context(&self.contexts, context, client, tools, &mut admission).await?;
                }
            }
            Ok(())
        })
        .await
        .map_err(|_| unresolved());
        if let Some(context) = &mut saved {
            discard_unsent(&self.contexts, context, admission.as_ref()).await?;
        }
        outcome?
    }
    pub async fn close(&self) {
        let slots: Vec<_> = self.slots.lock().unwrap().values().cloned().collect();
        for slot in slots {
            let retained = slot.retained.lock().unwrap().take();
            if let Some(retained) = retained {
                let _ = retained.session.close().await;
            }
        }
    }
}
impl McpBridgeFactory for NativeMcpFactory {
    fn open(&self, task: IvyJson, signal: CancellationToken) -> ServiceFuture<Arc<dyn McpBridge>> {
        let selected = self.selected();
        let contexts = self.contexts.clone();
        let slots = self.slots.clone();
        Box::pin(async move {
            let (client, tools) = selected?;
            let configuration = task.required("mcp")?.clone();
            let session: Arc<dyn TaskMcp> = if configuration.get("transport") == Some(&"ivy".into())
            {
                Arc::new(crate::ivy_mcp::IvyMcp::new(
                    client,
                    tools,
                    configuration,
                    signal.clone(),
                ))
            } else {
                let id = model::text(&task, "id")?;
                let lookup = id.clone();
                let row = contexts
                    .store
                    .call(move |s| s.get(&lookup, false))
                    .await?
                    .ok_or_else(unresolved)?;
                if row.get("task") != Some(&task) {
                    return Err(unresolved());
                }
                let selected = selection(&row);
                let slot = {
                    let mut slots = slots.lock().unwrap();
                    if slots.len() >= 64 && !slots.contains_key(&id) {
                        return Err(unresolved());
                    }
                    slots
                        .entry(id.clone())
                        .or_insert_with(|| {
                            Arc::new(Slot {
                                busy: AtomicBool::new(false),
                                retained: Mutex::new(None),
                            })
                        })
                        .clone()
                };
                if slot
                    .busy
                    .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                    .is_err()
                {
                    return Err(unresolved());
                }
                let mut claim = SlotClaim(Some(slot.clone()));
                let opened = async {
                    let previous = slot.retained.lock().unwrap().take();
                    let reusable = if let Some(previous) = previous {
                        if previous.selection == selected
                            && Arc::ptr_eq(&previous.session.0.client, &client)
                            && !previous.session.0.closed.is_cancelled()
                            && tokio::time::Instant::now() < previous.until
                        {
                            Some(previous.session)
                        } else {
                            previous.session.close().await?;
                            None
                        }
                    } else {
                        None
                    };
                    let session = if let Some(session) = reusable {
                        session
                    } else {
                        if contexts.task_pending(id.clone()).await? {
                            return Err(unresolved());
                        }
                        NativeMcpSession::new(
                            client,
                            tools,
                            configuration,
                            CancellationToken::new(),
                            contexts,
                        )?
                    };
                    let interval = model::number(&task, "intervalSeconds")? as u64;
                    let keep = model::truth(&task, "enabled") && interval > 0 && interval < 1800;
                    let retention = Duration::from_secs((interval + 60).min(1800));
                    *slot.retained.lock().unwrap() = Some(Retained {
                        session: session.clone(),
                        selection: selected,
                        until: tokio::time::Instant::now() + retention,
                    });
                    Ok::<_, IvyError>(Arc::new(Lease {
                        session: session.for_run(signal.clone(), id),
                        slot: slot.clone(),
                        keep,
                        retention,
                        finished: AtomicBool::new(false),
                    }) as Arc<dyn TaskMcp>)
                }
                .await;
                let opened = opened?;
                claim.0 = None;
                opened
            };
            Ok(Arc::new(LocalBridge::open(session, signal).await?) as Arc<dyn McpBridge>)
        })
    }
}
