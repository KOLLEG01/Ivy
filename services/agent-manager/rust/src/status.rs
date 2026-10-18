//! Current native-owner status. Reads remain available during startup and shutdown.
use crate::native_journal::{JournalClock, NativeJournal};
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString};
use ivy_sdk::service::ToolHandler;
use std::sync::{Arc, Mutex};

pub struct StatusConfig {
    pub service_node_id: JsString,
    pub host_id: JsString,
    pub settings: IvyJson,
    pub catalog_hash: JsString,
    /// Startup reads this retained evidence once, as in the original service.
    pub process_stops: IvyJson,
    pub windows: bool,
}

/// Startup supplies the current owner and environment managers, not defaults
/// substituted for unavailable runtime observations. Status performs no await.
pub struct StatusState {
    pub owner: IvyJson,
    pub closing: bool,
    pub connected: bool,
    pub ready: bool,
    pub inventory_code: Option<JsString>,
    pub capabilities: IvyJson,
    pub instructions: Option<IvyJson>,
    pub mcp: Option<IvyJson>,
    pub skills: Option<IvyJson>,
}
pub type StatusSource = dyn Fn() -> IvyResult<StatusState> + Send + Sync;

pub struct AgentStatus {
    config: StatusConfig,
    journal: Arc<Mutex<NativeJournal>>,
    source: Arc<StatusSource>,
    clock: Arc<JournalClock>,
}
fn truthy(value: &IvyJson) -> bool {
    match value {
        IvyJson::Null | IvyJson::Bool(false) => false,
        IvyJson::Number(number) => *number != 0.0 && !number.is_nan(),
        IvyJson::String(text) => !text.0.is_empty(),
        _ => true,
    }
}
fn defined(value: Option<&IvyJson>) -> Option<&IvyJson> {
    value.filter(|value| !value.is_null())
}
fn copy(target: &mut IvyJson, key: &str, value: Option<&IvyJson>) {
    if let Some(value) = value {
        target.insert(key, value.clone());
    }
}

impl AgentStatus {
    pub fn new(
        config: StatusConfig,
        journal: Arc<Mutex<NativeJournal>>,
        source: Arc<StatusSource>,
    ) -> Self {
        Self::with_clock(
            config,
            journal,
            source,
            Arc::new(|| {
                chrono::Utc::now()
                    .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
                    .into()
            }),
        )
    }
    pub fn with_clock(
        config: StatusConfig,
        journal: Arc<Mutex<NativeJournal>>,
        source: Arc<StatusSource>,
        clock: Arc<JournalClock>,
    ) -> Self {
        Self {
            config,
            journal,
            source,
            clock,
        }
    }
    pub fn snapshot(&self) -> IvyResult<IvyJson> {
        let state = (self.source)()?;
        let owner = &state.owner;
        if owner.is_null() || self.config.settings.is_null() {
            return Err(IvyError::internal());
        }
        let journal = self.journal.lock().map_err(|_| IvyError::internal())?;
        let mut result = IvyJson::object([
            (
                "serviceNodeId",
                IvyJson::String(self.config.service_node_id.clone()),
            ),
            ("hostId", IvyJson::String(self.config.host_id.clone())),
            (
                "serverType",
                if self
                    .config
                    .settings
                    .get("appServer")
                    .and_then(|server| server.get("mode"))
                    == Some(&"claude-adapter".into())
                {
                    "claude"
                } else {
                    "codex"
                }
                .into(),
            ),
        ]);
        copy(
            &mut result,
            "nativeVersion",
            self.config.settings.get("nativeVersion"),
        );
        copy(
            &mut result,
            "nativeExecutableHash",
            self.config.settings.get("nativeExecutableHash"),
        );
        result.insert(
            "catalogHash",
            IvyJson::String(self.config.catalog_hash.clone()),
        );
        result.insert(
            "epoch",
            journal
                .epoch()
                .cloned()
                .map(IvyJson::String)
                .unwrap_or(IvyJson::Null),
        );
        result.insert("processStops", self.config.process_stops.clone());
        if self.config.windows {
            result.insert("pid", IvyJson::Null);
        } else {
            copy(&mut result, "pid", owner.get("launcherPid"));
        }
        result.insert(
            "state",
            if state.closing {
                "stopping"
            } else if !state.connected {
                "failed"
            } else if state.ready {
                "ready"
            } else {
                "initializing"
            }
            .into(),
        );
        result.insert("observedAt", IvyJson::String((self.clock)()));
        result.insert(
            "code",
            state
                .inventory_code
                .map(IvyJson::String)
                .unwrap_or(IvyJson::Null),
        );
        copy(&mut result, "initialized", owner.get("initialized"));
        result.insert(
            "pendingInputs",
            IvyJson::Number(journal.pending_input_count() as f64),
        );
        result.insert("operations", journal.status());
        result.insert("observedMethods", journal.observed_methods());
        drop(journal);
        result.insert(
            "windowsShell",
            defined(owner.get("windowsShell"))
                .cloned()
                .unwrap_or(IvyJson::Null),
        );
        result.insert("capabilities", state.capabilities);
        if let Some(instructions) = state.instructions {
            result.insert("instructions", instructions);
        }
        if let (Some(mcp), Some(skills)) = (state.mcp, state.skills) {
            result.insert(
                "environment",
                IvyJson::object([("mcp", mcp), ("skills", skills)]),
            );
        }
        if owner.get("nativeHome").is_some_and(truthy) {
            let external = owner.get("connectionMode") == Some(&"external-proxy".into());
            let mut connection = IvyJson::object([
                (
                    "mode",
                    defined(owner.get("connectionMode"))
                        .cloned()
                        .unwrap_or("owned-stdio".into()),
                ),
                ("ownsServer", (!external).into()),
                (
                    "ownsHome",
                    defined(owner.get("ownsNativeHome"))
                        .cloned()
                        .unwrap_or(false.into()),
                ),
                ("actualHome", owner.get("nativeHome").unwrap().clone()),
            ]);
            copy(
                &mut connection,
                "actualVersion",
                defined(owner.get("actualServerVersion"))
                    .or(self.config.settings.get("nativeVersion")),
            );
            connection.insert(
                "socketPath",
                defined(owner.get("socketPath"))
                    .cloned()
                    .unwrap_or(IvyJson::Null),
            );
            let daemon = owner.get("daemon");
            connection.insert(
                "serverPid",
                defined(daemon.and_then(|daemon| daemon.get("pid")))
                    .cloned()
                    .unwrap_or(IvyJson::Null),
            );
            connection.insert(
                "lifecycle",
                defined(daemon.and_then(|daemon| daemon.get("status")))
                    .cloned()
                    .unwrap_or(
                        if external {
                            "explicit-endpoint"
                        } else {
                            "owned-stdio"
                        }
                        .into(),
                    ),
            );
            connection.insert(
                "mcpIdentity",
                if external {
                    "shared-user-home"
                } else {
                    "instance-environment"
                }
                .into(),
            );
            result.insert("connection", connection);
        }
        Ok(result)
    }
    pub fn handler(self: &Arc<Self>) -> ToolHandler {
        let owner = self.clone();
        Arc::new(move |_, _| {
            let owner = owner.clone();
            Box::pin(async move { owner.snapshot() })
        })
    }
}
