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
    input: IvyJson,
    output: Arc<IvyJson>,
}
struct Tool {
    definition_hash: JsString,
    mcp: bool,
    read_only: bool,
    schemas: Option<Schemas>,
}
struct Route {
    node: IvyJson,
    generation: f64,
    namespaces: HashSet<JsString>,
    tools: HashMap<JsString, Tool>,
    notifications: HashMap<JsString, IvyJson>,
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
                let definition = &binding.definition;
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
                                input: field(definition, "inputSchema")?.clone(),
                                output: Arc::new(field(definition, "outputSchema")?.clone()),
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
                        string(notification, "namespace")?,
                        string(notification, "name")?,
                        string(notification, "version")?,
                    ]),
                    field(notification, "payloadSchema")?.clone(),
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
    routes: Vec<(JsString, Route)>,
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
            .find(|(key, _)| key == id)
            .map(|(_, value)| value)
    }
    fn route_mut(&mut self, id: &JsString) -> Option<&mut Route> {
        self.routes
            .iter_mut()
            .find(|(key, _)| key == id)
            .map(|(_, value)| value)
    }
    pub fn clear(&mut self) {
        self.routes.clear();
    }
    pub fn retire_unconfigured_principals(&mut self, principal_ids: &[JsString]) {
        for (_, route) in &mut self.routes {
            if !route
                .node
                .get("principalId")
                .and_then(IvyJson::as_js_string)
                .is_some_and(|id| principal_ids.contains(id))
            {
                for name in ["connected", "synced", "ready"] {
                    route.node.insert(name, false.into());
                }
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
        let prior = self.routes.iter().position(|(key, _)| key == &id);
        if prior.is_none() && generation.is_none() {
            return Ok(());
        }
        let generation = generation.unwrap_or_else(|| self.routes[prior.unwrap()].1.generation);
        if let Some(index) = prior.filter(|i| self.routes[*i].1.generation == generation) {
            self.routes[index].1.node = node.clone();
            return self.routes[index]
                .1
                .update(catalog, self.validators.is_some());
        }
        let mut route = Route {
            node: node.clone(),
            generation,
            namespaces: HashSet::new(),
            tools: HashMap::new(),
            notifications: HashMap::new(),
        };
        route.update(catalog, self.validators.is_some())?;
        if let Some(index) = prior {
            self.routes[index] = (id, route);
        } else {
            self.routes.push((id, route));
        }
        Ok(())
    }
    pub fn disconnect(&mut self, id: &JsString, generation: f64) -> bool {
        let Some(route) = self.route_mut(id).filter(|r| r.generation == generation) else {
            return false;
        };
        for name in ["connected", "synced", "ready"] {
            route.node.insert(name, false.into());
        }
        true
    }
    pub fn suspend(&mut self, id: &JsString, generation: f64) {
        if let Some(route) = self.route_mut(id).filter(|r| r.generation == generation) {
            route.node.insert("synced", false.into());
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
                    r.node.get("connected").and_then(IvyJson::as_bool) == Some(true)
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
                || route.node.get("hostId") == params.get("hostId"))
                && (params
                    .get("serviceName")
                    .and_then(IvyJson::as_js_string)
                    .is_none_or(|v| v.0.is_empty())
                    || route.node.get("serviceName") == params.get("serviceName"))
        };
        let eligible = |route: &Route| {
            ["connected", "synced", "ready", "desiredEnabled"]
                .into_iter()
                .all(|name| route.node.get(name).and_then(IvyJson::as_bool) == Some(true))
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
                route.node.get("connected").and_then(IvyJson::as_bool) == Some(true),
                "service_unavailable",
                "Provider connection is closed.",
            )?;
            require(
                eligible(route),
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
                .map(|(_, r)| r)
                .filter(|r| matches(r) && r.namespaces.contains(&namespace) && eligible(r))
                .collect();
            if candidates.len() > 1 {
                return Err(IvyError::new(
                    "ambiguous_service_node",
                    "Choose the owning Service Node.",
                )
                .with_details(IvyJson::Array(
                    candidates
                        .iter()
                        .map(|r| {
                            IvyJson::object([
                                (
                                    "serviceNodeId",
                                    r.node
                                        .get("serviceNodeId")
                                        .cloned()
                                        .unwrap_or(IvyJson::Null),
                                ),
                                (
                                    "hostId",
                                    r.node.get("hostId").cloned().unwrap_or(IvyJson::Null),
                                ),
                                (
                                    "serviceName",
                                    r.node.get("serviceName").cloned().unwrap_or(IvyJson::Null),
                                ),
                            ])
                        })
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
                        field(&route.node, "serviceNodeId")?.clone(),
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
                service_node_id: string(&route.node, "serviceNodeId")?,
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
                r.node.get("connected").and_then(IvyJson::as_bool) == Some(true)
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
