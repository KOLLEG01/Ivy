//! Native adapter state. All persistence and event ordering happen on one
//! owner; asynchronous provider calls leave as effects and return by call ID.
use super::{
    config,
    error::{AdapterError, Result},
    models::runtime_model_list,
    policy,
    projects::ProjectStore,
    rate_limits::RateLimitStore,
    store::{PINNED_SECTION_ID, SessionStore, ThreadList, ThreadSort},
    value::*,
};
use ivy_contracts::{IvyJson as J, JsString};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

pub type FileReader = Arc<dyn Fn(&JsString) -> Result<JsString> + Send + Sync>;
pub type Worktree = Arc<dyn Fn(&JsString, &JsString) -> Result<JsString> + Send + Sync>;
pub struct AdapterClock {
    pub seconds: super::store::Clock,
    pub millis: super::store::Clock,
}
/// Explicit executing-host facts and admitted configuration. Constructors do
/// not discover another account, executable or provider as a fallback.
pub struct Environment {
    pub environment: J,
    pub discovered_router_models: Vec<JsString>,
    pub cwd: JsString,
    pub codex_home: JsString,
    pub platform_os: JsString,
    pub cpu: JsString,
    pub compat_version: JsString,
    pub version_suffix: JsString,
    pub default_model: J,
    pub default_effort: J,
    pub mcp_servers: J,
    pub startup_statuses: Vec<J>,
    pub config_overrides: J,
    /// Explicit admitted adapter config file; never discover another home.
    pub config_path: std::path::PathBuf,
    pub prepare_worktree: Worktree,
    pub read_base64: FileReader,
    pub subagent_completed: Option<bool>,
    pub subagent_watchdog_ms: Option<u64>,
}
#[derive(Debug, Clone)]
pub enum Effect {
    Send {
        peer: JsString,
        message: J,
    },
    /// Must run after preceding sends (the original setImmediate boundary).
    Begin {
        turn_id: JsString,
    },
    Run {
        turn_id: JsString,
        context: J,
    },
    Runtime {
        call_id: u64,
        method: &'static str,
        params: J,
    },
    Permission {
        callback: J,
        result: J,
    },
    Diff {
        turn_id: JsString,
        cwd: JsString,
    },
    Watchdog {
        turn_id: JsString,
        timeout_ms: u64,
    },
    CancelWatchdog {
        turn_id: JsString,
    },
}
struct Call {
    peer: JsString,
    id: J,
    kind: CallKind,
}
pub(super) enum CallKind {
    Models,
    Usage,
    Steer(J),
    Interrupt,
}
struct Reverse {
    request_id: JsString,
    peer: JsString,
    thread: JsString,
    turn: JsString,
    item: JsString,
    callback: J,
    event: J,
    questions: Option<Vec<J>>,
    started: f64,
}
pub(super) struct TurnState {
    pub(super) peer: JsString,
    pub(super) thread: J,
    pub(super) params: J,
    pub(super) prompt: JsString,
    pub(super) images: Vec<J>,
    pub(super) plan_mode: bool,
    pub(super) agent: Option<JsString>,
    pub(super) reasoning: Option<JsString>,
    pub(super) plan: Option<JsString>,
    pub(super) item_ids: BTreeMap<JsString, JsString>,
    pub(super) item_started: BTreeMap<JsString, f64>,
    pub(super) output_seen: BTreeSet<JsString>,
    pub(super) notices: BTreeSet<JsString>,
    pub(super) has_text: bool,
    pub(super) subagents: Vec<(JsString, super::subagents::Child)>,
    pub(super) workflow_tools: BTreeSet<JsString>,
}
pub struct Adapter {
    pub(super) store: SessionStore,
    pub(super) projects: ProjectStore,
    pub(super) limits: RateLimitStore,
    pub(super) env: Environment,
    pub(super) clock: super::store::Clock,
    pub(super) millis: super::store::Clock,
    pub(super) ids: IdSource,
    pub(super) peers: Vec<(JsString, JsString)>,
    pub(super) features: BTreeMap<JsString, bool>,
    pub(super) active: BTreeMap<JsString, JsString>,
    pub(super) turns: BTreeMap<JsString, TurnState>,
    calls: BTreeMap<u64, Call>,
    next_call: u64,
    usage_waiters: Vec<(JsString, J)>,
    reverse: Vec<Reverse>,
    pub(super) command_allow: BTreeMap<JsString, BTreeSet<JsString>>,
    pub(super) tokens: BTreeMap<JsString, J>,
    pub(super) stopped: bool,
}
impl Adapter {
    pub fn new(
        store: SessionStore,
        projects: ProjectStore,
        limits: RateLimitStore,
        mut env: Environment,
        clock: AdapterClock,
        ids: IdSource,
    ) -> Self {
        config::load(&mut env);
        Self {
            store,
            projects,
            limits,
            env,
            clock: clock.seconds,
            millis: clock.millis,
            ids,
            peers: vec![],
            features: BTreeMap::new(),
            active: BTreeMap::new(),
            turns: BTreeMap::new(),
            calls: BTreeMap::new(),
            next_call: 1,
            usage_waiters: vec![],
            reverse: vec![],
            command_allow: BTreeMap::new(),
            tokens: BTreeMap::new(),
            stopped: false,
        }
    }
    pub fn has_active_turns(&self) -> bool {
        !self.active.is_empty()
    }
    pub fn close_peer(&mut self, peer: &JsString) {
        self.peers.retain(|(_, p)| p != peer);
    }
    pub(super) fn bind(&mut self, thread: JsString, peer: JsString) {
        if let Some((_, p)) = self.peers.iter_mut().find(|(t, _)| t == &thread) {
            *p = peer
        } else {
            self.peers.push((thread, peer))
        }
    }
    pub(super) fn notify(&self, peer: &JsString, method: &str, params: J, out: &mut Vec<Effect>) {
        if self.stopped {
            return;
        }
        let thread = field(&params, "threadId")
            .as_js_string()
            .or_else(|| field(field(&params, "thread"), "id").as_js_string());
        let target = thread
            .and_then(|id| self.peers.iter().find(|(t, _)| t == id).map(|(_, p)| p))
            .unwrap_or(peer);
        out.push(Effect::Send {
            peer: target.clone(),
            message: J::object([
                ("jsonrpc", "2.0".into()),
                ("method", method.into()),
                ("params", params),
            ]),
        });
    }
    fn response(peer: &JsString, id: &J, result: Result<J>, out: &mut Vec<Effect>) {
        let mut message = J::object([("jsonrpc", "2.0".into()), ("id", id.clone())]);
        match result {
            Ok(result) => message.insert("result", result),
            Err(error) => message.insert("error", error.to_wire()),
        }
        out.push(Effect::Send {
            peer: peer.clone(),
            message,
        });
    }
    pub fn receive(&mut self, peer: &JsString, message: &J) -> Result<Vec<Effect>> {
        let mut out = vec![];
        if self.stopped {
            return Ok(out);
        }
        if truthy(field(message, "method")) {
            if message.get("id").is_none() {
                return Ok(out);
            }
            let id = field(message, "id");
            let result = self.dispatch(
                peer,
                id,
                field(message, "method"),
                &coalesce(field(message, "params"), &object()),
                &mut out,
            );
            let deferred = if matches!(out.last(), Some(Effect::Begin { .. })) {
                out.pop()
            } else {
                None
            };
            match result {
                Ok(Some(result)) => Self::response(peer, id, Ok(result), &mut out),
                Ok(None) => {}
                Err(error) => Self::response(peer, id, Err(error), &mut out),
            }
            if let Some(effect) = deferred {
                out.push(effect)
            }
        } else if message.get("id").is_some() {
            self.resolve_reverse(message, &mut out)?;
        }
        Ok(out)
    }
    pub(super) fn call(
        &mut self,
        peer: &JsString,
        id: &J,
        kind: CallKind,
        method: &'static str,
        params: J,
        out: &mut Vec<Effect>,
    ) {
        let call_id = self.next_call;
        self.next_call += 1;
        self.calls.insert(
            call_id,
            Call {
                peer: peer.clone(),
                id: id.clone(),
                kind,
            },
        );
        out.push(Effect::Runtime {
            call_id,
            method,
            params,
        });
    }
    pub fn runtime_reply(&mut self, call_id: u64, result: Result<J>) -> Result<Vec<Effect>> {
        let mut out = vec![];
        let Some(call) = self.calls.remove(&call_id) else {
            return Ok(out);
        };
        if self.stopped {
            return Ok(out);
        }
        let result = match call.kind {
            CallKind::Models => result.and_then(|models| {
                runtime_model_list(
                    models.as_array().unwrap_or(&[]),
                    &string(&self.env.default_model),
                    &string(&self.env.default_effort),
                )
            }),
            CallKind::Usage => {
                let limits = result.ok().unwrap_or(J::Null);
                self.limits.record_usage(&limits, (self.millis)());
                let snapshot = self.limits.read((self.clock)());
                Self::response(&call.peer, &call.id, snapshot.clone(), &mut out);
                for (peer, id) in std::mem::take(&mut self.usage_waiters) {
                    Self::response(&peer, &id, snapshot.clone(), &mut out)
                }
                return Ok(out);
            }
            CallKind::Steer(turn) => result.map(|_| J::object([("turnId", turn)])),
            CallKind::Interrupt => result.map(|_| object()),
        };
        Self::response(&call.peer, &call.id, result, &mut out);
        Ok(out)
    }
    fn dispatch(
        &mut self,
        peer: &JsString,
        id: &J,
        method: &J,
        p: &J,
        out: &mut Vec<Effect>,
    ) -> Result<Option<J>> {
        let method = method.as_str().unwrap_or_default();
        let result = match method.as_str() {
            "initialize" => {
                let cap = field(p, "capabilities");
                self.features.insert(
                    peer.clone(),
                    field(cap, "subAgentActivityCompleted").as_bool() == Some(true)
                        || field(cap, "subAgentActivityKinds")
                            .as_array()
                            .is_some_and(|a| a.iter().any(|v| is(v, "completed"))),
                );
                // The original async dispatch yields to its queued microtask
                // before sending initialize's response.
                self.notify(
                    peer,
                    "account/updated",
                    J::object([("authMode", "apikey".into()), ("planType", J::Null)]),
                    out,
                );
                for status in &self.env.startup_statuses {
                    self.notify(peer, "mcpServer/startupStatus/updated", status.clone(), out)
                }
                let client = field(p, "clientInfo");
                let name = string(&str_or(field(client, "name"), &"codex-app".into())).trim();
                let version = string(&str_or(field(client, "version"), &"unknown".into())).trim();
                let name = if name.0.is_empty() {
                    JsString::from("codex-app")
                } else {
                    name
                };
                let version = if version.0.is_empty() {
                    JsString::from("unknown")
                } else {
                    version
                };
                let origin = if self.env.version_suffix.0.is_empty() {
                    JsString::from("unknown")
                } else {
                    self.env.version_suffix.clone()
                };
                let agent = concat(&[
                    &name,
                    &"/".into(),
                    &self.env.compat_version,
                    &" (".into(),
                    &self.env.platform_os,
                    &"; ".into(),
                    &self.env.cpu,
                    &") ".into(),
                    &origin,
                    &" (".into(),
                    &name,
                    &"; ".into(),
                    &version,
                    &")".into(),
                ]);
                J::object([
                    ("userAgent", agent.into_json()),
                    ("codexHome", self.env.codex_home.clone().into_json()),
                    (
                        "platformFamily",
                        if self.env.platform_os == JsString::from("windows") {
                            "windows".into()
                        } else {
                            "unix".into()
                        },
                    ),
                    ("platformOs", self.env.platform_os.clone().into_json()),
                ])
            }
            "project/list" => self
                .projects
                .list(p, &|id| Ok(self.store.get_thread(id)?))?,
            "project/read" => J::object([(
                "project",
                self.projects.read(field(p, "projectId"), &|id| {
                    Ok(self.store.get_thread(id)?)
                })?,
            )]),
            "project/create" | "project/update" => {
                let project = if method == "project/create" {
                    self.projects
                        .create(p, &|id| Ok(self.store.get_thread(id)?))?
                } else {
                    self.projects
                        .update(p, &|id| Ok(self.store.get_thread(id)?))?
                };
                self.notify(
                    peer,
                    "project/changed",
                    J::object([
                        ("projectId", field(&project, "id").clone()),
                        (
                            "changeType",
                            if method == "project/create" {
                                "created".into()
                            } else {
                                "updated".into()
                            },
                        ),
                    ]),
                    out,
                );
                J::object([("project", project)])
            }
            "project/delete" => {
                self.projects.delete(field(p, "projectId"), &|id| {
                    Ok(self.store.get_thread(id)?)
                })?;
                self.notify(
                    peer,
                    "project/changed",
                    J::object([
                        ("projectId", field(p, "projectId").clone()),
                        ("changeType", "deleted".into()),
                    ]),
                    out,
                );
                object()
            }
            "thread/start" => self.thread_start(peer, p, out)?,
            "thread/resume" => self.thread_resume(peer, p)?,
            "thread/list" => self.thread_list(p)?,
            "thread/read" => {
                let t = self.thread(field(p, "threadId"))?;
                let turns = if field(p, "includeTurns").as_bool() != Some(false)
                    || is(field(&t, "threadSource"), "subagent")
                {
                    self.store.list_turns(&string(field(&t, "id")))?
                } else {
                    vec![]
                };
                J::object([("thread", self.to_thread(&t, &turns)?)])
            }
            "thread/turns/list" => {
                let view = if is(field(p, "itemsView"), "full") {
                    "full"
                } else if is(field(p, "itemsView"), "notLoaded") {
                    "notLoaded"
                } else {
                    "summary"
                };
                J::object([
                    (
                        "data",
                        J::Array(
                            self.store
                                .list_turns(&string(&str_or(field(p, "threadId"), &"".into())))?
                                .iter()
                                .map(|t| policy::turn_view(t, view))
                                .collect(),
                        ),
                    ),
                    ("nextCursor", J::Null),
                    ("backwardsCursor", J::Null),
                ])
            }
            "thread/items/list" => self.items_list(p)?,
            "thread/loaded/list" => {
                let values = self
                    .peers
                    .iter()
                    .map(|(t, _)| J::String(t.clone()))
                    .collect::<Vec<_>>();
                let limit = field(p, "limit")
                    .as_f64()
                    .filter(|n| n.is_finite())
                    .unwrap_or(if values.is_empty() {
                        50.0
                    } else {
                        values.len() as f64
                    });
                J::object([
                    ("data", J::Array(array_slice(&values, 0.0, limit))),
                    (
                        "nextCursor",
                        if (values.len() as f64) > limit {
                            string(&J::Number(limit)).into_json()
                        } else {
                            J::Null
                        },
                    ),
                ])
            }
            "thread/archive" | "thread/unarchive" => {
                let thread = string(&str_or(field(p, "threadId"), &"".into()));
                let archived = method == "thread/archive";
                self.store.set_archived(&thread, archived)?;
                if let Some((_, peer)) = self.peers.iter().find(|(t, _)| t == &thread) {
                    self.notify(
                        peer,
                        if archived {
                            "thread/archived"
                        } else {
                            "thread/unarchived"
                        },
                        J::object([("threadId", thread.clone().into_json())]),
                        out,
                    )
                }
                if archived {
                    self.command_allow.remove(&thread);
                    self.tokens.remove(&thread);
                    object()
                } else {
                    let t = self.thread(&J::String(thread.clone()))?;
                    J::object([(
                        "thread",
                        self.to_thread(&t, &self.store.list_turns(&thread)?)?,
                    )])
                }
            }
            "turn/start" => self.turn_start(peer, p, out)?,
            "turn/steer" => {
                self.steer(peer, id, p, out)?;
                return Ok(None);
            }
            "turn/interrupt" => {
                self.interrupt(peer, id, p, out)?;
                return Ok(None);
            }
            "model/list" => {
                self.call(peer, id, CallKind::Models, "models", object(), out);
                return Ok(None);
            }
            "account/rateLimits/read" => {
                if self
                    .calls
                    .values()
                    .any(|c| matches!(c.kind, CallKind::Usage))
                {
                    self.usage_waiters.push((peer.clone(), id.clone()));
                    return Ok(None);
                }
                if self.limits.usage_due((self.millis)(), 60_000.0) {
                    self.call(peer, id, CallKind::Usage, "usage", object(), out);
                    return Ok(None);
                }
                self.limits.read((self.clock)())?
            }
            "permissionProfile/list" => policy::permission_profiles(p)?,
            "collaborationMode/list" => J::object([(
                "data",
                J::Array(
                    [("Default", "default"), ("Plan", "plan")]
                        .into_iter()
                        .map(|(name, mode)| {
                            J::object([
                                ("name", name.into()),
                                ("mode", mode.into()),
                                ("model", self.env.default_model.clone()),
                                ("reasoning_effort", self.env.default_effort.clone()),
                            ])
                        })
                        .collect(),
                ),
            )]),
            "config/read" => self.config_read()?,
            "fs/readFile" => J::object([(
                "dataBase64",
                (self.env.read_base64)(&string(&str_or(
                    &coalesce(field(p, "path"), field(p, "filePath")),
                    &"".into(),
                )))?
                .into_json(),
            )]),
            _ => {
                return Err(AdapterError::new(concat(&[
                    &"method not implemented: ".into(),
                    &string(&J::from(method)),
                ])));
            }
        };
        Ok(Some(result))
    }
    pub(super) fn thread(&self, id: &J) -> Result<J> {
        let id = string(&str_or(id, &"".into()));
        self.store
            .get_thread(&id)?
            .ok_or_else(|| AdapterError::new(concat(&[&"unknown thread: ".into(), &id])))
    }
    pub(super) fn cli_version(&self) -> J {
        let suffix = if self.env.version_suffix.0.is_empty() {
            JsString::from("")
        } else {
            concat(&[&" (".into(), &self.env.version_suffix, &")".into()])
        };
        concat(&[&"codex-cli ".into(), &self.env.compat_version, &suffix]).into_json()
    }
    pub(super) fn to_thread(&self, t: &J, turns: &[J]) -> Result<J> {
        let mut v = object();
        for key in [
            "id",
            "sessionId",
            "forkedFromId",
            "preview",
            "ephemeral",
            "modelProvider",
            "createdAt",
            "updatedAt",
            "status",
            "cwd",
            "model",
            "reasoningEffort",
            "name",
        ] {
            v.insert(key, field(t, key).clone())
        }
        let parent = if is(field(t, "threadSource"), "subagent") {
            field(t, "forkedFromId").clone()
        } else {
            J::Null
        };
        let mut source = policy::session_source(field(t, "source"));
        if !parent.is_null() {
            let mut depth = 1.0;
            let mut ancestor = parent.clone();
            let mut seen = BTreeSet::from([string(field(t, "id"))]);
            while let Some(id) = ancestor.as_js_string() {
                if !seen.insert(id.clone()) {
                    break;
                }
                let Some(next) = self.store.get_thread(id)? else {
                    break;
                };
                if !is(field(&next, "threadSource"), "subagent") {
                    break;
                }
                depth += 1.0;
                ancestor = field(&next, "forkedFromId").clone();
            }
            source = J::object([(
                "subAgent",
                J::object([(
                    "thread_spawn",
                    J::object([
                        ("parent_thread_id", parent.clone()),
                        ("depth", depth.into_json()),
                        ("agent_path", J::Null),
                        (
                            "agent_nickname",
                            policy::null_if_empty(field(t, "agentNickname")),
                        ),
                        ("agent_role", policy::null_if_empty(field(t, "agentRole"))),
                    ]),
                )]),
            )]);
        }
        let section = if let Some(id) = field(t, "sectionId").as_js_string() {
            self.store.get_section(id)?.unwrap_or(J::Null)
        } else {
            J::Null
        };
        for (key, value) in [
            (
                "isPinned",
                (is(field(t, "sectionId"), PINNED_SECTION_ID)
                    || field(t, "isPinned").as_bool() == Some(true))
                .into(),
            ),
            ("section", section),
            ("sectionEnteredAt", field(t, "sectionEnteredAt").clone()),
            ("parentThreadId", parent),
            (
                "projectId",
                self.projects
                    .project_id(t, &|id| Ok(self.store.get_thread(id)?))?,
            ),
            ("recencyAt", field(t, "updatedAt").clone()),
            ("path", J::Null),
            ("cliVersion", self.cli_version()),
            ("canAcceptDirectInput", true.into()),
            (
                "collaborationMode",
                J::object([
                    (
                        "mode",
                        coalesce(field(t, "collaborationMode"), &"default".into()),
                    ),
                    (
                        "settings",
                        J::object([
                            ("model", field(t, "model").clone()),
                            ("reasoning_effort", field(t, "reasoningEffort").clone()),
                            (
                                "developer_instructions",
                                field(t, "developerInstructions").clone(),
                            ),
                        ]),
                    ),
                ]),
            ),
            (
                "activePermissionProfile",
                if is(field(t, "sandboxMode"), "danger-full-access") {
                    ":danger-full-access".into()
                } else if is(field(t, "sandboxMode"), "read-only") {
                    ":read-only".into()
                } else {
                    ":workspace".into()
                },
            ),
            ("source", source),
            (
                "threadSource",
                policy::thread_source(field(t, "threadSource")),
            ),
            (
                "agentNickname",
                policy::null_if_empty(field(t, "agentNickname")),
            ),
            ("agentRole", policy::null_if_empty(field(t, "agentRole"))),
            ("gitInfo", J::Null),
            (
                "turns",
                J::Array(turns.iter().map(|t| policy::turn_view(t, "full")).collect()),
            ),
        ] {
            v.insert(key, value)
        }
        Ok(v)
    }
    fn envelope(&self, t: &J, turns: &[J]) -> Result<J> {
        let profile = policy::thread_profile(t);
        Ok(J::object([
            ("thread", self.to_thread(t, turns)?),
            ("model", field(t, "model").clone()),
            ("modelProvider", field(t, "modelProvider").clone()),
            ("serviceTier", J::Null),
            ("cwd", field(t, "cwd").clone()),
            (
                "runtimeWorkspaceRoots",
                J::Array(vec![field(t, "cwd").clone()]),
            ),
            ("instructionSources", J::Array(vec![])),
            (
                "approvalPolicy",
                coalesce(field(t, "approvalPolicy"), &"never".into()),
            ),
            ("approvalsReviewer", "user".into()),
            ("sandbox", policy::sandbox_envelope(t)),
            ("permissionProfile", J::Null),
            (
                "activePermissionProfile",
                if profile.is_null() {
                    J::Null
                } else {
                    J::object([("id", profile.clone()), ("extends", J::Null)])
                },
            ),
            ("permissions", profile),
            ("reasoningEffort", field(t, "reasoningEffort").clone()),
            ("multiAgentMode", "explicitRequestOnly".into()),
        ]))
    }
    fn thread_start(&mut self, peer: &JsString, p: &J, out: &mut Vec<Effect>) -> Result<J> {
        let id = (self.ids)();
        let now = (self.clock)();
        let project = if field(p, "projectId").is_null() {
            None
        } else {
            Some(
                self.projects.read(field(p, "projectId"), &|id| {
                    Ok(self.store.get_thread(id)?)
                })?,
            )
        };
        let root = project
            .as_ref()
            .and_then(|p| field(p, "roots").as_array())
            .and_then(|r| r.first())
            .map(|r| field(r, "path").clone())
            .unwrap_or_else(|| self.env.cwd.clone().into_json());
        let cwd = (self.env.prepare_worktree)(&id, &string(&str_or(field(p, "cwd"), &root)))?;
        let model = policy::model_from_params(p, &self.env.default_model);
        if Self::codex_model(&model) {
            return Err(AdapterError::new(
                "Codex proxy models are disabled for this Claude adapter",
            ));
        }
        let selection =
            config::provider_loop_config(&self.env.environment, &self.env.config_overrides)?;
        let profile = policy::profile_from_params(p);
        let (approval, sandbox) = policy::profile_policy(&profile);
        let mut t = J::object([
            ("id", id.clone().into_json()),
            ("sessionId", id.clone().into_json()),
            ("forkedFromId", J::Null),
            ("isPinned", false.into()),
            ("sectionId", J::Null),
            ("sectionEnteredAt", J::Null),
            ("sectionPosition", J::Null),
            ("preview", "".into()),
            ("name", J::Null),
            ("archived", false.into()),
            ("cwd", cwd.into_json()),
            ("model", model),
            (
                "reasoningEffort",
                policy::effort_from_params(p, &self.env.default_effort),
            ),
            ("modelProvider", "claude-code".into()),
            ("claudeSessionId", J::Null),
            ("source", "appServer".into()),
            ("createdAt", now.into_json()),
            ("updatedAt", now.into_json()),
            ("status", J::object([("type", "idle".into())])),
            (
                "approvalPolicy",
                coalesce(
                    &approval,
                    &coalesce(
                        &policy::approval(field(p, "approvalPolicy")),
                        &"never".into(),
                    ),
                ),
            ),
            (
                "sandboxMode",
                coalesce(
                    &sandbox,
                    &coalesce(
                        &policy::sandbox(field(p, "sandbox")),
                        &"danger-full-access".into(),
                    ),
                ),
            ),
            ("permissionProfileId", profile),
            (
                "ephemeral",
                (field(p, "ephemeral").as_bool() == Some(true)
                    || is(field(p, "threadSource"), "title_generation")
                    || is(field(p, "threadSource"), "memory_consolidation"))
                .into(),
            ),
            (
                "threadSource",
                policy::thread_source(field(p, "threadSource")),
            ),
            ("personality", policy::personality(field(p, "personality"))),
            (
                "runtimeBackend",
                if is(
                    field(field(&selection, "selection"), "runtimeType"),
                    "codex-proxy",
                ) {
                    "codex"
                } else {
                    "claude"
                }
                .into(),
            ),
            ("codexSessionId", J::Null),
        ]);
        for key in [
            "agentRole",
            "agentNickname",
            "baseInstructions",
            "developerInstructions",
        ] {
            t.insert(key, policy::null_if_empty(field(p, key)))
        }
        if let Some(project) = project {
            self.projects.bind(&t, field(&project, "id"), &|id| {
                Ok(self.store.get_thread(id)?)
            })?
        }
        self.store.upsert_thread(&t)?;
        self.bind(id, peer.clone());
        self.notify(
            peer,
            "thread/started",
            J::object([("thread", self.to_thread(&t, &[])?)]),
            out,
        );
        self.envelope(&t, &[])
    }
    pub(super) fn codex_model(model: &J) -> bool {
        let s = string(model);
        s.0.starts_with(&JsString::from("gpt-").0)
            || s.0.starts_with(&JsString::from("gpt_").0)
            || (s.0.first() == Some(&111) && s.0.get(1).is_some_and(|u| (48..=57).contains(u)))
            || s.0.starts_with(&JsString::from("codex-").0)
    }
    fn thread_resume(&mut self, peer: &JsString, p: &J) -> Result<J> {
        let mut t = self.thread(field(p, "threadId"))?;
        if field(p, "cwd")
            .as_js_string()
            .is_some_and(|s| !s.0.is_empty())
        {
            t.insert("cwd", field(p, "cwd").clone())
        }
        let model = policy::model_from_params(p, &J::Null);
        if truthy(&model) && Self::codex_model(&model) == is(field(&t, "runtimeBackend"), "codex") {
            t.insert("model", model)
        }
        let effort = policy::effort_from_params(p, &J::Null);
        if !effort.is_null() {
            t.insert("reasoningEffort", effort)
        }
        policy::apply_permissions(&mut t, p, false);
        for key in [
            "threadSource",
            "baseInstructions",
            "developerInstructions",
            "personality",
        ] {
            if field(p, key).as_js_string().is_some() {
                t.insert(
                    key,
                    match key {
                        "threadSource" => policy::thread_source(field(p, key)),
                        "personality" => policy::personality(field(p, key)),
                        _ => policy::null_if_empty(field(p, key)),
                    },
                )
            }
        }
        self.store.upsert_thread(&t)?;
        let id = string(field(&t, "id"));
        self.bind(id.clone(), peer.clone());
        let mut queue = std::collections::VecDeque::from([id.clone()]);
        let mut seen = BTreeSet::new();
        while let Some(parent) = queue.pop_front() {
            if !seen.insert(parent.clone()) {
                continue;
            }
            for child in self.store.list_threads(&ThreadList {
                include_ephemeral: true,
                parent_thread_id: Some(parent),
                sort_key: Some(ThreadSort::Created),
                ascending: true,
                ..Default::default()
            })? {
                let child = string(field(&child, "id"));
                self.bind(child.clone(), peer.clone());
                queue.push_back(child);
            }
        }
        let turns = if field(p, "excludeTurns").as_bool() == Some(true) {
            vec![]
        } else {
            self.store.list_turns(&id)?
        };
        self.envelope(&t, &turns)
    }
    fn thread_list(&self, p: &J) -> Result<J> {
        let sort = if is(field(p, "sortKey"), "section_position") {
            ThreadSort::SectionPosition
        } else if is(field(p, "sortKey"), "updated_at") {
            ThreadSort::Updated
        } else if is(field(p, "sortKey"), "recency_at") {
            ThreadSort::Recency
        } else {
            ThreadSort::Created
        };
        let limit = field(p, "limit")
            .as_f64()
            .filter(|n| n.is_finite())
            .unwrap_or(50.0);
        let options = ThreadList {
            archived: truthy(field(p, "archived")),
            limit: Some(limit),
            cursor: field(p, "cursor")
                .as_js_string()
                .map(|s| number(&s.clone().into_json())),
            is_pinned: field(p, "isPinned").as_bool(),
            section_id: match p.get("sectionId") {
                Some(J::Null) => Some(None),
                Some(J::String(s)) => Some(Some(s.clone())),
                _ => None,
            },
            cwd: field(p, "cwd")
                .as_js_string()
                .map(|s| vec![s.clone()])
                .unwrap_or_else(|| {
                    field(p, "cwd")
                        .as_array()
                        .unwrap_or(&[])
                        .iter()
                        .map(string)
                        .collect()
                }),
            include_ephemeral: field(p, "includeEphemeral").as_bool() == Some(true),
            parent_thread_id: field(p, "parentThreadId")
                .as_js_string()
                .filter(|s| !s.0.is_empty())
                .cloned(),
            ancestor_thread_id: field(p, "ancestorThreadId")
                .as_js_string()
                .filter(|s| !s.0.is_empty())
                .cloned(),
            source_kinds: field(p, "sourceKinds")
                .as_array()
                .unwrap_or(&[])
                .iter()
                .filter_map(|v| v.as_js_string().cloned())
                .collect(),
            sort_key: Some(sort),
            ascending: is(field(p, "sortDirection"), "asc"),
        };
        let threads = self.store.list_threads(&options)?;
        let cursor = |t: &J| match sort {
            ThreadSort::Created => field(t, "createdAt").clone(),
            ThreadSort::SectionPosition => {
                coalesce(field(t, "sectionPosition"), field(t, "createdAt"))
            }
            _ => field(t, "updatedAt").clone(),
        };
        let mut data = vec![];
        for t in &threads {
            if p.get("projectId").is_none()
                || self
                    .projects
                    .project_id(t, &|id| Ok(self.store.get_thread(id)?))?
                    == *field(p, "projectId")
            {
                data.push(self.to_thread(t, &[])?)
            }
        }
        Ok(J::object([
            ("data", J::Array(data)),
            (
                "nextCursor",
                if threads.len() as f64 >= limit {
                    threads
                        .last()
                        .map(|t| string(&cursor(t)).into_json())
                        .unwrap_or(J::Null)
                } else {
                    J::Null
                },
            ),
            (
                "backwardsCursor",
                threads
                    .first()
                    .map(|t| string(&cursor(t)).into_json())
                    .unwrap_or(J::Null),
            ),
        ]))
    }
    fn items_list(&self, p: &J) -> Result<J> {
        let thread = self.thread(field(p, "threadId"))?;
        let turns = self.store.list_turns(&string(field(&thread, "id")))?;
        let requested = field(p, "turnId").as_js_string();
        if requested.is_some_and(|s| {
            !s.0.is_empty()
                && !turns
                    .iter()
                    .any(|t| field(t, "id").as_js_string() == Some(s))
        }) {
            return Err(AdapterError::new(concat(&[
                &"turn does not belong to thread: ".into(),
                requested.unwrap(),
            ])));
        }
        let mut entries = vec![];
        for turn in &turns {
            if requested
                .is_some_and(|s| !s.0.is_empty() && field(turn, "id").as_js_string() != Some(s))
            {
                continue;
            }
            for item in field(turn, "items").as_array().unwrap_or(&[]) {
                entries.push(J::object([
                    ("turnId", field(turn, "id").clone()),
                    ("item", item.clone()),
                ]))
            }
        }
        if is(field(p, "sortDirection"), "desc") {
            entries.reverse()
        }
        let start = if field(p, "cursor").is_null() {
            0.0
        } else {
            number(field(p, "cursor"))
        };
        if !safe_integer(start) || start < 0.0 || start > entries.len() as f64 {
            return Err(AdapterError::new("invalid item cursor"));
        }
        let limit = if field(p, "limit").is_null() {
            100.0
        } else {
            number(field(p, "limit"))
        };
        if !safe_integer(limit) || !(1.0..=100.0).contains(&limit) {
            return Err(AdapterError::new("invalid item limit"));
        }
        let data = array_slice(&entries, start, start + limit);
        let end = start + data.len() as f64;
        Ok(J::object([
            (
                "nextCursor",
                if end < entries.len() as f64 {
                    string(&J::Number(end)).into_json()
                } else {
                    J::Null
                },
            ),
            (
                "backwardsCursor",
                if data.is_empty() {
                    J::Null
                } else {
                    string(&J::Number(start)).into_json()
                },
            ),
            ("data", J::Array(data)),
        ]))
    }
    fn config_read(&self) -> Result<J> {
        let mut config = self.env.config_overrides.clone();
        if config.as_object().is_none() {
            config = object()
        }
        for key in [
            "provider_loop_provider",
            "providerLoopProvider",
            "provider_loop_config.providerId",
            "provider_loop_agent_loop",
            "providerLoopAgentLoop",
            "provider_loop_config.loopId",
        ] {
            config.remove(key);
        }
        for (key, default) in [
            ("approval_policy", "on-request"),
            ("approvals_reviewer", "user"),
            ("sandbox_mode", "workspace-write"),
            ("web_search", "disabled"),
        ] {
            if field(&config, key).is_null() {
                config.insert(key, default.into())
            }
        }
        for key in [
            "sandbox_workspace_write",
            "tools",
            "profile",
            "instructions",
            "developer_instructions",
            "compact_prompt",
            "model_reasoning_summary",
            "model_verbosity",
            "service_tier",
            "analytics",
            "apps",
        ] {
            if config.get(key).is_none() {
                config.insert(key, J::Null)
            }
        }
        for key in [
            "review_model",
            "model_context_window",
            "model_auto_compact_token_limit",
            "forced_chatgpt_workspace_id",
            "forced_login_method",
        ] {
            config.insert(key, J::Null)
        }
        config.insert("model", self.env.default_model.clone());
        config.insert("model_reasoning_effort", self.env.default_effort.clone());
        config.insert("model_provider", "claude-code".into());
        config.insert("profiles", object());
        config.insert(
            "provider_loop_config",
            config::provider_loop_config(&self.env.environment, &self.env.config_overrides)?,
        );
        let mut provider = object();
        for key in [
            "base_url",
            "env_key",
            "env_key_instructions",
            "experimental_bearer_token",
            "auth",
            "aws",
            "query_params",
            "http_headers",
            "env_http_headers",
            "request_max_retries",
            "stream_max_retries",
            "stream_idle_timeout_ms",
            "websocket_connect_timeout_ms",
        ] {
            provider.insert(key, J::Null)
        }
        provider.insert("name", "Claude Code".into());
        provider.insert("wire_api", "responses".into());
        provider.insert("requires_openai_auth", false.into());
        provider.insert("supports_websockets", false.into());
        config.insert("model_providers", J::object([("claude-code", provider)]));
        let metadata = || {
            J::object([
                (
                    "name",
                    J::object([
                        ("type", "user".into()),
                        (
                            "file",
                            concat(&[&self.env.codex_home, &"/config.toml".into()]).into_json(),
                        ),
                    ]),
                ),
                (
                    "version",
                    concat(&[&"claude-codex-".into(), &string(&J::Number((self.clock)()))])
                        .into_json(),
                ),
            ])
        };
        Ok(J::object([
            ("config", config),
            (
                "origins",
                J::object([
                    ("model_provider", metadata()),
                    ("model_providers.claude-code", metadata()),
                ]),
            ),
            ("layers", J::Null),
        ]))
    }
    pub fn permission_request(
        &mut self,
        turn: &JsString,
        event: &J,
        callback: J,
    ) -> Result<Vec<Effect>> {
        let mut out = vec![];
        let Some(mut state) = self.turns.remove(turn) else {
            out.push(Effect::Permission {
                callback,
                result: J::object([("decision", "cancel".into())]),
            });
            return Ok(out);
        };
        let result = (|| -> Result<()> {
            if self.stopped || self.active.get(&string(field(&state.thread, "id"))) != Some(turn) {
                out.push(Effect::Permission {
                    callback: callback.clone(),
                    result: J::object([("decision", "cancel".into())]),
                });
                return Ok(());
            }
            self.complete_message(&mut state, turn, "commentary", &mut out)?;
            let tool = string(field(event, "toolUseId"));
            let item = if let Some(id) = state.item_ids.get(&tool) {
                id.clone()
            } else {
                let item =
                    super::items::tool_item((self.ids)(), event, field(&state.thread, "cwd"));
                let id = string(field(&item, "id"));
                state.item_ids.insert(tool, id.clone());
                self.store.append_item(turn, &item)?;
                self.item_event(&state, turn, &item, true, &mut out);
                if is(field(&item, "type"), "fileChange") {
                    self.notify(
                        &state.peer,
                        "item/fileChange/patchUpdated",
                        J::object([
                            ("threadId", field(&state.thread, "id").clone()),
                            ("turnId", turn.clone().into_json()),
                            ("itemId", id.clone().into_json()),
                            ("changes", field(&item, "changes").clone()),
                        ]),
                        &mut out,
                    )
                }
                id
            };
            let thread = string(field(&state.thread, "id"));
            let current = self.thread(&thread.clone().into_json())?;
            let command = string(&coalesce(
                field(field(event, "input"), "command"),
                &"".into(),
            ));
            if is(field(&current, "approvalPolicy"), "never")
                || is(field(&current, "sandboxMode"), "danger-full-access")
                || self
                    .command_allow
                    .get(&thread)
                    .is_some_and(|set| !command.0.is_empty() && set.contains(&command))
            {
                out.push(Effect::Permission {
                    callback: callback.clone(),
                    result: J::object([("decision", "accept".into())]),
                });
                return Ok(());
            }
            self.status(
                &state.peer,
                &thread,
                J::object([
                    ("type", "active".into()),
                    ("activeFlags", J::Array(vec!["waitingOnApproval".into()])),
                ]),
                &mut out,
            )?;
            let request = (self.ids)();
            let mut params = J::object([
                ("threadId", thread.clone().into_json()),
                ("turnId", turn.clone().into_json()),
                ("itemId", item.clone().into_json()),
                ("startedAtMs", ((self.millis)()).into_json()),
                ("reason", J::Null),
            ]);
            let bash = is(field(event, "toolName"), "Bash");
            if bash {
                for (key, value) in [
                    ("approvalId", request.clone().into_json()),
                    ("command", command.into_json()),
                    (
                        "cwd",
                        string(&coalesce(field(field(event, "input"), "cwd"), &"".into()))
                            .into_json(),
                    ),
                    ("commandActions", J::Array(vec![])),
                    ("additionalPermissions", J::Null),
                    ("proposedExecpolicyAmendment", J::Null),
                    ("proposedNetworkPolicyAmendments", J::Null),
                    (
                        "availableDecisions",
                        J::Array(
                            ["accept", "acceptForSession", "decline", "cancel"]
                                .map(J::from)
                                .to_vec(),
                        ),
                    ),
                ] {
                    params.insert(key, value)
                }
            } else {
                params.insert("grantRoot", J::Null)
            }
            self.reverse.push(Reverse {
                request_id: request.clone(),
                peer: state.peer.clone(),
                thread,
                turn: turn.clone(),
                item,
                callback: callback.clone(),
                event: event.clone(),
                questions: None,
                started: 0.0,
            });
            out.push(Effect::Send {
                peer: self
                    .peers
                    .iter()
                    .find(|(t, _)| field(&state.thread, "id").as_js_string() == Some(t))
                    .map(|(_, p)| p.clone())
                    .unwrap_or_else(|| state.peer.clone()),
                message: J::object([
                    ("jsonrpc", "2.0".into()),
                    ("id", request.into_json()),
                    (
                        "method",
                        if bash {
                            "item/commandExecution/requestApproval".into()
                        } else {
                            "item/fileChange/requestApproval".into()
                        },
                    ),
                    ("params", params),
                ]),
            });
            Ok(())
        })();
        self.turns.insert(turn.clone(), state);
        result?;
        Ok(out)
    }
    pub fn user_input_request(
        &mut self,
        turn: &JsString,
        event: &J,
        callback: J,
    ) -> Result<Vec<Effect>> {
        let mut out = vec![];
        let Some(mut state) = self.turns.remove(turn) else {
            out.push(Effect::Permission {
                callback,
                result: J::object([("answers", object())]),
            });
            return Ok(out);
        };
        let result = (|| -> Result<()> {
            self.complete_message(&mut state, turn, "commentary", &mut out)?;
            let questions = field(event, "questions").as_array().unwrap_or(&[]).to_vec();
            let item = J::object([
                ("type", "dynamicToolCall".into()),
                ("id", (self.ids)().into_json()),
                ("namespace", "claude".into()),
                ("tool", "AskUserQuestion".into()),
                (
                    "arguments",
                    J::object([("questions", J::Array(questions.clone()))]),
                ),
                ("status", "inProgress".into()),
                ("contentItems", J::Null),
                ("success", J::Null),
                ("durationMs", J::Null),
            ]);
            self.store.append_item(turn, &item)?;
            let started = (self.millis)();
            self.item_event(&state, turn, &item, true, &mut out);
            let thread = string(field(&state.thread, "id"));
            self.status(
                &state.peer,
                &thread,
                J::object([
                    ("type", "active".into()),
                    ("activeFlags", J::Array(vec!["waitingOnUserInput".into()])),
                ]),
                &mut out,
            )?;
            let request = (self.ids)();
            let normalized = questions
                .into_iter()
                .map(|mut q| {
                    let mut options = field(&q, "options").as_array().unwrap_or(&[]).to_vec();
                    if !options.iter().any(|o| is(field(o, "label"), "Other")) {
                        options.push(J::object([
                            ("label", "Other".into()),
                            ("description", "Provide a free-form answer".into()),
                        ]));
                        q.insert("options", J::Array(options))
                    }
                    q
                })
                .collect::<Vec<_>>();
            let params = J::object([
                ("threadId", thread.clone().into_json()),
                ("turnId", turn.clone().into_json()),
                ("itemId", field(&item, "id").clone()),
                ("questions", J::Array(normalized.clone())),
                ("isBlocking", true.into()),
            ]);
            self.reverse.push(Reverse {
                request_id: request.clone(),
                peer: state.peer.clone(),
                thread,
                turn: turn.clone(),
                item: string(field(&item, "id")),
                callback: callback.clone(),
                event: event.clone(),
                questions: Some(normalized),
                started,
            });
            out.push(Effect::Send {
                peer: self
                    .peers
                    .iter()
                    .find(|(t, _)| field(&state.thread, "id").as_js_string() == Some(t))
                    .map(|(_, p)| p.clone())
                    .unwrap_or_else(|| state.peer.clone()),
                message: J::object([
                    ("jsonrpc", "2.0".into()),
                    ("id", request.into_json()),
                    ("method", "item/tool/requestUserInput".into()),
                    ("params", params),
                ]),
            });
            Ok(())
        })();
        self.turns.insert(turn.clone(), state);
        result?;
        Ok(out)
    }
    fn resolve_reverse(&mut self, message: &J, out: &mut Vec<Effect>) -> Result<()> {
        let suffix = string(field(message, "id"));
        let Some(index) = self.reverse.iter().position(|p| p.request_id == suffix) else {
            return Ok(());
        };
        let pending = self.reverse.remove(index);
        self.notify(
            &pending.peer,
            "serverRequest/resolved",
            J::object([
                ("threadId", pending.thread.clone().into_json()),
                ("requestId", pending.request_id.into_json()),
            ]),
            out,
        );
        if pending.questions.is_none() {
            self.status(
                &pending.peer,
                &pending.thread,
                J::object([("type", "active".into()), ("activeFlags", J::Array(vec![]))]),
                out,
            )?;
        }
        if !field(message, "error").is_null() {
            out.push(Effect::Permission {
                callback: pending.callback,
                result: J::object([("error", field(message, "error").clone())]),
            });
            return Ok(());
        }
        let active = !self.stopped
            && self.active.get(&pending.thread) == Some(&pending.turn)
            && self
                .store
                .get_turn(&pending.turn)?
                .is_some_and(|t| is(field(&t, "status"), "inProgress"));
        if let Some(questions) = pending.questions {
            let answers = if active {
                super::items::normalize_answers(field(message, "result"), &questions)
            } else {
                J::object([("answers", object())])
            };
            if active {
                let content = super::items::answer_content(&questions, &answers);
                self.store
                    .update_item(&pending.turn, &pending.item, |mut item| {
                        item.insert("status", "completed".into());
                        item.insert("success", true.into());
                        item.insert("contentItems", content.clone());
                        item.insert(
                            "durationMs",
                            maximum(0.0, (self.millis)() - pending.started).into_json(),
                        );
                        Ok(item)
                    })?;
                if let Some(state) = self.turns.get(&pending.turn) {
                    if let Some(turn) = self.store.get_turn(&pending.turn)? {
                        if let Some(item) = field(&turn, "items")
                            .as_array()
                            .unwrap_or(&[])
                            .iter()
                            .find(|i| field(i, "id").as_js_string() == Some(&pending.item))
                        {
                            self.item_event(state, &pending.turn, item, false, out)
                        }
                    }
                }
                self.status(
                    &pending.peer,
                    &pending.thread,
                    J::object([("type", "active".into()), ("activeFlags", J::Array(vec![]))]),
                    out,
                )?;
            }
            out.push(Effect::Permission {
                callback: pending.callback,
                result: answers,
            });
        } else {
            let decision = if active {
                policy::decision(field(message, "result"))
            } else {
                "cancel".into()
            };
            if is(&decision, "acceptForSession") {
                let command = string(&coalesce(
                    field(field(&pending.event, "input"), "command"),
                    &"".into(),
                ));
                if !command.0.is_empty() {
                    self.command_allow
                        .entry(pending.thread)
                        .or_default()
                        .insert(command);
                }
            }
            out.push(Effect::Permission {
                callback: pending.callback,
                result: J::object([("decision", decision)]),
            });
        }
        Ok(())
    }
}
