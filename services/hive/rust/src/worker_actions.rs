//! Worker actions over initialized native domains and their single connection.
//! Startup, browser push, retention failure journaling, backup and retirement
//! remain separate; this module does not substitute policies for those actions.
use crate::{
    kernel::Kernel,
    mcp_request,
    package_kernel::{self, KernelCatalog},
    store::{AuthContext, AuthenticatedContext, Credential},
    uis::Uis,
    worker_identity,
    worker_protocol::Notification,
};
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString};

fn field<'a>(value: &'a IvyJson, key: &str) -> IvyResult<&'a IvyJson> {
    value.get(key).ok_or_else(IvyError::internal)
}
fn string<'a>(value: &'a IvyJson, key: &str) -> IvyResult<&'a JsString> {
    field(value, key)?
        .as_js_string()
        .ok_or_else(IvyError::internal)
}
fn text(value: &JsString) -> String {
    String::from_utf16_lossy(&value.0)
}
fn strings(value: &IvyJson) -> IvyResult<Vec<JsString>> {
    let IvyJson::Array(values) = value else {
        return Err(IvyError::internal());
    };
    values
        .iter()
        .map(|v| v.as_js_string().cloned().ok_or_else(IvyError::internal))
        .collect()
}
fn head(kernel: &Kernel) -> IvyResult<f64> {
    kernel
        .parts
        .store
        .connection()
        .query_row("SELECT MAX(sequence) AS sequence FROM events", [], |row| {
            row.get::<_, Option<f64>>(0)
        })
        .map(|v| v.unwrap_or(0.0))
        .map_err(|e| kernel.parts.store.query_error(e))
}

pub struct WorkerActions {
    catalog: KernelCatalog,
    configured_principals: Vec<JsString>,
    announced_event_sequence: f64,
}
impl WorkerActions {
    /// The original configure action updates storage, without replacing the
    /// constructor option used to select the package catalog's first principal.
    pub fn new(kernel: &mut Kernel, credentials: &[Credential]) -> IvyResult<Self> {
        let catalog = package_kernel::catalog(
            &mut kernel.parts.store,
            kernel.parts.registry.clone(),
            kernel.options.build_id.clone(),
            kernel.runtime.clone(),
            kernel.clock.clone(),
        )?;
        Ok(Self {
            catalog,
            configured_principals: credentials.iter().map(|v| v.principal_id.clone()).collect(),
            announced_event_sequence: head(kernel)?,
        })
    }
    /// None leaves an action with its other owner. It never fabricates a result
    /// or supplies a replacement policy for an unfinished action.
    pub fn handle(&self, kernel: &mut Kernel, message: &IvyJson) -> Option<IvyResult<IvyJson>> {
        let action = message.get("action")?.as_str().ok()?;
        if action.starts_with("mcp.") {
            return mcp_request::worker_action(
                &kernel.parts.store,
                &mut kernel.parts.registry.borrow_mut(),
                &mut kernel.parts.discovery,
                message,
                kernel.runtime.as_ref(),
                (kernel.clock)(),
            );
        }
        match action.as_str() {
            "execute" => Some((|| {
                let context = field(message, "context")?;
                worker_identity::check_access_expiry(context, (kernel.clock)() as u64)?;
                kernel.execute(context, field(message, "request")?)
            })()),
            "authenticate" => Some((|| {
                let context = field(message, "context")?;
                let principal = kernel.parts.store.authenticate(&AuthContext {
                    credential_digest: string(context, "credentialDigest")?.clone(),
                    session_digest: context
                        .get("sessionDigest")
                        .and_then(IvyJson::as_js_string)
                        .cloned(),
                })?;
                Ok(IvyJson::object([
                    ("principalId", IvyJson::String(principal.principal_id)),
                    (
                        "credentialDigest",
                        IvyJson::String(principal.credential_digest),
                    ),
                ]))
            })()),
            "credentials.configure" => Some((|| {
                let IvyJson::Array(values) = field(message, "credentials")? else {
                    return Err(IvyError::internal());
                };
                let credentials = values
                    .iter()
                    .map(|v| {
                        Ok(Credential {
                            principal_id: string(v, "principalId")?.clone(),
                            digest: string(v, "digest")?.clone(),
                        })
                    })
                    .collect::<IvyResult<Vec<_>>>()?;
                worker_identity::configure_credentials(
                    &mut kernel.parts.store,
                    &credentials,
                    (kernel.clock)() as u64,
                )
            })()),
            "oauth.initialize" => Some(worker_identity::oauth_secrets(&kernel.parts.store)),
            "oauth.family.create" => Some((|| {
                worker_identity::create_oauth_family(
                    &mut kernel.parts.store,
                    field(message, "state")?,
                    &strings(field(message, "activeCredentialReferences")?)?,
                    (kernel.clock)() as u64,
                )
            })()),
            "oauth.family.rotate" => Some((|| {
                worker_identity::rotate_oauth_family(
                    &mut kernel.parts.store,
                    field(message, "rotation")?,
                    (kernel.clock)() as u64,
                )
            })()),
            "session.resolve" => Some((|| {
                worker_identity::resolve_session(
                    &kernel.parts.store,
                    string(message, "sessionDigest")?,
                )
            })()),
            "session.create" => Some((|| {
                worker_identity::create_session_with_date(
                    &kernel.parts.store,
                    string(message, "credentialDigest")?,
                    || kernel.runtime.iso_date((kernel.clock)()),
                )
            })()),
            "session.end" => Some((|| {
                let context = field(message, "context")?;
                worker_identity::end_session(
                    &kernel.parts.store,
                    &AuthenticatedContext {
                        principal_id: context
                            .get("principalId")
                            .and_then(IvyJson::as_js_string)
                            .cloned()
                            .unwrap_or_default(),
                        credential_digest: string(context, "credentialDigest")?.clone(),
                    },
                    context.get("sessionDigest").and_then(IvyJson::as_js_string),
                )
            })()),
            "disconnect" => Some((|| {
                kernel.parts.registry.borrow_mut().disconnect(
                    &mut kernel.parts.store,
                    string(message, "serviceNodeId")?,
                    kernel.runtime.as_ref(),
                    (kernel.clock)(),
                )?;
                Ok(IvyJson::object([("disconnected", true.into())]))
            })()),
            "asset" | "ui.currentAsset" | "ui.resolveSlug" | "ui.entry" => Some((|| {
                let registry = kernel.parts.registry.borrow();
                let uis = Uis::new(&registry, &kernel.parts.ui_files);
                match action.as_str() {
                    "asset" => uis.asset(
                        &kernel.parts.store,
                        string(message, "uiId")?,
                        string(message, "releaseId")?,
                        string(message, "path")?,
                    ),
                    "ui.currentAsset" => uis.current_asset(
                        &kernel.parts.store,
                        string(message, "uiId")?,
                        message.get("path").and_then(IvyJson::as_js_string),
                        kernel.runtime.as_ref(),
                        kernel.clock.as_ref(),
                    ),
                    "ui.resolveSlug" => uis
                        .resolve_slug(&kernel.parts.store, string(message, "slug")?)
                        .map(|v| v.map(IvyJson::String).unwrap_or(IvyJson::Null)),
                    _ => uis.entry(
                        &kernel.parts.store,
                        string(message, "uiId")?,
                        kernel.runtime.as_ref(),
                        kernel.clock.as_ref(),
                    ),
                }
            })()),
            "ui.pointers" => Some((|| {
                let store = &kernel.parts.store;
                let mut query = store.connection().prepare_cached(if message.get("uiId").is_none() {
                    "SELECT app_id,current_release_id,previous_release_id FROM apps ORDER BY app_id"
                } else { "SELECT app_id,current_release_id,previous_release_id FROM apps WHERE app_id=?" }).map_err(|e| store.query_error(e))?;
                let values = message
                    .get("uiId")
                    .map(|_| string(message, "uiId").map(text))
                    .transpose()?;
                let rows = query
                    .query_map(
                        rusqlite::params_from_iter(values.iter().map(String::as_str)),
                        |row| {
                            Ok(IvyJson::object([
                                ("app_id", row.get::<_, String>(0)?.into()),
                                (
                                    "current_release_id",
                                    row.get::<_, Option<String>>(1)?
                                        .map(IvyJson::from)
                                        .unwrap_or(IvyJson::Null),
                                ),
                                (
                                    "previous_release_id",
                                    row.get::<_, Option<String>>(2)?
                                        .map(IvyJson::from)
                                        .unwrap_or(IvyJson::Null),
                                ),
                            ]))
                        },
                    )
                    .map_err(|e| store.query_error(e))?;
                rows.collect::<rusqlite::Result<Vec<_>>>()
                    .map(IvyJson::Array)
                    .map_err(|e| store.query_error(e))
            })()),
            "package.catalog" | "package.seed" | "package.prune" | "package.publish" => {
                Some((|| {
                    if action == "package.catalog" {
                        return self
                            .catalog
                            .list(
                                &kernel.parts.store,
                                field(message, "after")?,
                                field(message, "includeUis")? == &IvyJson::Bool(true),
                            )
                            .map(|v| v.into_json());
                    }
                    let principal = if action != "package.publish" {
                        package_kernel::configured_principal(&self.configured_principals)?
                    } else {
                        string(message, "publisherPrincipalId")?
                    };
                    let now = (kernel.clock)();
                    let date = kernel.runtime.iso_date(now)?;
                    match action.as_str() {
                        "package.seed" => self
                            .catalog
                            .seed(
                                &mut kernel.parts.store,
                                field(message, "catalog")?,
                                principal,
                                now as u64,
                                &date,
                            )
                            .map(|v| v.into_json()),
                        "package.prune" => self
                            .catalog
                            .prune(&mut kernel.parts.store, principal, now as u64, &date)
                            .map(IvyJson::Array),
                        _ => self.catalog.publish(
                            &mut kernel.parts.store,
                            field(message, "input")?,
                            principal,
                            now as u64,
                            &date,
                        ),
                    }
                })())
            }
            "protocol.diagnostic" | "diagnostic" => Some((|| {
                let value;
                let diagnostic = if action == "diagnostic" {
                    field(message, "value")?
                } else {
                    let now = kernel.runtime.iso_date((kernel.clock)())?;
                    value = IvyJson::object([
                        ("code", field(message, "code")?.clone()),
                        (
                            "resource",
                            IvyJson::object([(
                                "serviceNodeId",
                                field(message, "context")?
                                    .get("serviceNodeId")
                                    .cloned()
                                    .unwrap_or(IvyJson::Null),
                            )]),
                        ),
                        ("source", "hive".into()),
                        ("severity", "warning".into()),
                        ("status", "resolved".into()),
                        ("firstObservedAt", IvyJson::String(now.clone())),
                        ("lastObservedAt", IvyJson::String(now)),
                        ("message", field(message, "message")?.clone()),
                    ]);
                    &value
                };
                kernel.parts.registry.borrow_mut().diagnostic(
                    &mut kernel.parts.store,
                    diagnostic,
                    kernel.runtime.as_ref(),
                    (kernel.clock)(),
                )?;
                Ok(IvyJson::object([("recorded", true.into())]))
            })()),
            "binding.diagnostics" => Some(
                kernel
                    .parts
                    .registry
                    .borrow()
                    .diagnostics(&kernel.parts.store, Some(&"unresolved".into()))
                    .map(|values| {
                        IvyJson::Array(
                            values
                                .into_iter()
                                .filter(|v| {
                                    v.get("source") == Some(&IvyJson::from("hive.tool-bindings"))
                                })
                                .collect(),
                        )
                    }),
            ),
            "health" => Some(
                kernel
                    .parts
                    .store
                    .connection()
                    .query_row("SELECT 1 AS ready", [], |row| row.get::<_, i64>(0))
                    .map(|v| IvyJson::object([("ready", (v == 1).into())]))
                    .map_err(|e| kernel.parts.store.query_error(e)),
            ),
            _ => None,
        }
    }
    /// Routes precede changes and the event high-water mark, also after errors.
    /// Emission is incremental so later failures do not erase earlier messages.
    pub fn publish(
        &mut self,
        kernel: &mut Kernel,
        payload: &IvyJson,
        mut emit: impl FnMut(Notification),
    ) -> IvyResult<()> {
        let mut registry = kernel.parts.registry.borrow_mut();
        if payload.get("action") != Some(&IvyJson::from("close")) {
            for id in registry.changed_nodes().to_vec() {
                let route = (|| -> IvyResult<IvyJson> {
                    // Hints may outlive a rolled-back node or failed catalog read.
                    let node = registry.node(
                        &kernel.parts.store,
                        &id,
                        kernel.runtime.as_ref(),
                        (kernel.clock)(),
                    )?;
                    let catalog = if payload.get("action") == Some(&IvyJson::from("execute"))
                        && payload.get("request").and_then(|v| v.get("method"))
                            == Some(&IvyJson::from("registry.sync"))
                        && payload
                            .get("context")
                            .and_then(|v| v.get("serviceNodeId"))
                            .and_then(IvyJson::as_js_string)
                            == Some(&id)
                        && node.get("synced") == Some(&IvyJson::Bool(true))
                    {
                        registry.live_catalog(&kernel.parts.store, &id)?
                    } else {
                        None
                    };
                    let mut route = IvyJson::object([("node", node)]);
                    if let Some((_, generation)) = registry
                        .changed_generations()
                        .iter()
                        .find(|(changed, _)| changed == &id)
                    {
                        route.insert("generation", IvyJson::Number(*generation));
                    }
                    if let Some(catalog) = catalog {
                        route.insert("catalog", catalog);
                    }
                    Ok(route)
                })();
                if let Ok(route) = route {
                    emit(Notification::Routing(route));
                }
            }
            registry.take_changed_nodes();
            registry.take_changed_generations();
        }
        let changes = kernel.parts.store.take_changes();
        if !changes.is_empty() {
            emit(Notification::Changes(changes));
        }
        let through = head(kernel)?;
        if through > self.announced_event_sequence {
            self.announced_event_sequence = through;
            emit(Notification::EventsAvailable(IvyJson::object([(
                "throughSequence",
                IvyJson::Number(through),
            )])));
        }
        Ok(())
    }
}
