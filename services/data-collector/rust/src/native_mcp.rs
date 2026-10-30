//! Ephemeral native MCP contexts. No model turn is started by this transport.
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
    sync::{Arc, Mutex},
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
struct Guard(CancellationToken);
impl NativeDispatchGuard for Guard {
    fn check(&self) -> GuardFuture<'_> {
        Box::pin(async { check(&self.0) })
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
}
struct Inner {
    client: Arc<dyn RpcClient>,
    tools: Arc<ToolClients>,
    configuration: IvyJson,
    signal: CancellationToken,
    closed: CancellationToken,
    state: AsyncMutex<State>,
    contexts: Contexts,
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
            signal,
            closed: CancellationToken::new(),
            state: AsyncMutex::new(State::default()),
            contexts,
        })))
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
            if context.thread.is_none() {
                let owner = owner(context, self.0.tools.clone(), &self.0.signal);
                let operation = owner.operation(&context.start).await?;
                context.thread = Some(started(result(context, &operation)?)?);
                self.0.contexts.save(context).await?;
            }
            return Ok(());
        }
        let service = self.0.tools.service_tools(
            text(&self.0.configuration, "serviceNodeId")?,
            &[InterfaceRequirement {
                namespace: "agent".into(),
                interface_version: "1.0.0".into(),
            }],
        )?;
        let status = service
            .call(
                &"agent.status".into(),
                object(),
                None,
                ToolCallOptions::default(),
            )
            .await?;
        validate_agent("Status", &status)?;
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
        let params = owner
            .thread_start_params(&IvyJson::object([
                ("cwd", self.0.configuration.required("cwd")?.clone()),
                ("ephemeral", true.into()),
                ("permissions", ":read-only".into()),
                ("approvalPolicy", "never".into()),
            ]))
            .await?;
        let start = prepare(&owner, self.0.client.clone(), "thread/start", params).await?;
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
        });
        let context = state.context.as_mut().unwrap();
        self.0.contexts.insert(context, &self.0.closed).await?;
        let operation = dispatch(&owner, &context.start, &self.0.signal).await?;
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
    let binding = owner.binding(&method, None).await?;
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
) -> IvyResult<IvyJson> {
    check(signal)?;
    let mut operation = owner.dispatch(call, &Guard(signal.clone())).await?;
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
/// Only this context's original receipts authorize retirement. Unknown outcomes
/// remain in SQLite; neither starts nor uncertain unsubscriptions are replayed.
async fn retire_context(
    contexts: &Contexts,
    context: &mut Context,
    client: Arc<dyn RpcClient>,
    tools: Arc<ToolClients>,
) -> IvyResult<()> {
    let owner = owner(context, tools, &CancellationToken::new());
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
    if let Some(call) = &context.pending {
        let operation = owner.operation(call).await?;
        settled(context, &operation)?;
    }
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
        context.unsubscribe = Some(call);
        contexts.save(context).await?;
        let call = context.unsubscribe.as_ref().unwrap();
        // Recheck after discovery and immediately before every permitted send.
        let operation = owner
            .dispatch_local(call, || same_epoch(&owner, &context.epoch))
            .await?;
        let operation = match operation {
            Some(value) => {
                owner.check_operation(&value, call)?;
                value
            }
            None => owner.operation(call).await?,
        };
        result(context, &operation)?;
    }
    contexts.retire(context).await
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
                    context.pending=Some(prepare(&owner,this.0.client.clone(),"mcpServer/tool/call",params).await?);
                    this.0.contexts.save(context).await?;
                    let call=context.pending.as_ref().unwrap();
                    let operation=dispatch(&owner,call,&this.0.signal).await?;
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
            let Some(context) = &mut state.context else {
                return Ok(());
            };
            let _cleanup = this.0.contexts.cleanup.lock().await;
            this.0.contexts.mark_closing(context).await?;
            // Reload after acquiring the cleanup owner: a previous close may
            // already have completed, or saved its unsubscribe before timing out.
            let Some(mut saved) = this.0.contexts.read(context.key()?).await? else {
                return Ok(());
            };
            let _ = tokio::time::timeout(Duration::from_secs(5), async {
                retire_context(
                    &this.0.contexts,
                    &mut saved,
                    this.0.client.clone(),
                    this.0.tools.clone(),
                )
                .await
            })
            .await;
            Ok(())
        })
    }
}

pub type CurrentClient = Arc<dyn Fn() -> IvyResult<Arc<dyn RpcClient>> + Send + Sync>;
/// Reuse tool bindings for the same live RPC connection, across task runs.
pub struct NativeMcpFactory {
    client: CurrentClient,
    cached: Mutex<Option<(Arc<dyn RpcClient>, Arc<ToolClients>)>>,
    contexts: Contexts,
}
impl NativeMcpFactory {
    pub fn new(client: CurrentClient, store: StoreOwner) -> Self {
        Self {
            client,
            cached: Mutex::new(None),
            contexts: Contexts::new(store),
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
        tokio::time::timeout(Duration::from_secs(5), async {
            let mut cursor = self.contexts.cleanup.lock().await;
            if let Some(key) = self.contexts.next(cursor.clone()).await? {
                *cursor = key.clone();
                if let Some(mut context) = self.contexts.read(key).await? {
                    retire_context(&self.contexts, &mut context, client, tools).await?;
                }
            }
            Ok(())
        })
        .await
        .map_err(|_| unresolved())?
    }
}
impl McpBridgeFactory for NativeMcpFactory {
    fn open(
        &self,
        configuration: IvyJson,
        signal: CancellationToken,
    ) -> ServiceFuture<Arc<dyn McpBridge>> {
        let selected = self.selected();
        let contexts = self.contexts.clone();
        Box::pin(async move {
            let (client, tools) = selected?;
            let session =
                NativeMcpSession::new(client, tools, configuration, signal.clone(), contexts)?;
            Ok(Arc::new(LocalBridge::open(Arc::new(session), signal).await?) as Arc<dyn McpBridge>)
        })
    }
}
