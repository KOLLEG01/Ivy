//! Transport for one selected native owner; domain dispatch authority stays with callers.
use crate::{
    IvyError, IvyJson, IvyResult, JsString, Outcome,
    native::read_native_contract,
    native_evidence::{
        NativeOperationIdentity, NativeOwnerIdentity, NativeReadIdentity,
        validate_native_operation, validate_native_read, validate_native_status,
    },
    native_observations::{NativeObservations, NativeThreadState},
    native_operation::{NativeOperationCall, NativeOperationOwner},
    native_project::native_thread_project,
    native_turn_contents::{ContractFuture, NativeTurnOwner},
    rpc::{RequestFuture, RequestOptions},
    tools::{BoundTool, InterfaceRequirement, ToolCallOptions, ToolClients, call_bound},
};
use futures_util::{
    FutureExt,
    future::{BoxFuture, Shared},
};
use ivy_contracts::{
    agent_schema::validate_agent, hash::hash_json, native_contract::NativeContract,
};
use std::{
    collections::VecDeque,
    future::Future,
    sync::{Arc, Mutex},
};

pub type GuardFuture<'a> = BoxFuture<'a, IvyResult<()>>;
/// Rechecked immediately before sending, including after a rejected stale envelope.
pub trait NativeDispatchGuard: Send + Sync {
    fn check(&self) -> GuardFuture<'_>;
}
pub(crate) struct ContractEntry {
    lookup: Shared<BoxFuture<'static, IvyResult<Arc<NativeContract>>>>,
}
pub(crate) type ContractCache = Mutex<VecDeque<(String, Arc<ContractEntry>)>>;

/// Share the same ToolClients per RPC connection to share the bounded catalog cache.
#[derive(Clone)]
pub struct NativeOwner {
    tools: Arc<ToolClients>,
    caller: JsString,
    target: NativeOwnerIdentity,
    options: RequestOptions,
}
fn require(ok: bool, code: &str, message: &str) -> IvyResult<()> {
    if ok {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn text(value: &IvyJson, key: &str) -> IvyResult<JsString> {
    value
        .required(key)?
        .as_js_string()
        .cloned()
        .ok_or_else(IvyError::internal)
}
fn empty() -> IvyJson {
    IvyJson::object(Vec::<(String, IvyJson)>::new())
}
impl NativeOwner {
    pub fn new(
        tools: Arc<ToolClients>,
        caller: JsString,
        target: NativeOwnerIdentity,
        options: RequestOptions,
    ) -> Self {
        Self {
            tools,
            caller,
            target,
            options,
        }
    }
    pub fn target(&self) -> &NativeOwnerIdentity {
        &self.target
    }
    pub fn caller(&self) -> &JsString {
        &self.caller
    }
    fn check_signal(&self) -> IvyResult<()> {
        if self.options.cancellation.is_cancelled() {
            Err(self.options.cancel_error.clone())
        } else {
            Ok(())
        }
    }
    pub async fn selected_contract(&self) -> IvyResult<Arc<NativeContract>> {
        self.check_signal()?;
        let mut identity = IvyJson::object([
            (
                "serviceNodeId",
                IvyJson::String(self.target.service_node_id.clone()),
            ),
            (
                "nativeVersion",
                IvyJson::String(self.target.native_version.clone()),
            ),
            (
                "nativeExecutableHash",
                IvyJson::String(self.target.native_executable_hash.clone()),
            ),
            (
                "catalogHash",
                IvyJson::String(self.target.catalog_hash.clone()),
            ),
        ]);
        if let Some(host) = &self.target.host_id {
            identity.insert("hostId", IvyJson::String(host.clone()));
        }
        let key = hash_json(&identity)?;
        let entry =
            {
                let mut cache = self.tools.native_contracts.lock().unwrap();
                if let Some((_, entry)) = cache.iter().find(|(stored, _)| stored == &key) {
                    entry.clone()
                } else {
                    let owner = self.clone();
                    let lookup = async move {
                        let mut target = owner.target.clone();
                        if target.host_id.is_none() {
                            target.host_id = Some(text(&owner.status().await?, "hostId")?);
                        }
                        // Discovery is shared independently of the observing caller's options.
                        Ok(Arc::new(
                            read_native_contract(&owner.tools, &target, RequestOptions::default())
                                .await?
                                .contract,
                        ))
                    }
                    .boxed()
                    .shared();
                    let entry = Arc::new(ContractEntry { lookup });
                    if cache.len() >= 16 {
                        cache.pop_front();
                    }
                    cache.push_back((key.clone(), entry.clone()));
                    let weak = Arc::downgrade(&self.tools);
                    let pending = entry.clone();
                    let cleanup_key = key.clone();
                    tokio::spawn(async move {
                        if pending.lookup.clone().await.is_err() {
                            if let Some(tools) = weak.upgrade() {
                                tools.native_contracts.lock().unwrap().retain(
                                    |(stored, current)| {
                                        stored != &cleanup_key || !Arc::ptr_eq(current, &pending)
                                    },
                                );
                            }
                        }
                    });
                    entry
                }
            };
        let contract = match entry.lookup.clone().await {
            Ok(contract) => contract,
            Err(error) => {
                self.tools
                    .native_contracts
                    .lock()
                    .unwrap()
                    .retain(|(stored, current)| stored != &key || !Arc::ptr_eq(current, &entry));
                return Err(error);
            }
        };
        self.check_signal()?;
        Ok(contract)
    }
    pub async fn binding(
        &self,
        method: &JsString,
        expected_definition_hash: Option<&JsString>,
    ) -> IvyResult<Arc<BoundTool>> {
        self.check_signal()?;
        let mut name = JsString::from("codex.");
        name.0.extend(&method.0);
        let expected = expected_definition_hash.cloned().map(IvyJson::String);
        let binding = self
            .tools
            .native_service_tools(self.target.service_node_id.clone())?
            .binding(&name, expected.as_ref())
            .await?;
        self.check_signal()?;
        Ok(binding)
    }
    pub async fn management(
        &self,
        name: &str,
        args: IvyJson,
        operation_id: Option<JsString>,
    ) -> IvyResult<IvyJson> {
        self.call_management(name, args, operation_id, None, false, None)
            .await
    }
    // The explicit Send future also breaks status/recovery's recursive future
    // type. Local guards use the same implementation without this coercion.
    fn call_management<'a>(
        &'a self,
        name: &'a str,
        args: IvyJson,
        operation_id: Option<JsString>,
        guard: Option<&'a dyn NativeDispatchGuard>,
        reconcile_unknown: bool,
        reassess: Option<&'a NativeOperationCall>,
    ) -> RequestFuture<'a> {
        Box::pin(self.call_management_with_guard(
            name,
            args,
            operation_id,
            move || async move {
                if let Some(guard) = guard {
                    guard.check().await?;
                }
                Ok(())
            },
            reconcile_unknown,
            reassess,
        ))
    }
    async fn call_management_with_guard<G, F>(
        &self,
        name: &str,
        args: IvyJson,
        operation_id: Option<JsString>,
        guard: G,
        reconcile_unknown: bool,
        reassess: Option<&NativeOperationCall>,
    ) -> IvyResult<IvyJson>
    where
        G: Fn() -> F,
        F: Future<Output = IvyResult<()>>,
    {
        self.call_management_with_error(
            name,
            args,
            operation_id,
            guard,
            reconcile_unknown,
            reassess,
            |error: IvyError| error.with_outcome(Outcome::NotExecuted),
        )
        .await
    }
    async fn call_management_with_error<X, G, F, N>(
        &self,
        name: &str,
        args: IvyJson,
        operation_id: Option<JsString>,
        guard: G,
        reconcile_unknown: bool,
        reassess: Option<&NativeOperationCall>,
        retry_guard_failure: N,
    ) -> Result<IvyJson, X>
    where
        X: From<IvyError>,
        G: Fn() -> F,
        F: Future<Output = Result<(), X>>,
        N: Fn(X) -> X,
    {
        self.check_signal()?;
        let tools = self.tools.service_tools(
            self.target.service_node_id.clone(),
            &[InterfaceRequirement {
                namespace: "agent".into(),
                interface_version: "1.0.0".into(),
            }],
        )?;
        let mut binding = tools.binding(&format!("agent.{name}").into(), None).await?;
        for attempt in 0..=1 {
            let guarded = async {
                guard().await?;
                self.check_signal().map_err(X::from)
            }
            .await;
            if let Err(error) = guarded {
                return Err(if attempt == 0 {
                    error
                } else {
                    retry_guard_failure(error)
                });
            }
            match call_bound(
                self.tools.client(),
                &binding,
                args.clone(),
                operation_id.clone(),
                ToolCallOptions {
                    request: self.options.clone(),
                    expected_caller_principal_id: Some(self.caller.clone()),
                },
            )
            .await
            {
                Ok(value) => return Ok(value),
                Err(error) => {
                    if reconcile_unknown && error.outcome != Outcome::NotExecuted {
                        return Ok(IvyJson::Null);
                    }
                    if attempt > 0
                        || error.code != "tool_definition_changed"
                        || error.outcome != Outcome::NotExecuted
                    {
                        return Err(error.into());
                    }
                    let refreshed = async {
                        let fresh = tools.refresh(&binding).await?;
                        if fresh.value().get("definitionHash")
                            == binding.value().get("definitionHash")
                        {
                            return Err(error);
                        }
                        if let Some(call) = reassess {
                            self.reassess_native(call).await?;
                        }
                        Ok(fresh)
                    }
                    .await;
                    binding =
                        refreshed.map_err(|error| error.with_outcome(Outcome::NotExecuted))?;
                }
            }
        }
        unreachable!()
    }
    pub async fn status(&self) -> IvyResult<IvyJson> {
        let value = self.management("status", empty(), None).await?;
        validate_native_status(&value, &self.target, None)?;
        Ok(value)
    }
    pub async fn frame_limits(&self) -> IvyResult<IvyJson> {
        let value = self.management("frameLimits", empty(), None).await?;
        validate_agent("FrameLimits", &value)?;
        require(
            text(&value, "serviceNodeId")? == self.target.service_node_id
                && text(&value, "nativeVersion")? == self.target.native_version
                && text(&value, "nativeExecutableHash")? == self.target.native_executable_hash
                && text(&value, "catalogHash")? == self.target.catalog_hash
                && value
                    .get("epoch")
                    .and_then(IvyJson::as_js_string)
                    .is_some_and(|s| !s.0.is_empty()),
            "native_owner_mismatch",
            "Frame limits must come from the exact selected native owner.",
        )?;
        Ok(value)
    }
    pub async fn thread_start_params(&self, params: &IvyJson) -> IvyResult<IvyJson> {
        self.check_signal()?;
        native_thread_project(
            &self.tools,
            &self.target.service_node_id,
            params,
            self.options.clone(),
        )
        .await
    }
    pub fn check_operation(&self, value: &IvyJson, call: &NativeOperationCall) -> IvyResult<()> {
        validate_native_operation(
            value,
            &NativeOperationIdentity {
                owner: self.target.clone(),
                caller_principal_id: self.caller.clone(),
                operation_id: call.operation_id.clone(),
                method: call.method.clone(),
                params: call.params.clone(),
            },
            None,
        )
    }
    pub async fn operation(&self, call: &NativeOperationCall) -> IvyResult<IvyJson> {
        match self
            .management(
                "operation",
                IvyJson::object([("operationId", IvyJson::String(call.operation_id.clone()))]),
                None,
            )
            .await
        {
            Ok(value) => {
                self.check_operation(&value, call)?;
                Ok(value)
            }
            Err(error) if error.code == "not_found" => {
                let absence = error.details.unwrap_or(IvyJson::Null);
                validate_agent("OperationAbsence", &absence)?;
                require(
                    text(&absence, "serviceNodeId")? == self.target.service_node_id
                        && text(&absence, "operationId")? == call.operation_id,
                    "native_absence_mismatch",
                    "Only exact owner-produced absence can identify an unobserved original operation.",
                )?;
                Ok(absence)
            }
            Err(error) => Err(error),
        }
    }
    fn arguments(&self, call: &NativeOperationCall, binding: &BoundTool) -> IvyResult<IvyJson> {
        Ok(IvyJson::object([
            ("operationId", IvyJson::String(call.operation_id.clone())),
            (
                "nativeVersion",
                IvyJson::String(self.target.native_version.clone()),
            ),
            ("method", IvyJson::String(call.method.clone())),
            ("params", call.params.clone()),
            (
                "expectedDefinitionHash",
                binding.value().required("definitionHash")?.clone(),
            ),
        ]))
    }
    pub async fn dispatch(
        &self,
        call: &NativeOperationCall,
        guard: &dyn NativeDispatchGuard,
    ) -> IvyResult<Option<IvyJson>> {
        self.dispatch_local(call, || guard.check()).await
    }
    /// Guarded dispatch with a caller-local callback/future. The same guard is
    /// rechecked before every send, including after definition recovery; local
    /// state is never moved to a worker thread or granted dispatch authority.
    pub async fn dispatch_local<G, F>(
        &self,
        call: &NativeOperationCall,
        guard: G,
    ) -> IvyResult<Option<IvyJson>>
    where
        G: Fn() -> F,
        F: Future<Output = IvyResult<()>>,
    {
        self.dispatch_local_with_error(call, guard, |error: IvyError| {
            error.with_outcome(Outcome::NotExecuted)
        })
        .await
    }
    /// Keep initial guard failures unchanged. Only a guard or immediately
    /// following signal failure on the recovered retry passes through the mapper.
    /// Callback errors and futures may borrow caller-local, non-Send state.
    pub async fn dispatch_local_with_error<X, G, F, N>(
        &self,
        call: &NativeOperationCall,
        guard: G,
        retry_guard_failure: N,
    ) -> Result<Option<IvyJson>, X>
    where
        X: From<IvyError>,
        G: Fn() -> F,
        F: Future<Output = Result<(), X>>,
        N: Fn(X) -> X,
    {
        let binding = self
            .binding(&call.method, Some(&call.definition_hash))
            .await?;
        let value = self
            .call_management_with_error(
                "invoke",
                self.arguments(call, &binding)?,
                Some(call.operation_id.clone()),
                guard,
                true,
                Some(call),
                retry_guard_failure,
            )
            .await?;
        Ok((!value.is_null()).then_some(value))
    }
    async fn reassess_native(&self, call: &NativeOperationCall) -> IvyResult<()> {
        self.status().await?;
        let tools = self
            .tools
            .native_service_tools(self.target.service_node_id.clone())?;
        let mut name = JsString::from("codex.");
        name.0.extend(&call.method.0);
        let fresh = tools.refresh(&tools.binding(&name, None).await?).await?;
        require(
            fresh
                .value()
                .get("definitionHash")
                .and_then(IvyJson::as_js_string)
                == Some(&call.definition_hash),
            "tool_definition_changed",
            "Envelope recovery cannot change the original native method definition.",
        )
    }
    pub async fn interact(
        &self,
        call: &NativeOperationCall,
        guard: Option<&dyn NativeDispatchGuard>,
    ) -> IvyResult<IvyJson> {
        let binding = self
            .binding(&call.method, Some(&call.definition_hash))
            .await?;
        let value = self
            .call_management(
                "interact",
                self.arguments(call, &binding)?,
                Some(call.operation_id.clone()),
                guard,
                false,
                Some(call),
            )
            .await?;
        validate_agent("Interaction", &value)?;
        require(
            text(&value, "operationId")? == call.operation_id,
            "native_observation_invalid",
            "Interaction reply identity differs.",
        )?;
        Ok(value)
    }
    pub async fn interaction(&self, operation_id: &JsString) -> IvyResult<IvyJson> {
        let value = self
            .management(
                "interaction",
                IvyJson::object([("operationId", IvyJson::String(operation_id.clone()))]),
                None,
            )
            .await?;
        validate_agent("Interaction", &value)?;
        require(
            text(&value, "operationId")? == *operation_id,
            "native_observation_invalid",
            "Interaction lookup identity differs.",
        )?;
        Ok(value)
    }
    pub fn check_read(
        &self,
        value: &IvyJson,
        method: &JsString,
        params: &IvyJson,
        epoch: &JsString,
        allow_error: bool,
    ) -> IvyResult<()> {
        validate_native_read(
            value,
            &NativeReadIdentity {
                owner: self.target.clone(),
                caller_principal_id: self.caller.clone(),
                epoch: epoch.clone(),
                method: method.clone(),
                params: params.clone(),
            },
            None,
        )?;
        require(
            allow_error || value.required("reply")?.get("result").is_some(),
            "native_read_failed",
            "The native read returned its original error.",
        )
    }
    pub async fn read(
        &self,
        method: &JsString,
        params: IvyJson,
        epoch: Option<&JsString>,
        allow_error: bool,
    ) -> IvyResult<IvyJson> {
        let value = self
            .management(
                "read",
                IvyJson::object([
                    (
                        "nativeVersion",
                        IvyJson::String(self.target.native_version.clone()),
                    ),
                    ("method", IvyJson::String(method.clone())),
                    ("params", params.clone()),
                ]),
                None,
            )
            .await?;
        let observed = if let Some(epoch) = epoch {
            epoch.clone()
        } else {
            if value.is_null() {
                return Err(IvyError::internal());
            }
            value
                .get("epoch")
                .and_then(IvyJson::as_js_string)
                .cloned()
                .unwrap_or_default()
        };
        self.check_read(&value, method, &params, &observed, allow_error)?;
        Ok(value)
    }
    pub async fn thread_state(
        &self,
        observation: &IvyJson,
        epoch: &JsString,
    ) -> IvyResult<NativeThreadState> {
        self.check_read(
            observation,
            &"thread/read".into(),
            observation.required("params")?,
            epoch,
            false,
        )?;
        let contract = self.selected_contract().await?;
        let result = observation
            .required("reply")?
            .get("result")
            .ok_or_else(|| {
                IvyError::new(
                    "native_read_failed",
                    "Thread state needs a successful original read.",
                )
            })?;
        NativeObservations::new(&contract).native_thread_state(result)
    }
}
impl NativeOperationOwner for NativeOwner {
    fn operation<'a>(&'a self, call: &'a NativeOperationCall) -> RequestFuture<'a> {
        Box::pin(NativeOwner::operation(self, call))
    }
    fn check_operation(&self, value: &IvyJson, call: &NativeOperationCall) -> IvyResult<()> {
        NativeOwner::check_operation(self, value, call)
    }
}
impl NativeTurnOwner for NativeOwner {
    fn check_read(
        &self,
        value: &IvyJson,
        method: &str,
        params: &IvyJson,
        epoch: &JsString,
    ) -> IvyResult<()> {
        NativeOwner::check_read(self, value, &method.into(), params, epoch, false)
    }
    fn read<'a>(
        &'a self,
        method: &'a str,
        params: IvyJson,
        epoch: &'a JsString,
        allow_error: bool,
    ) -> RequestFuture<'a> {
        Box::pin(async move {
            NativeOwner::read(self, &method.into(), params, Some(epoch), allow_error).await
        })
    }
    fn selected_contract(&self) -> ContractFuture<'_> {
        Box::pin(NativeOwner::selected_contract(self))
    }
}
