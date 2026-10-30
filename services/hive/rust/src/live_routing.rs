//! Network routing index. The DB owner publishes admitted node/catalog updates;
//! only the live socket generation can accept or complete a provider call.
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString, Outcome, limits::MANAGEMENT_FRAME_BYTES,
    schema::SchemaValidators,
};
use crate::registry_catalog::Catalog;
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

fn field<'a>(value: &'a IvyJson, key: &str) -> IvyResult<&'a IvyJson> {
    value.get(key).ok_or_else(IvyError::internal)
}

fn string(value: &IvyJson, key: &str) -> IvyResult<JsString> {
    field(value, key)?
        .as_js_string()
        .cloned()
        .ok_or_else(IvyError::internal)
}
fn require(condition: bool, code: &str, message: &str) -> IvyResult<()> {
    if condition {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn key(parts: impl IntoIterator<Item = JsString>) -> JsString {
    let mut result = vec![];
    for (index, part) in parts.into_iter().enumerate() {
        if index > 0 {
            result.push(0);
        }
        result.extend(part.0);
    }
    JsString(result)
}
fn nullable<'a>(value: &'a IvyJson, name: &str) -> Option<&'a IvyJson> {
    value.get(name).filter(|v| !v.is_null())
}

#[derive(Debug)]
pub struct CallContext {
    pub service_node_id: JsString,
    pub generation: f64,
    pub qualified_name: JsString,
    pub output_schema: Option<Arc<IvyJson>>,
    pub caller_principal_id: JsString,
}
#[derive(Debug)]
pub struct PreparedCall {
    pub context: CallContext,
    pub definition_hash: JsString,
    pub arguments: IvyJson,
    pub operation_id: Option<IvyJson>,
}
struct Schemas {
    input: Arc<IvyJson>,
    output: Arc<IvyJson>,
}
struct Tool {
    definition_hash: JsString,
    mcp: bool,
    read_only: bool,
    schemas: Option<Schemas>,
}
/// Routing needs identities and availability, not the full registered status.
struct Node {
    id: JsString,
    host: Option<JsString>,
    service: Option<JsString>,
    principal: Option<JsString>,
    connected: bool,
    synced: bool,
    ready: bool,
    enabled: bool,
}
impl Node {
    fn from_status(value: &IvyJson, id: JsString) -> Self {
        let text = |key| value.get(key).and_then(IvyJson::as_js_string).cloned();
        let flag = |key| value.get(key).and_then(IvyJson::as_bool) == Some(true);
        Self {
            id,
            host: text("hostId"),
            service: text("serviceName"),
            principal: text("principalId"),
            connected: flag("connected"),
            synced: flag("synced"),
            ready: flag("ready"),
            enabled: flag("desiredEnabled"),
        }
    }
    fn eligible(&self) -> bool {
        self.connected && self.synced && self.ready && self.enabled
    }
    fn disconnect(&mut self) {
        self.connected = false;
        self.synced = false;
        self.ready = false;
    }
    fn target(&self) -> IvyJson {
        IvyJson::object([
            ("serviceNodeId", self.id.clone().into()),
            ("hostId", self.host.clone().map_or(IvyJson::Null, Into::into)),
            ("serviceName", self.service.clone().map_or(IvyJson::Null, Into::into)),
        ])
    }
}
struct Route {
    node: Node,
    generation: f64,
    namespaces: HashSet<JsString>,
    tools: HashMap<JsString, Tool>,
    notifications: HashMap<JsString, Arc<IvyJson>>,
}
impl Route {
    fn update(&mut self, catalog: Option<&Catalog>, validate: bool) -> IvyResult<()> {
        if let Some(catalog) = catalog {
            self.namespaces = catalog.namespaces
                .iter()
                .map(|ns| ns.name.clone())
                .collect();
            self.tools.clear();
            for binding in &catalog.tools
            {
                let definition = &binding.metadata;
                self.tools.insert(
                    binding.qualified_name.clone(),
                    Tool {
                        definition_hash: binding.definition_hash.clone(),
                        mcp: definition
                            .get("discovery")
                            .and_then(|d| d.get("mcp"))
                            .is_some_and(|value| value.as_object().is_some()),
                        read_only: definition
                            .get("annotations")
                            .and_then(|a| a.get("readOnlyHint"))
                            .and_then(IvyJson::as_bool)
                            == Some(true),
                        schemas: if validate {
                            Some(Schemas {
                                input: binding.input_schema.as_ref().ok_or_else(IvyError::internal)?.clone(),
                                output: binding.output_schema.as_ref().ok_or_else(IvyError::internal)?.clone(),
                            })
                        } else {
                            None
                        },
                    },
                );
            }
            let mut notifications = HashMap::new();
            for notification in &catalog.notifications {
                notifications.insert(
                    key([
                        notification.namespace.clone(),
                        notification.name.clone(),
                        notification.version.clone(),
                    ]),
                    notification.payload_schema.clone(),
                );
            }
            self.notifications = notifications;
        }
        Ok(())
    }
}
pub type RejectedBinding = Box<dyn Fn(IvyJson) -> IvyResult<()> + Send + Sync>;
pub struct LiveRouting {
    validators: Option<SchemaValidators>,
    notification_validators: SchemaValidators,
    routes: Vec<Route>,
    rejected: Option<RejectedBinding>,
}
impl LiveRouting {
    pub fn new(validate_messages: bool, rejected: Option<RejectedBinding>) -> Self {
        Self {
            validators: validate_messages.then(SchemaValidators::new),
            notification_validators: SchemaValidators::new(),
            routes: vec![],
            rejected,
        }
    }
    fn route(&self, id: &JsString) -> Option<&Route> {
        self.routes
            .iter()
            .find(|route| &route.node.id == id)
    }
    fn route_mut(&mut self, id: &JsString) -> Option<&mut Route> {
        self.routes
            .iter_mut()
            .find(|route| &route.node.id == id)
    }
    pub fn clear(&mut self) {
        self.routes.clear();
    }
    pub fn retire_unconfigured_principals(&mut self, principal_ids: &[JsString]) {
        for route in &mut self.routes {
            if !route
                .node
                .principal.as_ref()
                .is_some_and(|id| principal_ids.contains(id))
            {
                route.node.disconnect();
            }
        }
    }
    /// The worker owner supplies an admitted RoutingUpdate. A generation-only
    /// notification may reuse a catalog solely within that exact generation.
    pub fn update(
        &mut self,
        node: &IvyJson,
        generation: Option<f64>,
        catalog: Option<&Catalog>,
    ) -> IvyResult<()> {
        let id = string(node, "serviceNodeId")?;
        let prior = self.routes.iter().position(|route| route.node.id == id);
        if prior.is_none() && generation.is_none() {
            return Ok(());
        }
        let generation = generation.unwrap_or_else(|| self.routes[prior.unwrap()].generation);
        let node = Node::from_status(node, id);
        if let Some(index) = prior.filter(|i| self.routes[*i].generation == generation) {
            self.routes[index].node = node;
            return self.routes[index]
                .update(catalog, self.validators.is_some());
        }
        let mut route = Route {
            node,
            generation,
            namespaces: HashSet::new(),
            tools: HashMap::new(),
            notifications: HashMap::new(),
        };
        route.update(catalog, self.validators.is_some())?;
        if let Some(index) = prior {
            self.routes[index] = route;
        } else {
            self.routes.push(route);
        }
        Ok(())
    }
    pub fn disconnect(&mut self, id: &JsString, generation: f64) -> bool {
        let Some(route) = self.route_mut(id).filter(|r| r.generation == generation) else {
            return false;
        };
        route.node.disconnect();
        true
    }
    pub fn suspend(&mut self, id: &JsString, generation: f64) {
        if let Some(route) = self.route_mut(id).filter(|r| r.generation == generation) {
            route.node.synced = false;
        }
    }
    pub fn caller(&self, context: &IvyJson) -> IvyResult<()> {
        if context
            .get("serviceNodeId")
            .and_then(IvyJson::as_js_string)
            .is_some_and(|v| !v.0.is_empty())
        {
            let id = string(context, "serviceNodeId")?;
            require(
                self.route(&id).is_some_and(|r| {
                    r.node.connected
                        && context.get("generation").and_then(IvyJson::as_f64) == Some(r.generation)
                }),
                "stale_generation",
                "Service connection is no longer current.",
            )?;
        }
        Ok(())
    }
    pub fn notification(&self, context: &IvyJson, params: &IvyJson) -> IvyResult<()> {
        self.caller(context)?;
        let name = key([
            string(params, "namespace")?,
            string(params, "name")?,
            string(params, "version")?,
        ]);
        let schema=context.get("serviceNodeId").and_then(IvyJson::as_js_string).and_then(|id|self.route(id)).and_then(|r|r.notifications.get(&name)).ok_or_else(||IvyError::new("not_found","Transient notification name and version are not registered by this connection."))?;
        self.notification_validators
            .validate(schema, field(params, "payload")?, 1024 * 1024)
    }
    pub fn prepare(&self, context: &IvyJson, params: &IvyJson) -> IvyResult<PreparedCall> {
        self.caller(context)?;
        require(
            context
                .get("principalId")
                .and_then(IvyJson::as_js_string)
                .is_some_and(|v| !v.0.is_empty())
                && (params.get("expectedCallerPrincipalId").is_none()
                    || params.get("expectedCallerPrincipalId") == context.get("principalId")),
            "caller_changed",
            "Caller identity differs from the connected principal.",
        )?;
        let qualified = string(params, "qualifiedName")?;
        let namespace = JsString(
            qualified
                .0
                .split(|c| *c == 46)
                .next()
                .unwrap_or_default()
                .to_vec(),
        );
        let resource = params.get("resourceRef");
        let selected = nullable(params, "serviceNodeId")
            .or_else(|| resource.and_then(|r| r.get("serviceNodeId")));
        require(
            params
                .get("serviceNodeId")
                .and_then(IvyJson::as_js_string)
                .is_none_or(|v| v.0.is_empty())
                || resource.is_none_or(|v| v.as_object().is_none())
                || params.get("serviceNodeId") == resource.and_then(|r| r.get("serviceNodeId")),
            "target_conflict",
            "Resource and node selectors disagree.",
        )?;
        require(
            resource.is_none_or(|v| v.as_object().is_none())
                || resource.and_then(|r| r.get("namespace"))
                    == Some(&IvyJson::String(namespace.clone())),
            "target_conflict",
            "Resource and namespace selectors disagree.",
        )?;
        let matches = |route: &Route| {
            (params
                .get("hostId")
                .and_then(IvyJson::as_js_string)
                .is_none_or(|v| v.0.is_empty())
                || route.node.host.as_ref() == params.get("hostId").and_then(IvyJson::as_js_string))
                && (params
                    .get("serviceName")
                    .and_then(IvyJson::as_js_string)
                    .is_none_or(|v| v.0.is_empty())
                    || route.node.service.as_ref() == params.get("serviceName").and_then(IvyJson::as_js_string))
        };
        let route = if selected
            .and_then(IvyJson::as_js_string)
            .is_some_and(|v| !v.0.is_empty())
        {
            let selected = selected
                .and_then(IvyJson::as_js_string)
                .ok_or_else(IvyError::internal)?;
            let route = self
                .route(selected)
                .ok_or_else(|| IvyError::new("not_found", "Service Node not found."))?;
            require(
                matches(route),
                "target_conflict",
                "Selected resource has a different owner.",
            )?;
            require(
                route.node.connected,
                "service_unavailable",
                "Provider connection is closed.",
            )?;
            require(
                route.node.eligible(),
                "service_not_ready",
                "Provider is not ready.",
            )?;
            require(
                route.namespaces.contains(&namespace),
                "not_found",
                "Namespace is absent from the selected provider.",
            )?;
            route
        } else {
            let candidates: Vec<_> = self
                .routes
                .iter()
                .filter(|r| matches(r) && r.namespaces.contains(&namespace) && r.node.eligible())
                .collect();
            if candidates.len() > 1 {
                return Err(IvyError::new(
                    "ambiguous_service_node",
                    "Choose the owning Service Node.",
                )
                .with_details(IvyJson::Array(
                    candidates
                        .iter()
                        .map(|r| r.node.target())
                        .collect(),
                )));
            }
            candidates.first().copied().ok_or_else(|| {
                IvyError::new(
                    "service_unavailable",
                    "No eligible provider matches the target.",
                )
            })?
        };
        let binding = route.tools.get(&qualified).ok_or_else(|| {
            IvyError::new("not_found", "Tool is absent from the selected provider.")
        })?;
        require(
            context.get("transport") != Some(&"mcp".into()) || binding.mcp,
            "not_found",
            "Tool is not published through MCP.",
        )?;
        if Some(&binding.definition_hash)
            != params
                .get("expectedDefinitionHash")
                .and_then(IvyJson::as_js_string)
        {
            if let Some(rejected) = &self.rejected {
                rejected(IvyJson::object([
                    ("callerPrincipalId", field(context, "principalId")?.clone()),
                    (
                        "serviceNodeId",
                        route.node.id.clone().into(),
                    ),
                    ("qualifiedName", IvyJson::String(qualified)),
                ]))?;
            }
            return Err(IvyError::new(
                "tool_definition_changed",
                "Tool definition changed.",
            ));
        }
        let arguments = field(params, "arguments")?;
        if let (Some(validators), Some(schemas)) = (&self.validators, &binding.schemas) {
            validators.validate(&schemas.input, arguments, MANAGEMENT_FRAME_BYTES)?;
        }
        Ok(PreparedCall {
            context: CallContext {
                service_node_id: route.node.id.clone(),
                generation: route.generation,
                qualified_name: qualified,
                output_schema: binding
                    .schemas
                    .as_ref()
                    .map(|schemas| schemas.output.clone()),
                caller_principal_id: string(context, "principalId")?,
            },
            definition_hash: binding.definition_hash.clone(),
            arguments: arguments.clone(),
            operation_id: params.get("operationId").cloned(),
        })
    }
    pub fn prepare_dashboard_image(
        &self,
        context: &IvyJson,
        node: &JsString,
        args: &IvyJson,
    ) -> IvyResult<PreparedCall> {
        let binding = self
            .route(node)
            .and_then(|r| r.tools.get(&JsString::from("dashboards.image")))
            .ok_or_else(|| {
                IvyError::new("not_found", "Dashboard image endpoint is unavailable.")
            })?;
        require(
            binding.read_only,
            "provider_contract_error",
            "Dashboard image handler must be read-only.",
        )?;
        self.prepare(
            context,
            &IvyJson::object([
                ("serviceNodeId", IvyJson::String(node.clone())),
                ("serviceName", "dashboards".into()),
                ("qualifiedName", "dashboards.image".into()),
                (
                    "expectedDefinitionHash",
                    IvyJson::String(binding.definition_hash.clone()),
                ),
                ("arguments", args.clone()),
            ]),
        )
    }
    pub fn complete(&self, call: &CallContext, result: IvyJson) -> IvyResult<IvyJson> {
        require(
            self.route(&call.service_node_id).is_some_and(|r| {
                r.node.connected
                    && r.generation == call.generation
            }),
            "stale_generation",
            "Provider result belongs to an expired connection.",
        )
        .map_err(|e| e.with_outcome(Outcome::Unknown))?;
        if let Some(validators) = &self.validators {
            if call
                .output_schema
                .as_deref()
                .ok_or_else(IvyError::internal)
                .and_then(|schema| validators.validate(schema, &result, MANAGEMENT_FRAME_BYTES))
                .is_err()
            {
                return Err(IvyError::new(
                    "provider_contract_error",
                    "Provider returned output incompatible with the dispatched definition.",
                )
                .with_outcome(Outcome::Unknown));
            }
        }
        Ok(result)
    }
}

#[cfg(test)]
#[path = "../../../../tests/rust/hive_routing_ownership.rs"]
mod ownership_tests;
