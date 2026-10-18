use crate::{
    IvyError, IvyJson, IvyResult, JsString, Outcome,
    rpc::{BoundRead, RequestOptions, RpcClient},
};
use futures_util::{
    FutureExt,
    future::{BoxFuture, Shared},
};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

pub struct BoundTool {
    value: IvyJson,
}
impl BoundTool {
    pub fn value(&self) -> &IvyJson {
        &self.value
    }
    pub fn qualified_name(&self) -> IvyResult<JsString> {
        self.value
            .required("qualifiedName")?
            .as_js_string()
            .cloned()
            .ok_or_else(IvyError::internal)
    }
    pub fn service_node_id(&self) -> IvyResult<JsString> {
        self.value
            .required("serviceNodeId")?
            .as_js_string()
            .cloned()
            .ok_or_else(IvyError::internal)
    }
    fn read_only(&self) -> bool {
        self.value
            .get("definition")
            .and_then(|d| d.get("annotations"))
            .and_then(|a| a.get("readOnlyHint"))
            .and_then(IvyJson::as_bool)
            == Some(true)
    }
    fn definition_hash(&self) -> IvyResult<&IvyJson> {
        self.value.required("definitionHash")
    }
    fn interface_version(&self) -> IvyResult<&IvyJson> {
        self.value
            .required("definition")?
            .required("interfaceVersion")
    }
}
fn name_parts(name: &JsString) -> IvyResult<(JsString, JsString)> {
    let separator = name
        .0
        .iter()
        .position(|unit| *unit == 46)
        .filter(|i| *i > 0)
        .ok_or_else(|| {
            IvyError::new(
                "invalid_arguments",
                "Expected namespace and exact tool name.",
            )
        })?;
    Ok((
        JsString(name.0[..separator].to_vec()),
        JsString(name.0[separator + 1..].to_vec()),
    ))
}
pub(crate) fn truthy(value: Option<&IvyJson>) -> bool {
    match value {
        None | Some(IvyJson::Null) => false,
        Some(IvyJson::Bool(value)) => *value,
        Some(IvyJson::Number(value)) => *value != 0.0 && !value.is_nan(),
        Some(IvyJson::String(value)) => !value.0.is_empty(),
        _ => true,
    }
}
pub async fn discover(
    client: &dyn RpcClient,
    qualified_name: &JsString,
    target: &IvyJson,
) -> IvyResult<Arc<BoundTool>> {
    discover_with_boundary(client, qualified_name, target, IvyError::internal).await
}
// Keep local response-access failures distinct from coded RPC/requireThat errors
// for helpers whose original caller catches only instanceof IvyError.
pub(crate) async fn discover_with_boundary<E: From<IvyError>>(
    client: &dyn RpcClient,
    qualified_name: &JsString,
    target: &IvyJson,
    boundary: fn() -> E,
) -> Result<Arc<BoundTool>, E> {
    let (namespace, name) = name_parts(qualified_name)?;
    let mut selectors = target.clone();
    let interface = selectors.remove("interfaceVersion");
    let mut cursor = None;
    loop {
        let mut params = IvyJson::object([("namespace", IvyJson::String(namespace.clone()))]);
        for (key, value) in selectors.as_object().ok_or_else(boundary)? {
            params.insert(key.clone(), value.clone());
        }
        params.insert("namePrefix", IvyJson::String(name.clone()));
        if let Some(cursor) = cursor {
            params.insert("cursor", cursor);
        }
        let page = client
            .request("tools.list", params, RequestOptions::default())
            .await?;
        let items = page
            .get("items")
            .and_then(IvyJson::as_array)
            .ok_or_else(boundary)?;
        let mut selected = None;
        for item in items {
            if item.is_null() {
                return Err(boundary());
            }
            if item.get("qualifiedName").and_then(IvyJson::as_js_string) == Some(qualified_name) {
                selected = Some(item);
                break;
            }
        }
        if let Some(binding) = selected {
            if let Some(version) = &interface {
                let definition = binding
                    .get("definition")
                    .filter(|v| !v.is_null())
                    .ok_or_else(boundary)?;
                if definition.get("interfaceVersion") != Some(version) {
                    return Err(IvyError::new(
                        "contract_version_conflict",
                        "The selected provider exposes another namespace interface version.",
                    )
                    .into());
                }
            }
            let mut value = binding.clone();
            let provider = page
                .get("provider")
                .filter(|v| !v.is_null())
                .ok_or_else(boundary)?;
            let node = provider
                .get("node")
                .filter(|v| !v.is_null())
                .ok_or_else(boundary)?;
            if let Some(id) = node.get("serviceNodeId") {
                value.insert("serviceNodeId", id.clone());
            } else {
                value.remove("serviceNodeId");
            }
            if let Some(reference) = selectors.get("resourceRef").filter(|r| truthy(Some(r))) {
                value.insert("resourceRef", reference.clone());
            }
            return Ok(Arc::new(BoundTool { value }));
        }
        cursor = page.get("nextCursor").filter(|v| truthy(Some(v))).cloned();
        if cursor.is_none() {
            return Err(IvyError::new(
                "not_found",
                "The exact tool is not in the selected provider catalog.",
            )
            .into());
        }
    }
}
#[derive(Clone, Default)]
pub struct ToolCallOptions {
    pub request: RequestOptions,
    pub expected_caller_principal_id: Option<JsString>,
}
pub async fn call_bound(
    client: &dyn RpcClient,
    binding: &BoundTool,
    args: IvyJson,
    operation_id: Option<JsString>,
    options: ToolCallOptions,
) -> IvyResult<IvyJson> {
    call_bound_with_boundary(
        client,
        binding,
        args,
        operation_id,
        options,
        IvyError::internal,
    )
    .await
}
pub(crate) async fn call_bound_with_boundary<E: From<IvyError>>(
    client: &dyn RpcClient,
    binding: &BoundTool,
    args: IvyJson,
    operation_id: Option<JsString>,
    mut options: ToolCallOptions,
    boundary: fn() -> E,
) -> Result<IvyJson, E> {
    let mut params = IvyJson::object(Vec::<(&str, IvyJson)>::new());
    // A missing JS property is omitted when the request is serialized. Do not
    // replace the original provider call with an invented local validation error.
    for (source, target) in [
        ("qualifiedName", "qualifiedName"),
        ("serviceNodeId", "serviceNodeId"),
        ("definitionHash", "expectedDefinitionHash"),
    ] {
        if let Some(value) = binding.value.get(source) {
            params.insert(target, value.clone());
        }
    }
    params.insert("arguments", args);
    if let Some(principal) = options.expected_caller_principal_id {
        params.insert("expectedCallerPrincipalId", IvyJson::String(principal));
    }
    if let Some(reference) = binding.value.get("resourceRef").filter(|r| truthy(Some(r))) {
        params.insert("resourceRef", reference.clone());
    }
    if let Some(operation) = operation_id {
        params.insert("operationId", IvyJson::String(operation));
    }
    binding
        .value
        .get("definition")
        .filter(|v| !v.is_null())
        .ok_or_else(boundary)?;
    options.request.bound_read = Some(BoundRead(binding.read_only()));
    Ok(client
        .request("tools.call", params, options.request)
        .await?)
}
#[derive(Clone)]
pub struct InterfaceRequirement {
    pub namespace: String,
    pub interface_version: String,
}
/// Keep one of these for each RPC connection to reuse its selected provider bindings.
pub struct ToolClients {
    client: Arc<dyn RpcClient>,
    providers: Mutex<HashMap<String, BoundToolClient>>,
    pub(crate) native_contracts: crate::native_owner::ContractCache,
}
impl ToolClients {
    pub fn new(client: Arc<dyn RpcClient>) -> Self {
        Self {
            client,
            providers: Mutex::new(HashMap::new()),
            native_contracts: Default::default(),
        }
    }
    pub(crate) fn client(&self) -> &dyn RpcClient {
        self.client.as_ref()
    }
    pub fn service_tools(
        &self,
        service_node_id: JsString,
        requirements: &[InterfaceRequirement],
    ) -> IvyResult<BoundToolClient> {
        if requirements.is_empty() {
            return Err(IvyError::new(
                "invalid_arguments",
                "Stable service Tools require at least one namespace interface version.",
            ));
        }
        self.selected(service_node_id, requirements, None)
    }
    pub fn native_service_tools(&self, service_node_id: JsString) -> IvyResult<BoundToolClient> {
        self.selected(service_node_id, &[], Some("codex".into()))
    }
    fn selected(
        &self,
        service_node_id: JsString,
        requirements: &[InterfaceRequirement],
        dynamic: Option<JsString>,
    ) -> IvyResult<BoundToolClient> {
        let mut sorted = requirements.to_vec();
        sorted.sort_by(|a, b| a.namespace.cmp(&b.namespace));
        let key = ivy_contracts::json::canonical(
            &IvyJson::object([
                ("serviceNodeId", IvyJson::String(service_node_id.clone())),
                (
                    "dynamicNamespace",
                    dynamic.clone().map_or(IvyJson::Null, IvyJson::String),
                ),
                (
                    "requirements",
                    IvyJson::Array(
                        sorted
                            .iter()
                            .map(|r| {
                                IvyJson::object([
                                    ("namespace", r.namespace.clone().into()),
                                    ("interfaceVersion", r.interface_version.clone().into()),
                                ])
                            })
                            .collect(),
                    ),
                ),
            ]),
            ivy_contracts::limits::MANAGEMENT_FRAME_BYTES,
        )?;
        let mut providers = self.providers.lock().unwrap();
        if let Some(provider) = providers.get(&key) {
            return Ok(provider.clone());
        }
        if providers.len() >= 64 {
            return Err(IvyError::new(
                "limit_exceeded",
                "The RPC client has too many selected Tool providers.",
            ));
        }
        let provider =
            BoundToolClient::new(self.client.clone(), service_node_id, requirements, dynamic)?;
        providers.insert(key, provider.clone());
        Ok(provider)
    }
}
type Lookup = Shared<BoxFuture<'static, IvyResult<Arc<BoundTool>>>>;
struct BindingEntry {
    lookup: Lookup,
}
struct Inner {
    client: Arc<dyn RpcClient>,
    service_node_id: JsString,
    interfaces: HashMap<JsString, String>,
    dynamic_namespace: Option<JsString>,
    bindings: Mutex<HashMap<JsString, Arc<BindingEntry>>>,
}
#[derive(Clone)]
pub struct BoundToolClient {
    inner: Arc<Inner>,
}
impl BoundToolClient {
    pub fn new(
        client: Arc<dyn RpcClient>,
        service_node_id: JsString,
        requirements: &[InterfaceRequirement],
        dynamic_namespace: Option<JsString>,
    ) -> IvyResult<Self> {
        if service_node_id.0.is_empty() {
            return Err(IvyError::new(
                "invalid_arguments",
                "A bound Tool client requires one exact service node.",
            ));
        }
        if requirements.is_empty() && dynamic_namespace.is_none() {
            return Err(IvyError::new(
                "invalid_arguments",
                "A bound Tool client requires an interface version or an explicit native namespace.",
            ));
        }
        let mut interfaces = HashMap::new();
        for requirement in requirements {
            let name = &requirement.namespace;
            let version = &requirement.interface_version;
            let parts: Vec<_> = version.split('.').collect();
            if name.is_empty()
                || name.len() > 64
                || !name.as_bytes()[0].is_ascii_lowercase()
                || !name
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
                || parts.len() != 3
                || parts.iter().any(|p| {
                    p.is_empty()
                        || !p.bytes().all(|b| b.is_ascii_digit())
                        || p.len() > 1 && p.starts_with('0')
                })
            {
                return Err(IvyError::new(
                    "invalid_arguments",
                    "A bound Tool interface requirement is invalid.",
                ));
            }
            if interfaces
                .insert(name.as_str().into(), version.clone())
                .is_some()
            {
                return Err(IvyError::new(
                    "invalid_arguments",
                    "A bound Tool client cannot declare the same namespace twice.",
                ));
            }
        }
        Ok(Self {
            inner: Arc::new(Inner {
                client,
                service_node_id,
                interfaces,
                dynamic_namespace,
                bindings: Mutex::new(HashMap::new()),
            }),
        })
    }
    fn target(&self, qualified_name: &JsString) -> IvyResult<IvyJson> {
        let (namespace, _) = name_parts(qualified_name)?;
        let interface = self.inner.interfaces.get(&namespace);
        if interface.is_none() && Some(&namespace) != self.inner.dynamic_namespace.as_ref() {
            return Err(IvyError::new(
                "invalid_arguments",
                "The Tool namespace has no declared interface requirement.",
            ));
        }
        let mut target = IvyJson::object([(
            "serviceNodeId",
            IvyJson::String(self.inner.service_node_id.clone()),
        )]);
        if let Some(version) = interface {
            target.insert("interfaceVersion", version.clone().into());
        }
        Ok(target)
    }
    pub async fn binding(
        &self,
        name: &JsString,
        expected_definition_hash: Option<&IvyJson>,
    ) -> IvyResult<Arc<BoundTool>> {
        let entry = {
            let mut bindings = self.inner.bindings.lock().unwrap();
            if let Some(entry) = bindings.get(name) {
                entry.clone()
            } else {
                if bindings.len() >= 256 {
                    return Err(IvyError::new(
                        "limit_exceeded",
                        "The selected provider binding cache is full.",
                    ));
                }
                let target = self.target(name)?;
                let client = self.inner.client.clone();
                let name = name.clone();
                let lookup_name = name.clone();
                let selected = self.inner.service_node_id.clone();
                let lookup = async move {
                    let binding = discover(client.as_ref(), &lookup_name, &target).await?;
                    if binding.service_node_id()? != selected {
                        return Err(IvyError::new(
                            "target_conflict",
                            "Discovery returned another selected provider.",
                        ));
                    }
                    Ok(binding)
                }
                .boxed()
                .shared();
                let entry = Arc::new(BindingEntry { lookup });
                bindings.insert(name.clone(), entry.clone());
                // Discovery progresses independently of a caller abandoning its observation.
                let weak = Arc::downgrade(&self.inner);
                let pending = entry.clone();
                tokio::spawn(async move {
                    if pending.lookup.clone().await.is_err() {
                        if let Some(inner) = weak.upgrade() {
                            let mut bindings = inner.bindings.lock().unwrap();
                            if bindings
                                .get(&name)
                                .is_some_and(|current| Arc::ptr_eq(current, &pending))
                            {
                                bindings.remove(&name);
                            }
                        }
                    }
                });
                entry
            }
        };
        let binding = entry.lookup.clone().await?;
        if expected_definition_hash
            .is_some_and(|hash| binding.value.get("definitionHash") != Some(hash))
        {
            return Err(IvyError::new(
                "tool_definition_changed",
                "The selected Tool differs from the retained action definition.",
            ));
        }
        Ok(binding)
    }
    pub async fn refresh(&self, binding: &Arc<BoundTool>) -> IvyResult<Arc<BoundTool>> {
        if binding.service_node_id()? != self.inner.service_node_id {
            return Err(IvyError::new(
                "target_conflict",
                "Only the selected provider's binding can be refreshed.",
            ));
        }
        let name = binding.qualified_name()?;
        let entry = self.inner.bindings.lock().unwrap().get(&name).cloned();
        if let Some(entry) = entry {
            if Arc::ptr_eq(&entry.lookup.clone().await?, binding) {
                let mut bindings = self.inner.bindings.lock().unwrap();
                if bindings
                    .get(&name)
                    .is_some_and(|current| Arc::ptr_eq(current, &entry))
                {
                    bindings.remove(&name);
                }
            }
        }
        self.binding(&name, None).await
    }
    pub async fn call(
        &self,
        name: &JsString,
        args: IvyJson,
        operation_id: Option<JsString>,
        options: ToolCallOptions,
    ) -> IvyResult<IvyJson> {
        if options.request.cancellation.is_cancelled() {
            return Err(options.request.cancel_error.clone());
        }
        let binding = self.binding(name, None).await?;
        if options.request.cancellation.is_cancelled() {
            return Err(options.request.cancel_error.clone());
        }
        if binding.read_only() {
            self.read_bound(binding, args, operation_id, options).await
        } else {
            call_bound(
                self.inner.client.as_ref(),
                &binding,
                args,
                operation_id,
                options,
            )
            .await
        }
    }
    pub async fn read(
        &self,
        name: &JsString,
        args: IvyJson,
        options: ToolCallOptions,
    ) -> IvyResult<IvyJson> {
        if options.request.cancellation.is_cancelled() {
            return Err(options.request.cancel_error.clone());
        }
        let binding = self.binding(name, None).await?;
        if !binding.read_only() {
            return Err(IvyError::new(
                "invalid_arguments",
                "A read requires a read-only Tool.",
            ));
        }
        self.read_bound(binding, args, None, options).await
    }
    async fn read_bound(
        &self,
        binding: Arc<BoundTool>,
        args: IvyJson,
        operation_id: Option<JsString>,
        options: ToolCallOptions,
    ) -> IvyResult<IvyJson> {
        let failure = match call_bound(
            self.inner.client.as_ref(),
            &binding,
            args.clone(),
            operation_id.clone(),
            options.clone(),
        )
        .await
        {
            Ok(result) => return Ok(result),
            Err(error) => error,
        };
        if failure.code != "tool_definition_changed" || failure.outcome != Outcome::NotExecuted {
            return Err(failure);
        }
        let name = binding.qualified_name()?;
        let fresh = discover(self.inner.client.as_ref(), &name, &self.target(&name)?).await?;
        if fresh.definition_hash()? == binding.definition_hash()?
            || !fresh.read_only()
            || fresh.interface_version()? != binding.interface_version()?
        {
            return Err(failure);
        }
        let entry = self.inner.bindings.lock().unwrap().get(&name).cloned();
        if let Some(entry) = entry {
            if Arc::ptr_eq(&entry.lookup.clone().await?, &binding) {
                let mut bindings = self.inner.bindings.lock().unwrap();
                if bindings
                    .get(&name)
                    .is_some_and(|current| Arc::ptr_eq(current, &entry))
                {
                    let replacement = fresh.clone();
                    bindings.insert(
                        name,
                        Arc::new(BindingEntry {
                            lookup: async move { Ok(replacement) }.boxed().shared(),
                        }),
                    );
                }
            }
        }
        if options.request.cancellation.is_cancelled() {
            return Err(options.request.cancel_error.clone());
        }
        call_bound(
            self.inner.client.as_ref(),
            &fresh,
            args,
            operation_id,
            options,
        )
        .await
    }
}
