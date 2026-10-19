//! Canonical request execution over the storage owner's initialized domains.
use crate::{
    configurations::{self, Configurations},
    discovery::Discovery,
    events,
    mcp::Runtime,
    objects,
    registry::{ConnectionContext, Registry},
    storage_read::{self, STORAGE_FORMAT},
    store::{AuthenticatedContext, Store},
    ui_files::UiFiles,
};
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString, Outcome,
    core_schema::{operation_definition, validate_input, validate_output},
    validation::validate_request,
};
use std::{cell::RefCell, collections::HashMap, rc::Rc, time::Duration};
#[path = "kernel_catalogs.rs"]
mod catalogs;
#[path = "kernel_wiki.rs"]
mod wiki;

pub struct Options {
    pub version: JsString,
    pub build_id: JsString,
    pub public_base_url: JsString,
    pub validate_messages: bool,
    pub reset_bootstrap_credential_digest: Option<JsString>,
}
/// Startup owns credential configuration, migrations and domain initialization.
/// The Registry is the same owner shared with the package catalog callback.
pub struct Parts {
    pub store: Store,
    pub registry: Rc<RefCell<Registry>>,
    pub discovery: Discovery,
    pub configurations: Configurations,
    pub ui_files: UiFiles,
}
pub struct Call<'a> {
    pub store: &'a mut Store,
    pub registry: &'a mut Registry,
    pub discovery: &'a mut Discovery,
    pub configurations: &'a Configurations,
    pub ui_files: &'a UiFiles,
    pub options: &'a Options,
    pub runtime: &'a dyn Runtime,
    pub clock: &'a dyn Fn() -> f64,
    pub connection_generation: &'a mut f64,
    handlers: &'a HashMap<JsString, Handler>,
}
pub type Handler = fn(&mut Call<'_>, &IvyJson, &IvyJson, &str) -> IvyResult<IvyJson>;

pub struct Kernel {
    pub parts: Parts,
    pub options: Options,
    pub(crate) runtime: Rc<dyn Runtime>,
    pub(crate) clock: Rc<dyn Fn() -> f64>,
    handlers: HashMap<JsString, Handler>,
    connection_generation: f64,
}
fn require(value: bool, code: &str, message: &str) -> IvyResult<()> {
    if value {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn field<'a>(value: &'a IvyJson, key: &str) -> IvyResult<&'a IvyJson> {
    value.get(key).ok_or_else(IvyError::internal)
}
fn string<'a>(value: &'a IvyJson, key: &str) -> IvyResult<&'a JsString> {
    field(value, key)?
        .as_js_string()
        .ok_or_else(IvyError::internal)
}
fn truthy(value: Option<&IvyJson>) -> bool {
    match value {
        None | Some(IvyJson::Null) => false,
        Some(IvyJson::Bool(v)) => *v,
        Some(IvyJson::Number(v)) => *v != 0.0 && !v.is_nan(),
        Some(IvyJson::String(v)) => !v.0.is_empty(),
        _ => true,
    }
}
fn is_text(value: Option<&IvyJson>, expected: &str) -> bool {
    value.and_then(IvyJson::as_js_string) == Some(&expected.into())
}
/// Context is captured by authenticated ingress, rather than request params.
pub fn service(context: &IvyJson) -> IvyResult<()> {
    require(
        is_text(context.get("transport"), "ws")
            && truthy(context.get("serviceNodeId"))
            && truthy(context.get("generation")),
        "stale_generation",
        "This operation requires the current registered service connection.",
    )
}
fn principal(context: &IvyJson) -> IvyResult<AuthenticatedContext> {
    Ok(AuthenticatedContext {
        principal_id: string(context, "principalId")?.clone(),
        credential_digest: string(context, "credentialDigest")?.clone(),
    })
}
fn location(params: &IvyJson) -> IvyResult<objects::Location<'_>> {
    if let Some(id) = params.get("objectId").and_then(IvyJson::as_js_string) {
        Ok(objects::Location::Id(id))
    } else {
        Ok(objects::Location::Path(string(params, "path")?))
    }
}
impl Kernel {
    /// Compose the execution boundary; this does not open storage or start IPC.
    pub fn compose(
        parts: Parts,
        options: Options,
        runtime: Rc<dyn Runtime>,
        clock: Rc<dyn Fn() -> f64>,
    ) -> IvyResult<Self> {
        let mut kernel = Self {
            parts,
            options,
            runtime,
            clock,
            handlers: HashMap::new(),
            connection_generation: 0.0,
        };
        for method in [
            "system.status",
            "system.inspectStorage",
            "objects.writeReceipt",
            "contracts.register",
            "contracts.get",
            "contracts.validate",
            "objects.stat",
            "objects.read",
            "objects.write",
            "objects.pruneRevisions",
            "objects.move",
            "objects.reorder",
            "objects.archive",
            "objects.delete",
            "objects.list",
            "objects.history",
            "objects.tree",
            "objects.search",
            "objects.query",
            "discovery.list",
            "discovery.instructions",
            "system.instructions",
            "system.toolSchema",
            "discovery.describe",
            "service.connect",
            "registry.sync",
            "service.heartbeat",
            "hosts.report",
            "events.head",
            "events.read",
            "hostConfigurations.list",
            "hostConfigurations.get",
            "hostConfigurations.put",
            "hostConfigurations.edit",
            "hostConfigurations.save",
            "hostConfigurations.history",
            "inventory.sync",
            "inventory.get",
            "inventory.list",
            "events.publish",
            "events.subscribe",
            "events.ack",
            "events.unsubscribe",
            "retention.preview",
            "retention.status",
            "uis.list",
            "uis.get",
            "uis.catalog",
            "uis.releases",
            "uis.inspect",
            "uis.deploy",
            "uis.stageAsset",
            "uis.rollback",
        ] {
            kernel.add(method, domain)?;
        }
        for method in catalogs::METHODS {
            kernel.add(method, catalogs::handle)?;
        }
        for method in wiki::METHODS {
            kernel.add(method, wiki::handle)?;
        }
        Ok(kernel)
    }
    pub fn add(&mut self, method: &str, handler: Handler) -> IvyResult<()> {
        let name = JsString::from(method);
        require(
            operation_definition(method).is_some() && !self.handlers.contains_key(&name),
            "internal_error",
            "Invalid canonical handler registration.",
        )?;
        self.handlers.insert(name, handler);
        Ok(())
    }
    pub fn execute(&mut self, context: &IvyJson, value: &IvyJson) -> IvyResult<IvyJson> {
        Call {
            store: &mut self.parts.store,
            registry: &mut self.parts.registry.borrow_mut(),
            discovery: &mut self.parts.discovery,
            configurations: &self.parts.configurations,
            ui_files: &self.parts.ui_files,
            options: &self.options,
            runtime: self.runtime.as_ref(),
            clock: self.clock.as_ref(),
            connection_generation: &mut self.connection_generation,
            handlers: &self.handlers,
        }
        .execute(context, value)
    }
}
impl Call<'_> {
    fn execute(&mut self, context: &IvyJson, value: &IvyJson) -> IvyResult<IvyJson> {
        if context.is_null() {
            return Err(IvyError::internal());
        }
        if context.get("serviceNodeId").is_some() {
            service(context)?;
        }
        validate_request(value)?;
        // Canonical method names are ASCII. A non-Unicode UTF-16 method is still
        // an admitted string and must reach the unknown-operation check.
        let method = String::from_utf16_lossy(&string(value, "method")?.0);
        let params = field(value, "params")?;
        if let Some(reset) = self
            .options
            .reset_bootstrap_credential_digest
            .as_ref()
            .filter(|v| !v.0.is_empty())
        {
            require(
                is_text(context.get("transport"), "http")
                    && context
                        .get("credentialDigest")
                        .and_then(IvyJson::as_js_string)
                        == Some(reset)
                    && [
                        "uis.inspect",
                        "uis.stageAsset",
                        "contracts.register",
                        "objects.write",
                        "objects.read",
                        "uis.deploy",
                    ]
                    .contains(&method.as_str()),
                "maintenance_active",
                "Hive accepts only the configured ui bootstrap while the runtime reset is held.",
            )?;
        }
        if method == "service.connect" && params.get("hiveProtocol") != Some(&IvyJson::Number(1.0))
        {
            return Err(IvyError::new(
                "unsupported_protocol",
                "This Hive supports protocol major 1.",
            ));
        }
        if let Err(error) = validate_input(&method, params) {
            if method == "registry.sync" && context.get("serviceNodeId").is_some() {
                let registry = &mut *self.registry;
                let mut node = registry.node(
                    self.store,
                    string(context, "serviceNodeId")?,
                    self.runtime,
                    (self.clock)(),
                )?;
                let now = self.runtime.iso_date((self.clock)())?;
                for (key, value) in [
                    ("ready", false.into()),
                    ("synced", false.into()),
                    ("lastFailedSyncAt", IvyJson::String(now.clone())),
                ] {
                    node.insert(key, value);
                }
                registry.save(self.store, &node, true)?;
                registry.diagnostic(
                    self.store,
                    &IvyJson::object([
                        ("code", "registry_invalid".into()),
                        (
                            "resource",
                            IvyJson::object([(
                                "serviceNodeId",
                                field(&node, "serviceNodeId")?.clone(),
                            )]),
                        ),
                        ("severity", "error".into()),
                        ("source", "hive".into()),
                        ("firstObservedAt", IvyJson::String(now.clone())),
                        ("lastObservedAt", IvyJson::String(now)),
                        ("status", "current".into()),
                        (
                            "message",
                            "Registry input failed contract validation.".into(),
                        ),
                    ]),
                    self.runtime,
                    (self.clock)(),
                )?;
            }
            return Err(error);
        }
        if operation_definition(&method).and_then(|v| v["access"].as_str()) == Some("service")
            && method != "service.connect"
        {
            service(context)?;
        }
        if method == "objects.delete" {
            require(
                !is_text(context.get("transport"), "mcp"),
                "forbidden",
                "Permanent deletion is unavailable through MCP.",
            )?;
        }
        if method == "discovery.call" {
            require(
                params.get("expectedCallerPrincipalId").is_none()
                    || params.get("expectedCallerPrincipalId") == context.get("principalId"),
                "caller_changed",
                "Return to the original caller before reconciling or repeating this action.",
            )?;
            let resolved = self.discovery.resolve_call(params)?;
            let mut request = value.clone();
            for (key, value) in resolved.as_object().ok_or_else(IvyError::internal)? {
                request.insert(key.clone(), value.clone());
            }
            return self.execute(context, &request);
        }
        let duration = Duration::from_millis(match method.as_str() {
            "registry.sync" => 25_000,
            "objects.query" | "objects.delete" => 15_000,
            _ => 1000,
        });
        let Call {
            store,
            registry,
            discovery,
            configurations,
            ui_files,
            options,
            runtime,
            clock,
            handlers,
            connection_generation,
        } = self;
        store.with_budget_mut(duration, |store| {
            require(
                method != "tools.call",
                "internal_error",
                "Tool routing is owned by the live connection table.",
            )?;
            let handler = *handlers
                .get(&JsString::from(method.as_str()))
                .ok_or_else(|| IvyError::new("not_found", "Unknown Hive operation."))?;
            let result = handler(
                &mut Call {
                    store,
                    registry,
                    discovery,
                    configurations,
                    ui_files,
                    options,
                    runtime: *runtime,
                    clock: *clock,
                    connection_generation,
                    handlers,
                },
                context,
                params,
                &method,
            )?;
            if options.validate_messages && validate_output(&method, &result).is_err() {
                return Err(IvyError::new(
                    "internal_error",
                    "The completed operation produced an invalid server result.",
                )
                .with_outcome(Outcome::Completed));
            }
            Ok(IvyJson::object([
                ("kind", "result".into()),
                ("value", result),
            ]))
        })
    }
}

fn domain(
    call: &mut Call<'_>,
    context: &IvyJson,
    params: &IvyJson,
    method: &str,
) -> IvyResult<IvyJson> {
    match method {
        "system.status" => Ok(IvyJson::object([
            ("callerPrincipalId", field(context, "principalId")?.clone()),
            ("version", IvyJson::String(call.options.version.clone())),
            ("buildId", IvyJson::String(call.options.build_id.clone())),
            ("hiveProtocol", IvyJson::Number(1.0)),
            ("runtimeEpoch", IvyJson::String(call.store.runtime_epoch()?)),
            ("ready", true.into()),
            (
                "serverTime",
                IvyJson::String(call.runtime.iso_date((call.clock)())?),
            ),
            ("storageFormat", IvyJson::Number(STORAGE_FORMAT as f64)),
            (
                "publicBaseUrl",
                IvyJson::String(call.options.public_base_url.clone()),
            ),
        ])),
        "system.inspectStorage" => storage_read::storage_inspection(
            call.store.connection(),
            field(params, "contracts")?
                .as_array()
                .ok_or_else(IvyError::internal)?,
        ),
        "objects.writeReceipt" => Ok(call
            .store
            .object_write_receipt(
                &principal(context)?,
                string(params, "mutationId")?,
                &string(params, "expectedRequestHash")?.to_utf8()?,
            )?
            .unwrap_or(IvyJson::Null)),
        "contracts.register" => objects::register_agent(
            call.store,
            &principal(context)?,
            params,
            (call.clock)() as u64,
        ),
        "contracts.get" => objects::get_contract(
            call.store,
            string(params, "key")?,
            params.get("version").and_then(IvyJson::as_js_string),
        ),
        "contracts.validate" => {
            let contract = call
                .store
                .contract(string(params, "key")?, string(params, "contractVersion")?)?;
            let decoded = objects::decode(call.store, &contract, field(params, "content")?)?;
            Ok(IvyJson::object([
                ("valid", true.into()),
                ("contentHash", decoded.content_hash.into()),
                ("byteLength", IvyJson::Number(decoded.byte_length as f64)),
            ]))
        }
        "objects.stat" => objects::stat(call.store, location(params)?),
        "objects.read" => {
            let read = objects::read(
                call.store,
                location(params)?,
                params
                    .get("revision")
                    .and_then(IvyJson::as_f64)
                    .map(|v| v as u64),
            )?;
            if truthy(context.get("sessionDigest")) {
                call.configurations.project_read(&read)
            } else {
                Ok(read)
            }
        }
        "objects.write" | "objects.move" | "objects.reorder" | "objects.archive"
        | "objects.delete" => {
            let now = (call.clock)();
            let at = call.runtime.iso_date(now)?;
            let principal = principal(context)?;
            match method {
                "objects.write" => {
                    objects::write(call.store, &principal, params, false, now as u64, &at)
                }
                "objects.move" => {
                    objects::move_object(call.store, &principal, params, now as u64, &at)
                }
                "objects.reorder" => {
                    objects::reorder(call.store, &principal, params, now as u64, &at)
                }
                "objects.archive" => objects::archive(
                    call.store,
                    &principal,
                    context.get("serviceNodeId").and_then(IvyJson::as_js_string),
                    params,
                    now as u64,
                    &at,
                ),
                _ => objects::delete(call.store, &principal, params, now as u64, &at),
            }
        }
        "objects.pruneRevisions" => objects::prune_revisions(
            call.store,
            &principal(context)?,
            context.get("serviceNodeId").and_then(IvyJson::as_js_string),
            params,
            (call.clock)() as u64,
            call.runtime,
        ),
        "objects.list" => objects::list(call.store, params),
        "objects.history" => objects::history(call.store, params),
        "objects.tree" => objects::tree(call.store, params),
        "objects.search" => {
            let mut parameters = params.clone();
            if truthy(context.get("sessionDigest")) {
                parameters.insert("excludeContractKey", configurations::CONTRACT_KEY.into());
            }
            objects::search(call.store, &parameters, &|point| {
                call.runtime.word_character(point)
            })
        }
        "objects.query" => {
            require(
                !truthy(context.get("sessionDigest"))
                    || !Configurations::is_configuration_contract(string(params, "contractKey")?),
                "forbidden",
                "Browser sessions cannot query host configuration fields.",
            )?;
            crate::query::query_objects(call.store, params)
        }
        "discovery.list" => call.discovery.list(
            call.store,
            call.registry,
            params,
            is_text(context.get("transport"), "mcp"),
            call.runtime,
            (call.clock)(),
        ),
        "discovery.instructions" => {
            call.discovery
                .instructions(call.store, call.registry, call.runtime, (call.clock)())
        }
        "system.instructions" => call.discovery.instructions_page(
            call.store,
            call.registry,
            params,
            call.runtime,
            (call.clock)(),
        ),
        "system.toolSchema" => call.discovery.tool_schema(
            call.store,
            call.registry,
            params,
            call.runtime,
            (call.clock)(),
        ),
        "discovery.describe" => call.discovery.describe(
            call.store,
            call.registry,
            params,
            is_text(context.get("transport"), "mcp"),
            call.runtime,
            (call.clock)(),
        ),
        "service.connect" => {
            require(
                is_text(context.get("transport"), "ws") && !truthy(context.get("serviceNodeId")),
                "stale_generation",
                "service.connect must initialize a new WebSocket session.",
            )?;
            let generation = match context.get("generation").filter(|v| !v.is_null()) {
                Some(value) => value.as_f64().ok_or_else(IvyError::internal)?,
                None if *call.connection_generation == 9_007_199_254_740_991.0 => 1.0,
                None => *call.connection_generation + 1.0,
            };
            *call.connection_generation =
                if call.connection_generation.is_nan() || generation.is_nan() {
                    f64::NAN
                } else {
                    call.connection_generation.max(generation)
                };
            call.registry.connect(
                call.store,
                &ConnectionContext {
                    principal: &principal(context)?,
                    session_digest: context.get("sessionDigest").and_then(IvyJson::as_js_string),
                    generation: Some(generation),
                },
                params,
                call.runtime,
                (call.clock)(),
            )
        }
        "registry.sync" | "service.heartbeat" | "hosts.report" => {
            service(context)?;
            let id = string(context, "serviceNodeId")?;
            let generation = context.get("generation").and_then(IvyJson::as_f64);
            match method {
                "registry.sync" => call.registry.sync(
                    call.store,
                    id,
                    generation,
                    params,
                    call.runtime,
                    (call.clock)(),
                ),
                "service.heartbeat" => call.registry.heartbeat(
                    call.store,
                    id,
                    generation,
                    params,
                    call.runtime,
                    (call.clock)(),
                ),
                _ => {
                    call.registry
                        .report_host(call.store, id, params, call.runtime, (call.clock)())
                }
            }
        }
        "events.head" => events::head(call.store),
        "events.read" => events::read(call.store, params),
        "events.publish" => {
            service(context)?;
            let now = (call.clock)();
            events::publish(
                call.store,
                &principal(context)?,
                string(context, "serviceNodeId")?,
                params,
                now as u64,
                &call.runtime.iso_date(now)?,
            )
        }
        "events.subscribe" => {
            service(context)?;
            events::subscribe(call.store, string(context, "serviceNodeId")?, params)
        }
        "events.ack" => {
            service(context)?;
            events::ack(call.store, string(context, "serviceNodeId")?, params)
        }
        "events.unsubscribe" => {
            service(context)?;
            events::unsubscribe(
                call.store,
                &principal(context)?,
                string(context, "serviceNodeId")?,
                params,
                (call.clock)() as u64,
            )
        }
        "inventory.sync" => {
            service(context)?;
            crate::inventory::sync(
                call.store,
                call.registry,
                string(context, "serviceNodeId")?,
                params,
                call.runtime,
                (call.clock)(),
            )
        }
        "inventory.get" => crate::inventory::get(
            call.store,
            call.registry,
            field(params, "resourceRef")?,
            call.runtime,
            (call.clock)(),
        ),
        "inventory.list" => crate::inventory::list(
            call.store,
            call.registry,
            params,
            call.runtime,
            (call.clock)(),
        ),
        "retention.preview" => crate::retention::preview(call.store, call.runtime, (call.clock)()),
        "retention.status" => crate::retention::status(call.store, call.runtime, (call.clock)()),
        method if method.starts_with("uis.") => {
            let owner = crate::uis::Uis::new(call.registry, call.ui_files);
            match method {
                "uis.list" => {
                    let records = owner.list(call.store)?;
                    catalogs::try_page(
                        call,
                        method,
                        params,
                        records,
                        |value| string(field(value, "metadata")?, "uiId").cloned(),
                        None,
                    )
                }
                "uis.get" => owner.get(call.store, string(params, "uiId")?),
                "uis.catalog" => owner.catalog(call.store, params),
                "uis.releases" => owner.releases(call.store, params),
                "uis.inspect" => owner.inspect(call.store, params, call.runtime, call.clock),
                "uis.stageAsset" => owner.stage_asset(
                    call.store,
                    &principal(context)?,
                    params,
                    (call.clock)() as u64,
                ),
                "uis.deploy" => owner.deploy(
                    call.store,
                    &principal(context)?,
                    params,
                    (call.clock)() as u64,
                ),
                "uis.rollback" => owner.rollback(
                    call.store,
                    &principal(context)?,
                    params,
                    (call.clock)() as u64,
                    call.runtime,
                    call.clock,
                ),
                _ => Err(IvyError::internal()),
            }
        }
        "hostConfigurations.list" => {
            let mut records = call.configurations.list(call.store, call.runtime)?;
            let mut identity = IvyJson::object([("method", method.into())]);
            for (key, value) in params.as_object().ok_or_else(IvyError::internal)? {
                if key != &JsString::from("cursor") {
                    identity.insert(key.clone(), value.clone());
                }
            }
            crate::paging::catalog_page(
                call.store,
                &mut records,
                &identity,
                |value| {
                    value
                        .get("hostId")
                        .and_then(IvyJson::as_js_string)
                        .cloned()
                        .unwrap_or_default()
                },
                params
                    .get("limit")
                    .and_then(IvyJson::as_f64)
                    .map(|v| v as usize),
                params.get("cursor").and_then(IvyJson::as_js_string),
            )
        }
        "hostConfigurations.get" => {
            require(
                !truthy(context.get("sessionDigest")),
                "forbidden",
                "Browser sessions use the redacted host configuration editor.",
            )?;
            call.configurations.get(
                call.store,
                string(params, "hostId")?,
                params
                    .get("revision")
                    .and_then(IvyJson::as_f64)
                    .map(|v| v as u64),
            )
        }
        "hostConfigurations.put" | "hostConfigurations.save" => {
            if method == "hostConfigurations.put" {
                require(
                    !truthy(context.get("sessionDigest")),
                    "forbidden",
                    "Browser sessions use redacted host configuration saves.",
                )?;
            }
            let now = (call.clock)();
            let at = call.runtime.iso_date(now)?;
            if method == "hostConfigurations.put" {
                call.configurations
                    .put(call.store, &principal(context)?, params, now as u64, &at)
            } else {
                call.configurations
                    .save(call.store, &principal(context)?, params, now as u64, &at)
            }
        }
        "hostConfigurations.edit" => call.configurations.edit(
            call.store,
            string(params, "hostId")?,
            params
                .get("revision")
                .and_then(IvyJson::as_f64)
                .map(|v| v as u64),
        ),
        "hostConfigurations.history" => call.configurations.history(call.store, params),
        _ => Err(IvyError::internal()),
    }
}
