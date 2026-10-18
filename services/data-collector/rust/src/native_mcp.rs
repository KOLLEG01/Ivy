//! Ephemeral native MCP contexts. No model turn is started by this transport.
use crate::{
    mcp_bridge::{LocalBridge, TaskMcp},
    mcp_result::tool_result,
    model,
    runner::{McpBridge, McpBridgeFactory},
};
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString, agent_schema::validate_agent};
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
fn text(value: &IvyJson, key: &str) -> IvyResult<JsString> {
    value
        .required(key)?
        .as_js_string()
        .cloned()
        .ok_or_else(IvyError::internal)
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
    owner: Option<NativeOwner>,
    epoch: Option<JsString>,
    thread: Option<JsString>,
    closed: bool,
}
struct Inner {
    client: Arc<dyn RpcClient>,
    tools: Arc<ToolClients>,
    configuration: IvyJson,
    signal: CancellationToken,
    closed: CancellationToken,
    state: AsyncMutex<State>,
}
#[derive(Clone)]
pub struct NativeMcpSession(Arc<Inner>);
impl NativeMcpSession {
    pub fn new(
        client: Arc<dyn RpcClient>,
        tools: Arc<ToolClients>,
        configuration: IvyJson,
        signal: CancellationToken,
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
        })))
    }
    fn options(signal: &CancellationToken) -> RequestOptions {
        RequestOptions {
            cancellation: signal.clone(),
            cancel_error: IvyError::internal(),
            ..Default::default()
        }
    }
    async fn invoke(
        &self,
        state: &State,
        method: &str,
        params: IvyJson,
        signal: &CancellationToken,
    ) -> IvyResult<IvyJson> {
        check(signal)?;
        let selected = state.owner.as_ref().ok_or_else(IvyError::internal)?;
        let owner = NativeOwner::new(
            self.0.tools.clone(),
            selected.caller().clone(),
            selected.target().clone(),
            Self::options(signal),
        );
        let method = JsString::from(method);
        let binding = owner.binding(&method, None).await?;
        let call = NativeOperationCall {
            operation_id: new_operation_id(self.0.client.clone(), None).await?,
            method,
            params,
            definition_hash: text(binding.value(), "definitionHash")?,
        };
        let mut operation = owner.dispatch(&call, &Guard(signal.clone())).await?;
        while !operation.as_ref().is_some_and(|op| {
            matches!(
                model::optional_text(op, "phase").as_deref(),
                Some("succeeded" | "failed")
            )
        }) {
            tokio::select! {biased;_=signal.cancelled()=>return Err(IvyError::internal()),_=tokio::time::sleep(Duration::from_millis(100))=>{}}
            let observed = owner.operation(&call).await?;
            if observed.get("phase").is_some() {
                operation = Some(observed);
            }
        }
        let operation = operation.unwrap();
        owner.check_operation(&operation, &call)?;
        if operation.get("epoch").and_then(IvyJson::as_js_string) != state.epoch.as_ref()
            || model::optional_text(&operation, "phase").as_deref() != Some("succeeded")
            || operation
                .get("reply")
                .and_then(|r| r.get("result"))
                .is_none()
        {
            return Err(IvyError::new(
                "provider_unavailable",
                "The original MCP operation did not complete on the selected owner.",
            ));
        }
        Ok(operation.required("reply")?.required("result")?.clone())
    }
    async fn initialize(&self, state: &mut State) -> IvyResult<()> {
        if state.thread.is_some() {
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
        state.owner = Some(NativeOwner::new(
            self.0.tools.clone(),
            text(&identity, "callerPrincipalId")?,
            target,
            Self::options(&self.0.signal),
        ));
        state.epoch = Some(text(&status, "epoch")?);
        let params = state
            .owner
            .as_ref()
            .unwrap()
            .thread_start_params(&IvyJson::object([
                ("cwd", self.0.configuration.required("cwd")?.clone()),
                ("ephemeral", true.into()),
                ("permissions", ":read-only".into()),
                ("approvalPolicy", "never".into()),
            ]))
            .await?;
        let value = self
            .invoke(state, "thread/start", params, &self.0.signal)
            .await?;
        let thread = value.get("thread");
        if !thread.is_some_and(|t| {
            t.get("id").and_then(IvyJson::as_js_string).is_some() && model::truth(t, "ephemeral")
        }) {
            return Err(IvyError::new(
                "provider_unavailable",
                "The provider did not create the requested ephemeral MCP context.",
            ));
        }
        state.thread = Some(text(thread.unwrap(), "id")?);
        Ok(())
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
                _=this.0.closed.cancelled()=>Err(IvyError::internal()),
                result=async {
                    let mut state=this.0.state.lock().await;
                    if state.closed{return Err(IvyError::internal());}
                    this.initialize(&mut state).await?;
                    let params=IvyJson::object([("threadId",IvyJson::String(state.thread.clone().unwrap())),("server",this.0.configuration.required("server")?.clone()),("tool",IvyJson::String(tool)),("arguments",args)]);
                    let value=this.invoke(&state,"mcpServer/tool/call",params,&this.0.signal).await?;
                    tool_result(&value)
                }=>result
            }
        })
    }
    fn close(&self) -> ServiceFuture<()> {
        let this = self.clone();
        Box::pin(async move {
            this.0.closed.cancel();
            let _ = tokio::time::timeout(Duration::from_secs(5), async {
                let mut state = this.0.state.lock().await;
                state.closed = true;
                let Some(thread) = state.thread.take() else {
                    return;
                };
                let _ = this
                    .invoke(
                        &state,
                        "thread/unsubscribe",
                        IvyJson::object([("threadId", IvyJson::String(thread))]),
                        &CancellationToken::new(),
                    )
                    .await;
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
}
impl NativeMcpFactory {
    pub fn new(client: CurrentClient) -> Self {
        Self {
            client,
            cached: Mutex::new(None),
        }
    }
}
impl McpBridgeFactory for NativeMcpFactory {
    fn open(
        &self,
        configuration: IvyJson,
        signal: CancellationToken,
    ) -> ServiceFuture<Arc<dyn McpBridge>> {
        let selected = (self.client)().map(|client| {
            let mut cached = self.cached.lock().unwrap();
            let tools = match cached.as_ref() {
                Some((previous, tools)) if Arc::ptr_eq(previous, &client) => tools.clone(),
                _ => {
                    let tools = Arc::new(ToolClients::new(client.clone()));
                    *cached = Some((client.clone(), tools.clone()));
                    tools
                }
            };
            (client, tools)
        });
        Box::pin(async move {
            let (client, tools) = selected?;
            let session = NativeMcpSession::new(client, tools, configuration, signal.clone())?;
            Ok(Arc::new(LocalBridge::open(Arc::new(session), signal).await?) as Arc<dyn McpBridge>)
        })
    }
}
