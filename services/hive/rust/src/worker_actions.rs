//! Worker actions over initialized native domains and their single connection.
//! Startup, backup and retirement
//! remain separate; this module does not substitute policies for those actions.
use crate::{
    browser_push_store::BrowserPushStore,
    kernel::Kernel,
    mcp_request,
    package_kernel::{self, KernelCatalog},
    retention,
    storage::{Notification, Payload},
    store::Credential,
    uis::Uis,
    worker_identity,
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
fn integer(value: &IvyJson, key: &str, maximum: u64) -> IvyResult<u64> {
    field(value, key)?
        .as_f64()
        .filter(|value| value.fract() == 0.0 && *value >= 0.0 && *value <= maximum as f64)
        .map(|value| value as u64)
        .ok_or_else(IvyError::internal)
}
fn head(kernel: &Kernel) -> IvyResult<i64> {
    kernel
        .parts
        .store
        .connection()
        .query_row("SELECT MAX(sequence) AS sequence FROM events", [], |row| {
            row.get::<_, Option<i64>>(0)
        })
        .map(|v| v.unwrap_or(0))
        .map_err(|e| kernel.parts.store.query_error(e))
}

pub struct WorkerActions {
    browser_push: Option<BrowserPushStore>,
    catalog: KernelCatalog,
    configured_principals: Vec<JsString>,
    announced_event_sequence: i64,
}
impl WorkerActions {
    /// The original configure action updates storage, without replacing the
    /// constructor option used to select the package catalog's first principal.
    pub fn new(kernel: &mut Kernel, credentials: &[Credential]) -> IvyResult<Self> {
        let mut actions = Self::before_ready(kernel, credentials)?;
        actions.initialize_announcements(kernel)?;
        Ok(actions)
    }
    pub(crate) fn before_ready(kernel: &mut Kernel, credentials: &[Credential]) -> IvyResult<Self> {
        let catalog = match kernel.catalog.take() {
            Some(catalog) => catalog,
            None => package_kernel::catalog(
                &mut kernel.parts.store,
                kernel.options.build_id.clone(),
            )?,
        };
        Ok(Self {
            browser_push: None,
            catalog,
            configured_principals: credentials.iter().map(|v| v.principal_id.clone()).collect(),
            announced_event_sequence: 0,
        })
    }
    pub(crate) fn initialize_announcements(&mut self, kernel: &Kernel) -> IvyResult<()> {
        self.announced_event_sequence = head(kernel)?;
        Ok(())
    }
    /// Bind the initialized store domain and its explicit cryptographic provider.
    /// An absent binding leaves these actions unhandled, with no fallback policy.
    pub fn bind_browser_push(&mut self, push: BrowserPushStore) {
        self.browser_push = Some(push);
    }
    pub fn handle(&self, kernel: &mut Kernel, payload: &Payload) -> IvyResult<IvyJson> {
        match payload {
            Payload::Execute { context, request } => {
                worker_identity::check_access_expiry(context, (kernel.clock)() as u64)?;
                kernel.execute(context, request)
            }
            Payload::ConfigureCredentials(credentials) => worker_identity::configure_credentials(
                &mut kernel.parts.store, credentials, (kernel.clock)() as u64,
            ),
            Payload::Authenticate(context) => {
                let principal = kernel.parts.store.authenticate(context)?;
                Ok(IvyJson::object([
                    ("principalId", principal.principal_id.into()),
                    ("credentialDigest", principal.credential_digest.into()),
                ]))
            }
            Payload::CreateSession(credential) => worker_identity::create_session_with_date(
                &kernel.parts.store, credential, || kernel.runtime.iso_date((kernel.clock)()),
            ),
            Payload::ResolveSession(session) => worker_identity::resolve_session(&kernel.parts.store, session),
            Payload::EndSession(context) => worker_identity::end_session(&kernel.parts.store, context),
            Payload::Json(message) => self.handle_json(kernel, message).unwrap_or_else(|| Err(IvyError::internal())),
        }
    }
    /// None leaves an action with its other owner. It never fabricates a result
    /// or supplies a replacement policy for an unfinished action.
    fn handle_json(&self, kernel: &mut Kernel, message: &IvyJson) -> Option<IvyResult<IvyJson>> {
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
            "browserPush.request"
            | "browserPush.keys"
            | "browserPush.next"
            | "browserPush.complete" => {
                let push = self.browser_push.as_ref()?;
                Some((|| match action.as_str() {
                    "browserPush.request" => push.request(
                        &mut kernel.parts.store,
                        field(message, "context")?,
                        field(message, "request")?,
                    ),
                    "browserPush.keys" => push.keys(&kernel.parts.store),
                    "browserPush.next" => push.next(&mut kernel.parts.store),
                    _ => {
                        push.complete(
                            &kernel.parts.store,
                            integer(message, "id", 9_007_199_254_740_991)? as i64,
                            integer(message, "status", u16::MAX as u64)? as u16,
                        )?;
                        Ok(IvyJson::Null)
                    }
                })())
            }
            "execute" => Some((|| {
                let context = field(message, "context")?;
                worker_identity::check_access_expiry(context, (kernel.clock)() as u64)?;
                kernel.execute(context, field(message, "request")?)
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
                    ),
                    "ui.resolveSlug" => uis
                        .resolve_slug(&kernel.parts.store, string(message, "slug")?)
                        .map(|v| v.map(IvyJson::String).unwrap_or(IvyJson::Null)),
                    _ => uis.entry(&kernel.parts.store, string(message, "uiId")?),
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
            "retention.collect" => Some((|| {
                kernel.parts.store.invalidate("system/retention".into());
                let runtime = kernel.runtime.as_ref();
                let clock = kernel.clock.as_ref();
                let registry = &kernel.parts.registry;
                let files = &kernel.parts.ui_files;
                let (result, cause) = kernel.parts.store.capture_failure(|store| {
                    let mut diagnostics = |store: &mut crate::store::Store, now| {
                        registry
                            .borrow_mut()
                            .collect_diagnostics(store, runtime, now)
                    };
                    let result =
                        retention::collect(store, runtime, clock(), Some(&mut diagnostics))?;
                    Uis::new(&registry.borrow(), files).collect_files(store, clock)?;
                    store.transaction(|store| {
                        worker_identity::collect_oauth_families(store, clock() as u64, None)
                            .map(|_| ())
                    })?;
                    store.reclaim_storage_pages()?;
                    Ok(result)
                });
                if let Err(error) = &result {
                    retention::record_failure(
                        &kernel.parts.store,
                        runtime,
                        &cause.unwrap_or_else(|| error.message_js()),
                        clock(),
                    )?;
                }
                result
            })()),
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
        payload: &Payload,
        mut emit: impl FnMut(Notification),
    ) -> IvyResult<()> {
        let mut registry = kernel.parts.registry.borrow_mut();
        let execution = match payload {
            Payload::Execute { context, request } => Some((context, request)),
            Payload::Json(value) if value.get("action") == Some(&IvyJson::from("execute")) =>
                value.get("context").zip(value.get("request")),
            _ => None,
        };
        if !matches!(payload, Payload::Json(value) if value.get("action") == Some(&IvyJson::from("close"))) {
            for id in registry.changed_nodes().to_vec() {
                let route = (|| -> IvyResult<crate::storage::RoutingUpdate> {
                    // Hints may outlive a rolled-back node or failed catalog read.
                    let node = registry.node(
                        &kernel.parts.store,
                        &id,
                        kernel.runtime.as_ref(),
                        (kernel.clock)(),
                    )?;
                    let catalog = if execution.is_some_and(|(context, request)|
                        request.get("method") == Some(&IvyJson::from("registry.sync"))
                        && context.get("serviceNodeId").and_then(IvyJson::as_js_string) == Some(&id))
                        && node.get("synced") == Some(&IvyJson::Bool(true))
                    {
                        registry.live_catalog(&kernel.parts.store, &id)?
                    } else {
                        None
                    };
                    let generation = registry
                        .changed_generations()
                        .iter()
                        .find(|(changed, _)| changed == &id)
                        .map(|(_, generation)| *generation);
                    Ok(crate::storage::RoutingUpdate { node, generation, catalog })
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
            let value = crate::events::sequence_value(through)?;
            self.announced_event_sequence = through;
            emit(Notification::EventsAvailable(IvyJson::object([(
                "throughSequence",
                value,
            )])));
        }
        Ok(())
    }
}
