//! One service registry, with explicit native backend/account admission.
use crate::{host_metrics::HostMetrics, service_kernel::HandlerSet};
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString, Outcome, native_backend::BackendIdentity,
};
use ivy_host_runtime::config::InstanceConfig;
use ivy_sdk::service::{InvocationContext, ServiceFuture, ToolHandler};
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

pub struct BackendRoute {
    pub identity: Option<BackendIdentity>,
    pub registry: IvyJson,
    pub handlers: HandlerSet,
    pub preparation_failure: Option<Arc<BackendFailure>>,
}

/// Preparation did not establish a usable native owner. In particular, this
/// says nothing about the presence or outcome of previously accepted work.
pub struct BackendFailure {
    identity: BackendIdentity,
    service_node: String,
    host: String,
    native_version: String,
    executable_hash: String,
    code: String,
    failed_at: chrono::DateTime<chrono::Utc>,
    operation_reads_available: bool,
}
impl BackendFailure {
    pub(crate) fn new(
        config: &InstanceConfig,
        identity: BackendIdentity,
        error: &IvyError,
        operation_reads_available: bool,
    ) -> IvyResult<Self> {
        let value = config.json();
        let settings = value.required("settings")?;
        Ok(Self {
            identity,
            service_node: crate::service_kernel::text(value, "serviceNodeId")?.to_utf8()?,
            host: crate::service_kernel::text(value, "hostId")?.to_utf8()?,
            native_version: crate::service_kernel::text(settings, "nativeVersion")?.to_utf8()?,
            executable_hash: crate::service_kernel::text(settings, "nativeExecutableHash")?
                .to_utf8()?,
            code: error.code.clone(),
            failed_at: chrono::Utc::now(),
            operation_reads_available,
        })
    }
    pub(crate) fn snapshot(&self) -> IvyJson {
        annotate(
            IvyJson::object([
                ("serviceNodeId", self.service_node.as_str().into()),
                ("hostId", self.host.as_str().into()),
                ("nativeVersion", self.native_version.as_str().into()),
                ("nativeExecutableHash", self.executable_hash.as_str().into()),
                ("phase", "preparation".into()),
                ("state", "failed".into()),
                ("observedAt", IvyJson::String(crate::service_kernel::now())),
                ("code", self.code.as_str().into()),
                (
                    "operationReadsAvailable",
                    self.operation_reads_available.into(),
                ),
            ]),
            Some(&self.identity),
        )
    }
    pub(crate) fn diagnostic(&self) -> IvyJson {
        let at = crate::service_kernel::now();
        let resource = IvyJson::object([
            ("serviceNodeId", self.service_node.as_str().into()),
            ("backend", self.identity.json()),
        ]);
        IvyJson::object([
            ("code", self.code.as_str().into()),
            ("severity", "error".into()),
            ("source", self.service_node.as_str().into()),
            ("status", "current".into()),
            (
                "message",
                "Native backend preparation failed; retained operation outcomes are unavailable."
                    .into(),
            ),
            ("resource", resource),
            (
                "firstObservedAt",
                self.failed_at
                    .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
                    .into(),
            ),
            ("lastObservedAt", IvyJson::String(at)),
        ])
    }
}
impl BackendRoute {
    pub(crate) fn failed(failure: Arc<BackendFailure>) -> IvyResult<Self> {
        let registry = crate::registry::management_registry()?;
        let mut management = HashMap::new();
        let rejected = failure.clone();
        let handler: ToolHandler = Arc::new(move |_, context| {
            let rejected = rejected.clone();
            Box::pin(async move {
                context.throw_if_aborted()?;
                Err(IvyError::new(rejected.code.as_str(), "The selected backend could not finish preparation; no native action was admitted.").with_outcome(Outcome::NotExecuted))
            })
        });
        for namespace in registry
            .required("namespaces")?
            .as_array()
            .ok_or_else(IvyError::internal)?
        {
            for definition in namespace
                .required("tools")?
                .as_array()
                .ok_or_else(IvyError::internal)?
            {
                management.insert(
                    format!("agent.{}", definition.required("name")?.as_str()?).into(),
                    handler.clone(),
                );
            }
        }
        Ok(Self {
            identity: Some(failure.identity.clone()),
            registry,
            handlers: HandlerSet {
                management,
                native: HashMap::new(),
            },
            preparation_failure: Some(failure),
        })
    }
}

/// Selection is evaluated once, before any accepted backend work. This owns no
/// provider process, retries or alternate dispatch path.
pub struct BackendRoutes {
    routes: Arc<Vec<Route>>,
    registry: IvyJson,
    host_metrics: Arc<HostMetrics>,
}
struct Route {
    identity: Option<BackendIdentity>,
    handlers: HandlerSet,
    preparation_failure: Option<Arc<BackendFailure>>,
}
fn invalid(message: &str) -> IvyError {
    IvyError::new("native_backend_mismatch", message).with_outcome(Outcome::NotExecuted)
}
fn selection(routes: &[Route], args: &mut IvyJson) -> IvyResult<usize> {
    if args.as_object().is_none() {
        return Err(IvyError::new(
            "invalid_arguments",
            "Management arguments must be an object.",
        ));
    }
    let selected = BackendIdentity::optional(args.remove("backend").as_ref())?;
    if selected.is_none() && routes.len() == 1 && routes[0].identity.is_none() {
        return Ok(0);
    }
    let selected = selected.ok_or_else(|| {
        IvyError::new(
            "native_backend_required",
            "Select the backend, provider and account explicitly.",
        )
    })?;
    routes
        .iter()
        .position(|route| route.identity.as_ref() == Some(&selected))
        .ok_or_else(|| {
            invalid("The selected backend, provider or account is not configured on this owner.")
        })
}
fn annotate(mut value: IvyJson, identity: Option<&BackendIdentity>) -> IvyJson {
    if let Some(identity) = identity {
        value.insert("backend", identity.json());
    }
    value
}
fn envelope(name: &JsString) -> bool {
    [
        "agent.status",
        "agent.storageIdentity",
        "agent.discover",
        "agent.frameLimits",
        "agent.interact",
        "agent.interaction",
        "agent.read",
        "agent.inputs",
        "agent.inputDefinition",
        "agent.notifications",
        "agent.projects",
        "agent.resolveProject",
        "agent.resolveWorkspace",
        "agent.listDirectories",
        "agent.stageFile",
        "agent.configureCapabilities",
        "agent.capabilities",
        "agent.cleanupTaskOutputs",
    ]
    .iter()
    .any(|method| name.eq_str(method))
}
fn call(
    route: &Route,
    name: &JsString,
    args: IvyJson,
    context: InvocationContext,
) -> ServiceFuture<IvyJson> {
    let Some(handler) = route.handlers.management.get(name) else {
        return Box::pin(async {
            Err(IvyError::new(
                "native_method_unsupported",
                "The selected backend does not expose this management capability.",
            ))
        });
    };
    // Admission stays eager: dropping an unpolled response must not cancel work
    // already accepted by the backend's original work owner.
    let run = handler(args, context);
    let identity = route.identity.clone();
    let is_envelope = envelope(name);
    Box::pin(async move {
        match run.await {
            Ok(value) => Ok(if is_envelope {
                annotate(value, identity.as_ref())
            } else {
                value
            }),
            Err(mut error) => {
                if let Some(details) = error.details.take() {
                    error.details = Some(
                        if details
                            .get("kind")
                            .is_some_and(|kind| kind.is_str("agent_operation_absent"))
                        {
                            annotate(details, identity.as_ref())
                        } else {
                            details
                        },
                    );
                }
                Err(error)
            }
        }
    })
}
impl BackendRoutes {
    pub fn new(mut routes: Vec<BackendRoute>) -> IvyResult<Self> {
        if routes.is_empty()
            || routes.len() > 8
            || (routes.len() > 1 && routes.iter().any(|route| route.identity.is_none()))
        {
            return Err(invalid(
                "Configure one to eight explicitly identified backends; an unscoped owner must stand alone.",
            ));
        }
        let mut ids = HashSet::new();
        for route in &routes {
            if !ids.insert(route.identity.as_ref().map_or("codex", BackendIdentity::id)) {
                return Err(invalid("Backend identifiers must be unique on a host."));
            }
        }
        let mut registry = std::mem::replace(&mut routes[0].registry, IvyJson::Null);
        let first_namespaces = registry
            .remove("namespaces")
            .ok_or_else(IvyError::internal)?;
        let mut first_namespaces = Some(first_namespaces);
        let mut namespaces = Vec::new();
        let mut names = HashSet::new();
        let mut native_names = HashSet::new();
        for (index, route) in routes.iter_mut().enumerate() {
            let own = route.identity.as_ref().map_or("codex", BackendIdentity::id);
            let prefix = format!("{own}.");
            for name in route.handlers.native.keys() {
                if !name.to_utf8()?.starts_with(&prefix) || !native_names.insert(name.clone()) {
                    return Err(invalid(
                        "Native handlers must belong to their unique backend namespace.",
                    ));
                }
            }
            let source = if index == 0 {
                first_namespaces.take().unwrap()
            } else {
                route
                    .registry
                    .remove("namespaces")
                    .ok_or_else(IvyError::internal)?
            };
            for namespace in source.into_array().ok_or_else(IvyError::internal)? {
                let name = namespace.required("namespace")?.as_str()?;
                if name == own || index == 0 {
                    if !names.insert(name) {
                        return Err(invalid(
                            "A backend namespace collides with another service namespace.",
                        ));
                    }
                    namespaces.push(namespace);
                }
            }
        }
        // All backends use the same management contract; native namespaces carry
        // their own pinned definitions and keep provider arguments unchanged.
        registry.insert("namespaces", IvyJson::Array(namespaces));
        Ok(Self {
            routes: Arc::new(
                routes
                    .into_iter()
                    .map(|route| Route {
                        identity: route.identity,
                        handlers: route.handlers,
                        preparation_failure: route.preparation_failure,
                    })
                    .collect(),
            ),
            registry,
            host_metrics: Arc::default(),
        })
    }
    pub fn registry(&self) -> &IvyJson {
        &self.registry
    }
    /// Host load is shared by all backends of this process.
    pub fn host_metrics(&self) -> &Arc<HostMetrics> {
        &self.host_metrics
    }
    pub fn handlers(&self) -> HandlerSet {
        let mut management = HashMap::new();
        let names: HashSet<_> = self
            .routes
            .iter()
            .flat_map(|route| route.handlers.management.keys().cloned())
            .collect();
        for name in names {
            let routes = self.routes.clone();
            let method = name.clone();
            let handler: ToolHandler =
                Arc::new(
                    move |mut args, context| match selection(&routes, &mut args) {
                        Ok(index) => call(&routes[index], &method, args, context),
                        Err(error) => Box::pin(async move { Err(error) }),
                    },
                );
            management.insert(name, handler);
        }
        let routes = self.routes.clone();
        let list: ToolHandler = Arc::new(move |args, context| {
            let routes = routes.clone();
            Box::pin(async move {
                ivy_contracts::agent_schema::validate_agent("BackendsInput", &args)?;
                let mut backends = Vec::with_capacity(routes.len());
                for route in routes.iter() {
                    if let Some(failure) = &route.preparation_failure {
                        context.throw_if_aborted()?;
                        backends.push(failure.snapshot());
                        continue;
                    }
                    backends.push(
                        call(
                            route,
                            &"agent.status".into(),
                            IvyJson::object::<&str>([]),
                            context.clone(),
                        )
                        .await?,
                    );
                }
                Ok(IvyJson::object([("backends", IvyJson::Array(backends))]))
            })
        });
        management.insert("agent.backends".into(), list);
        management.insert("agent.hostMetrics".into(), self.host_metrics.handler());
        let native = self
            .routes
            .iter()
            .flat_map(|route| {
                route
                    .handlers
                    .native
                    .iter()
                    .map(|(name, handler)| (name.clone(), handler.clone()))
            })
            .collect();
        HandlerSet { management, native }
    }
}
