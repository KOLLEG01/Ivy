//! Turn/item state translated from the pinned server. Provider work is never
//! awaited while this SQLite owner handles another native message.
use super::{
    commands::{self, WorkflowCommand},
    error::{AdapterError, Result},
    images, policy,
    protocol::{Adapter, CallKind, Effect, TurnState},
    value::*,
};
use ivy_contracts::{IvyJson as J, JsString};
use std::collections::{BTreeMap, BTreeSet};
impl Adapter {
    pub(super) fn status(
        &self,
        peer: &JsString,
        thread: &JsString,
        status: J,
        out: &mut Vec<Effect>,
    ) -> Result<()> {
        self.store.update_thread_status(thread, &status)?;
        self.notify(
            peer,
            "thread/status/changed",
            J::object([("threadId", thread.clone().into_json()), ("status", status)]),
            out,
        );
        Ok(())
    }
    pub(super) fn turn_start(
        &mut self,
        peer: &JsString,
        p: &J,
        out: &mut Vec<Effect>,
    ) -> Result<J> {
        let mut thread = self.thread(field(p, "threadId"))?;
        let thread_id = string(field(&thread, "id"));
        self.bind(thread_id.clone(), peer.clone());
        if is(field(&thread, "runtimeBackend"), "codex")
            || Self::codex_model(&policy::model_from_params(p, field(&thread, "model")))
        {
            return Err(AdapterError::new(
                "Codex proxy models are disabled for this Claude adapter",
            ));
        }
        let id = (self.ids)();
        let input = field(p, "input")
            .as_array()
            .map(|a| a.to_vec())
            .unwrap_or_default();
        let (text, images) = images::extract(&J::Array(input.clone()))?;
        let prompt = if text.0.is_empty() {
            policy::text_input(&J::Array(input.clone()))
        } else {
            text
        };
        if is(field(field(p, "collaborationMode"), "mode"), "default")
            || is(field(field(p, "collaborationMode"), "mode"), "plan")
        {
            thread.insert(
                "collaborationMode",
                field(field(p, "collaborationMode"), "mode").clone(),
            )
        }
        if field(p, "cwd")
            .as_js_string()
            .is_some_and(|s| !s.0.is_empty())
        {
            thread.insert("cwd", field(p, "cwd").clone())
        }
        let model = policy::model_from_params(p, field(&thread, "model"));
        if truthy(&model) {
            thread.insert("model", model)
        }
        let effort = policy::effort_from_params(p, field(&thread, "reasoningEffort"));
        if !effort.is_null() {
            thread.insert("reasoningEffort", effort)
        }
        policy::apply_permissions(&mut thread, p, true);
        self.store.upsert_thread(&thread)?;
        if !truthy(field(&thread, "preview")) && !prompt.0.is_empty() {
            thread.insert("preview", slice(&prompt, 0, 200).into_json());
            thread.insert("updatedAt", (self.clock)().into_json());
            self.store.upsert_thread(&thread)?
        }
        let mut items = vec![J::object([
            ("type", "userMessage".into()),
            ("id", (self.ids)().into_json()),
            ("content", J::Array(input)),
        ])];
        for image in &images {
            let path = string(field(image, "displayPath"));
            if is(field(image, "kind"), "base64") && path.0.starts_with(&[47]) {
                items.push(J::object([
                    ("type", "imageView".into()),
                    ("id", (self.ids)().into_json()),
                    ("path", path.into_json()),
                ]))
            }
        }
        let turn = J::object([
            ("id", id.clone().into_json()),
            ("threadId", thread_id.clone().into_json()),
            ("status", "inProgress".into()),
            ("startedAt", (self.clock)().into_json()),
            ("completedAt", J::Null),
            ("durationMs", J::Null),
            ("items", J::Array(items)),
            ("diff", "".into()),
            ("error", J::Null),
        ]);
        self.store.upsert_turn(&turn)?;
        self.active.insert(thread_id.clone(), id.clone());
        let state = TurnState {
            peer: peer.clone(),
            plan_mode: is(field(&thread, "collaborationMode"), "plan")
                || field(p, "planMode").as_bool() == Some(true),
            thread,
            params: p.clone(),
            prompt,
            images,
            agent: None,
            reasoning: None,
            plan: None,
            item_ids: BTreeMap::new(),
            item_started: BTreeMap::new(),
            output_seen: BTreeSet::new(),
            notices: BTreeSet::new(),
            has_text: false,
            subagents: vec![],
            workflow_tools: BTreeSet::new(),
        };
        self.turns.insert(id.clone(), state);
        self.status(
            peer,
            &thread_id,
            J::object([("type", "active".into()), ("activeFlags", J::Array(vec![]))]),
            out,
        )?;
        out.push(Effect::Begin { turn_id: id });
        Ok(J::object([("turn", policy::turn_view(&turn, "notLoaded"))]))
    }
    pub fn begin_turn(&mut self, id: &JsString) -> Result<Vec<Effect>> {
        let mut out = vec![];
        let Some(state) = self.turns.get(id) else {
            return Ok(out);
        };
        if self.stopped {
            return Ok(out);
        }
        let turn = self
            .store
            .get_turn(id)?
            .ok_or_else(|| AdapterError::new("turn disappeared before runtime start"))?;
        let thread_id = field(&state.thread, "id").clone();
        self.notify(
            &state.peer,
            "turn/started",
            J::object([
                ("threadId", thread_id.clone()),
                ("turn", policy::turn_view(&turn, "notLoaded")),
            ]),
            &mut out,
        );
        for item in field(&turn, "items")
            .as_array()
            .unwrap_or(&[])
            .iter()
            .filter(|i| is(field(i, "type"), "imageView"))
        {
            self.item_event(state, id, item, true, &mut out);
            self.item_event(state, id, item, false, &mut out);
        }
        if field(&state.params, "outputSchema").is_null()
            && matches!(
                commands::workflow_command(&state.prompt),
                Some(WorkflowCommand::List)
            )
        {
            out.extend(self.complete_workflow_list(id)?);
            return Ok(out);
        }
        let mut context = J::object([
            ("threadId", thread_id),
            ("turnId", id.clone().into_json()),
            (
                "purpose",
                if field(&state.params, "outputSchema").is_null() {
                    "normal".into()
                } else {
                    "summary".into()
                },
            ),
            ("prompt", state.prompt.clone().into_json()),
            (
                "cwd",
                str_or(field(&state.params, "cwd"), field(&state.thread, "cwd")),
            ),
            ("runtimeType", J::Null),
            (
                "model",
                super::models::resolve_model(
                    &str_or(field(&state.params, "model"), field(&state.thread, "model")),
                    !field(&state.params, "outputSchema").is_null(),
                    &self.env.environment,
                    &self.env.discovered_router_models,
                ),
            ),
            (
                "effort",
                super::models::resolve_effort(
                    &if field(&state.params, "effort").as_js_string().is_some() {
                        field(&state.params, "effort").clone()
                    } else {
                        coalesce(
                            field(&state.thread, "reasoningEffort"),
                            field(&self.env.environment, "CLAUDE_CODEX_EFFORT"),
                        )
                    },
                    &self.env.environment,
                ),
            ),
            (
                "claudeSessionId",
                field(&state.thread, "claudeSessionId").clone(),
            ),
            (
                "forkSession",
                (!field(&state.thread, "forkedFromId").is_null()
                    && !field(&state.thread, "claudeSessionId").is_null()
                    && self
                        .store
                        .list_turns(&string(field(&state.thread, "id")))?
                        .len()
                        <= 1)
                    .into(),
            ),
            ("mcpServers", self.env.mcp_servers.clone()),
            (
                "allowedTools",
                super::models::allowed_tools(&self.env.environment),
            ),
            ("addDirs", super::models::add_dirs(&self.env.environment)),
            (
                "enableFileCheckpointing",
                is(
                    field(
                        &self.env.environment,
                        "CLAUDE_CODEX_ENABLE_FILE_CHECKPOINTING",
                    ),
                    "1",
                )
                .into(),
            ),
            (
                "outputFormat",
                if field(&state.params, "outputSchema").is_null() {
                    J::Null
                } else {
                    let schema = field(&state.params, "outputSchema");
                    if is(field(schema, "type"), "json_schema")
                        && !field(schema, "schema").is_null()
                    {
                        schema.clone()
                    } else {
                        J::object([("type", "json_schema".into()), ("schema", schema.clone())])
                    }
                },
            ),
            (
                "approvalPolicy",
                field(&state.thread, "approvalPolicy").clone(),
            ),
            ("sandboxMode", field(&state.thread, "sandboxMode").clone()),
            ("planMode", state.plan_mode.into()),
            ("imageInputs", J::Array(state.images.clone())),
        ]);
        let base = if field(&state.params, "baseInstructions")
            .as_js_string()
            .is_some()
        {
            field(&state.params, "baseInstructions")
        } else {
            field(&state.thread, "baseInstructions")
        };
        let dev = if field(&state.params, "developerInstructions")
            .as_js_string()
            .is_some()
        {
            field(&state.params, "developerInstructions")
        } else {
            field(&state.thread, "developerInstructions")
        };
        let personality = if field(&state.params, "personality").as_js_string().is_some() {
            policy::personality(field(&state.params, "personality"))
        } else {
            field(&state.thread, "personality").clone()
        };
        let desktop = field(&state.params, "outputSchema").is_null()
            && !is(field(&state.thread, "runtimeBackend"), "codex");
        let append = if !field(&state.thread, "forkedFromId").is_null() && truthy(dev) {
            let parent = self
                .store
                .get_thread(&string(field(&state.thread, "forkedFromId")))?
                .unwrap_or(J::Null);
            if field(&parent, "developerInstructions") != dev {
                context.insert(
                    "prompt",
                    concat(&[
                        &"<side-conversation-instructions>\n".into(),
                        &string(dev),
                        &"\n</side-conversation-instructions>\n\n".into(),
                        &state.prompt,
                    ])
                    .into_json(),
                );
                policy::system_prompt(
                    &coalesce(field(&parent, "baseInstructions"), base),
                    field(&parent, "developerInstructions"),
                    &coalesce(field(&parent, "personality"), &personality),
                    desktop,
                )
            } else {
                policy::system_prompt(base, dev, &personality, desktop)
            }
        } else {
            policy::system_prompt(base, dev, &personality, desktop)
        };
        context.insert("systemPromptAddendum", append);
        out.push(Effect::Run {
            turn_id: id.clone(),
            context,
        });
        Ok(out)
    }
    pub(super) fn steer(
        &mut self,
        peer: &JsString,
        id: &J,
        p: &J,
        out: &mut Vec<Effect>,
    ) -> Result<()> {
        let thread = string(&str_or(field(p, "threadId"), &"".into()));
        let expected = string(&str_or(field(p, "expectedTurnId"), &"".into()));
        let Some(turn) = self.active.get(&thread).cloned() else {
            return Err(AdapterError::new(concat(&[
                &"thread has no active turn: ".into(),
                &thread,
            ])));
        };
        if !expected.0.is_empty() && expected != turn {
            return Err(AdapterError::new(concat(&[
                &"active turn mismatch: expected ".into(),
                &expected,
                &", got ".into(),
                &turn,
            ])));
        }
        let input = J::Array(field(p, "input").as_array().unwrap_or(&[]).to_vec());
        let prompt = policy::text_input(&input);
        self.store.append_item(
            &turn,
            &J::object([
                ("type", "userMessage".into()),
                ("id", (self.ids)().into_json()),
                ("content", input),
            ]),
        )?;
        self.call(
            peer,
            id,
            CallKind::Steer(turn.clone().into_json()),
            "steer",
            J::object([
                ("threadId", thread.into_json()),
                ("prompt", prompt.into_json()),
            ]),
            out,
        );
        Ok(())
    }
    pub(super) fn interrupt(
        &mut self,
        peer: &JsString,
        id: &J,
        p: &J,
        out: &mut Vec<Effect>,
    ) -> Result<()> {
        let thread = string(&str_or(field(p, "threadId"), &"".into()));
        self.bind(thread.clone(), peer.clone());
        let turn = self
            .active
            .get(&thread)
            .cloned()
            .unwrap_or_else(|| string(&str_or(field(p, "turnId"), &"".into())));
        if !turn.0.is_empty() {
            if let Some(current) = self.store.get_turn(&turn)? {
                if is(field(&current, "status"), "inProgress") {
                    if let Some(mut state) = self.turns.remove(&turn) {
                        state.peer = peer.clone();
                        self.finalize_subagents(
                            &mut state,
                            &turn,
                            "Subagent ended without a terminal task result.",
                            out,
                        )?;
                    }
                    let completed = self
                        .store
                        .complete_turn(
                            &turn,
                            &"interrupted".into(),
                            &J::object([("message", "interrupted".into())]),
                        )?
                        .unwrap_or(current);
                    self.notify(
                        peer,
                        "turn/completed",
                        J::object([
                            ("threadId", thread.clone().into_json()),
                            ("turn", policy::turn_view(&completed, "notLoaded")),
                        ]),
                        out,
                    );
                }
            }
        }
        self.active.remove(&thread);
        self.turns.remove(&turn);
        self.status(peer, &thread, J::object([("type", "idle".into())]), out)?;
        self.call(
            peer,
            id,
            CallKind::Interrupt,
            "interrupt",
            J::object([
                ("threadId", thread.into_json()),
                ("turnId", turn.into_json()),
            ]),
            out,
        );
        Ok(())
    }
    pub(super) fn item_event(
        &self,
        state: &TurnState,
        turn: &JsString,
        item: &J,
        start: bool,
        out: &mut Vec<Effect>,
    ) {
        let mut params = J::object([
            ("threadId", field(&state.thread, "id").clone()),
            ("turnId", turn.clone().into_json()),
            ("item", item.clone()),
        ]);
        params.insert(
            if start {
                "startedAtMs"
            } else {
                "completedAtMs"
            },
            ((self.millis)()).into_json(),
        );
        self.notify(
            &state.peer,
            if start {
                "item/started"
            } else {
                "item/completed"
            },
            params,
            out,
        );
    }
    fn item(&self, turn: &JsString, id: &JsString) -> Result<Option<J>> {
        Ok(self.store.get_turn(turn)?.and_then(|t| {
            field(&t, "items")
                .as_array()
                .unwrap_or(&[])
                .iter()
                .find(|i| field(i, "id").as_js_string() == Some(id))
                .cloned()
        }))
    }
    pub(super) fn complete_message(
        &self,
        state: &mut TurnState,
        turn: &JsString,
        phase: &str,
        out: &mut Vec<Effect>,
    ) -> Result<()> {
        if let Some(id) = state.reasoning.take() {
            if let Some(item) = self.item(turn, &id)? {
                self.item_event(state, turn, &item, false, out)
            }
        }
        if let Some(id) = state.agent.take() {
            self.store.update_item(turn, &id, |item| {
                let mut v = item.clone();
                if is(field(&item, "type"), "agentMessage") {
                    v.insert("phase", phase.into())
                }
                Ok(v)
            })?;
            if let Some(item) = self.item(turn, &id)? {
                self.item_event(state, turn, &item, false, out)
            }
        }
        Ok(())
    }
    pub fn runtime_event(&mut self, turn: &JsString, event: &J) -> Result<Vec<Effect>> {
        let mut out = vec![];
        let Some(mut state) = self.turns.remove(turn) else {
            return Ok(out);
        };
        let thread = string(field(&state.thread, "id"));
        let active = !self.stopped
            && self.active.get(&thread) == Some(turn)
            && self
                .store
                .get_turn(turn)?
                .is_some_and(|t| is(field(&t, "status"), "inProgress"));
        if !active {
            return Ok(out);
        }
        let result = self.on_event(&mut state, turn, event, &mut out);
        self.turns.insert(turn.clone(), state);
        result?;
        Ok(out)
    }
    fn on_event(
        &mut self,
        state: &mut TurnState,
        turn: &JsString,
        e: &J,
        out: &mut Vec<Effect>,
    ) -> Result<()> {
        if !state.subagents.is_empty() {
            if ["text_delta", "reasoning_delta", "message_boundary"]
                .iter()
                .any(|k| is(field(e, "type"), k))
            {
                return Ok(());
            }
            if is(field(e, "type"), "tool_use") && !policy::is_subagent(field(e, "toolName")) {
                return Ok(());
            }
            if ["tool_result", "tool_output_delta"]
                .iter()
                .any(|k| is(field(e, "type"), k))
                && !state.item_ids.contains_key(&string(field(e, "toolUseId")))
            {
                return Ok(());
            }
        }
        match field(e, "type").as_str().ok().as_deref() {
            Some("session") => self.store.update_claude_session_id(
                &string(field(&state.thread, "id")),
                field(e, "claudeSessionId").as_js_string(),
            )?,
            Some("message_boundary") => self.complete_message(state, turn, "commentary", out)?,
            Some("text_delta" | "reasoning_delta") => {
                let delta = string(field(e, "delta"));
                if delta.0.is_empty() {
                    return Ok(());
                }
                let thinking = is(field(e, "type"), "reasoning_delta");
                if thinking {
                    if state.agent.is_some() {
                        let reasoning = state.reasoning.take();
                        self.complete_message(state, turn, "commentary", out)?;
                        state.reasoning = reasoning
                    }
                } else {
                    if let Some(id) = state.reasoning.take() {
                        if let Some(item) = self.item(turn, &id)? {
                            self.item_event(state, turn, &item, false, out)
                        }
                    }
                    state.has_text = true;
                }
                let current = if thinking {
                    &mut state.reasoning
                } else if state.plan_mode {
                    &mut state.plan
                } else {
                    &mut state.agent
                };
                let (id, new) = match current {
                    Some(id) => (id.clone(), false),
                    None => {
                        let id = (self.ids)();
                        *current = Some(id.clone());
                        (id, true)
                    }
                };
                if new {
                    let item = if thinking {
                        J::object([
                            ("type", "reasoning".into()),
                            ("id", id.clone().into_json()),
                            ("summary", J::Array(vec![])),
                            ("content", J::Array(vec!["".into()])),
                        ])
                    } else if state.plan_mode {
                        J::object([
                            ("type", "plan".into()),
                            ("id", id.clone().into_json()),
                            ("text", "".into()),
                        ])
                    } else {
                        J::object([
                            ("type", "agentMessage".into()),
                            ("id", id.clone().into_json()),
                            ("text", "".into()),
                            ("phase", J::Null),
                            ("memoryCitation", J::Null),
                        ])
                    };
                    self.store.append_item(turn, &item)?;
                    self.item_event(state, turn, &item, true, out)
                }
                self.store.update_item(turn, &id, |item| {
                    let mut item = item.clone();
                    if thinking {
                        let first = field(&item, "content")
                            .as_array()
                            .and_then(|a| a.first())
                            .unwrap_or(&NULL);
                        item.insert(
                            "content",
                            J::Array(vec![concat(&[&string(first), &delta]).into_json()]),
                        );
                    } else {
                        item.insert(
                            "text",
                            concat(&[&string(field(&item, "text")), &delta]).into_json(),
                        )
                    }
                    Ok(item)
                })?;
                let mut params = J::object([
                    ("threadId", field(&state.thread, "id").clone()),
                    ("turnId", turn.clone().into_json()),
                    ("itemId", id.into_json()),
                    ("delta", delta.into_json()),
                ]);
                if thinking {
                    params.insert("contentIndex", 0.0.into_json())
                }
                self.notify(
                    &state.peer,
                    if thinking {
                        "item/reasoning/textDelta"
                    } else if state.plan_mode {
                        "item/plan/delta"
                    } else {
                        "item/agentMessage/delta"
                    },
                    params,
                    out,
                );
            }
            Some("rate_limit") => self.limits.record_event(field(e, "info"), (self.millis)()),
            Some("tool_use" | "tool_output_delta" | "tool_result") => {
                self.tool_event(state, turn, e, out)?
            }
            Some("hook") => {
                let run = (self.ids)();
                let mut fragments = vec![J::object([
                    (
                        "text",
                        concat(&[&"Hook · ".into(), &string(field(e, "hookName"))]).into_json(),
                    ),
                    ("hookRunId", run.clone().into_json()),
                ])];
                for (key, prefix) in [
                    ("status", "status: "),
                    ("decision", "decision: "),
                    ("message", ""),
                ] {
                    if truthy(field(e, key)) {
                        fragments.push(J::object([
                            (
                                "text",
                                concat(&[&prefix.into(), &string(field(e, key))]).into_json(),
                            ),
                            ("hookRunId", run.clone().into_json()),
                        ]))
                    }
                }
                let item = J::object([
                    ("type", "hookPrompt".into()),
                    ("id", (self.ids)().into_json()),
                    ("fragments", J::Array(fragments)),
                ]);
                self.store.append_item(turn, &item)?;
                self.item_event(state, turn, &item, true, out);
                self.item_event(state, turn, &item, false, out);
            }
            Some("usage") => self.token_usage(state, turn, field(e, "usage"), out),
            Some("notice") => {
                if !is(field(e, "level"), "info")
                    && state.notices.insert(string(field(e, "message")))
                {
                    self.notify(
                        &state.peer,
                        "warning",
                        J::object([
                            ("threadId", field(&state.thread, "id").clone()),
                            ("message", field(e, "message").clone()),
                        ]),
                        out,
                    )
                }
            }
            Some("completed") => {
                if truthy(field(e, "claudeSessionId")) {
                    self.store.update_claude_session_id(
                        &string(field(&state.thread, "id")),
                        field(e, "claudeSessionId").as_js_string(),
                    )?
                }
                if !truthy(field(e, "success")) {
                    return Err(AdapterError::new(string(&coalesce(
                        field(e, "result"),
                        &"Claude turn failed".into(),
                    ))));
                }
            }
            Some("error") => return Err(AdapterError::new(string(field(e, "message")))),
            _ => {}
        }
        Ok(())
    }
    pub(super) fn token_usage(
        &mut self,
        state: &TurnState,
        turn: &JsString,
        usage: &J,
        out: &mut Vec<Effect>,
    ) {
        let num = |key| {
            field(usage, key)
                .as_f64()
                .filter(|n| n.is_finite())
                .unwrap_or(0.0)
        };
        let input = num("input_tokens") + num("cache_creation_input_tokens");
        let output = num("output_tokens");
        let cached = num("cache_read_input_tokens");
        let total = input + cached + output;
        if total == 0.0 {
            return;
        }
        let last = J::object([
            ("inputTokens", input.into_json()),
            ("cachedInputTokens", cached.into_json()),
            ("outputTokens", output.into_json()),
            ("reasoningOutputTokens", J::Number(0.0)),
            ("totalTokens", total.into_json()),
        ]);
        let thread = string(field(&state.thread, "id"));
        let prior = self.tokens.entry(thread.clone()).or_insert_with(object);
        let mut total = object();
        for (key, value) in last.as_object().unwrap() {
            let old = prior.get_key(key).and_then(J::as_f64).unwrap_or(0.0);
            total.insert(key.clone(), J::Number(old + number(value)))
        }
        *prior = total.clone();
        self.notify(
            &state.peer,
            "thread/tokenUsage/updated",
            J::object([
                ("threadId", thread.into_json()),
                ("turnId", turn.clone().into_json()),
                (
                    "tokenUsage",
                    J::object([
                        ("total", total),
                        ("last", last),
                        ("modelContextWindow", J::Null),
                    ]),
                ),
            ]),
            out,
        );
    }
    fn tool_event(
        &mut self,
        state: &mut TurnState,
        turn: &JsString,
        e: &J,
        out: &mut Vec<Effect>,
    ) -> Result<()> {
        let tool = string(field(e, "toolUseId"));
        let input = field(e, "input");
        if is(field(e, "type"), "tool_use") {
            let name = string(field(e, "toolName")).trim().to_english_lowercase();
            if name == JsString::from("workflow") || name == JsString::from("workflows") {
                state.workflow_tools.insert(tool.clone());
                if let Some(timeout_ms) = self.env.subagent_watchdog_ms.filter(|ms| *ms > 0) {
                    out.push(Effect::Watchdog {
                        turn_id: turn.clone(),
                        timeout_ms,
                    });
                }
            }
            if state.item_ids.contains_key(&tool) {
                return Ok(());
            }
            self.complete_message(state, turn, "commentary", out)?;
            if policy::is_subagent(field(e, "toolName")) {
                return self.spawn_subagent(state, turn, e, out);
            }
            if is(field(e, "toolName"), "TodoWrite") {
                if let Some(todos) = field(input, "todos").as_array() {
                    let plan = todos
                        .iter()
                        .map(|todo| {
                            J::object([
                                (
                                    "step",
                                    str_or(
                                        field(todo, "content"),
                                        &if field(todo, "activeForm").as_js_string().is_some() {
                                            field(todo, "activeForm").clone()
                                        } else {
                                            "".into()
                                        },
                                    ),
                                ),
                                (
                                    "status",
                                    if is(field(todo, "status"), "in_progress") {
                                        "inProgress".into()
                                    } else if is(field(todo, "status"), "completed") {
                                        "completed".into()
                                    } else {
                                        "pending".into()
                                    },
                                ),
                            ])
                        })
                        .collect();
                    self.notify(
                        &state.peer,
                        "turn/plan/updated",
                        J::object([
                            ("threadId", field(&state.thread, "id").clone()),
                            ("turnId", turn.clone().into_json()),
                            ("explanation", J::Null),
                            ("plan", J::Array(plan)),
                        ]),
                        out,
                    )
                }
                state.item_ids.insert(tool, "".into());
                return Ok(());
            }
            let item = super::items::tool_item((self.ids)(), e, field(&state.thread, "cwd"));
            let id = string(field(&item, "id"));
            state.item_ids.insert(tool, id.clone());
            state.item_started.insert(id.clone(), (self.millis)());
            self.store.append_item(turn, &item)?;
            self.item_event(state, turn, &item, true, out);
            if is(field(&item, "type"), "fileChange") {
                self.notify(
                    &state.peer,
                    "item/fileChange/patchUpdated",
                    J::object([
                        ("threadId", field(&state.thread, "id").clone()),
                        ("turnId", turn.clone().into_json()),
                        ("itemId", id.into_json()),
                        ("changes", field(&item, "changes").clone()),
                    ]),
                    out,
                )
            }
            return Ok(());
        }
        if is(field(e, "type"), "tool_result")
            && state.workflow_tools.remove(&tool)
            && state.workflow_tools.is_empty()
            && state.subagents.is_empty()
        {
            out.push(Effect::CancelWatchdog {
                turn_id: turn.clone(),
            });
        }
        let Some(id) = state
            .item_ids
            .get(&tool)
            .filter(|s| !s.0.is_empty())
            .cloned()
        else {
            return Ok(());
        };
        if is(field(e, "type"), "tool_result") && state.subagents.iter().any(|(t, _)| t == &tool) {
            return self.complete_subagent(state, turn, e, out);
        }
        if is(field(e, "type"), "tool_output_delta") {
            state.output_seen.insert(id.clone());
            let delta = string(field(e, "delta"));
            self.store.update_item(turn, &id, |mut item| {
                if is(field(&item, "type"), "commandExecution") {
                    item.insert(
                        "aggregatedOutput",
                        concat(&[
                            &string(&coalesce(field(&item, "aggregatedOutput"), &"".into())),
                            &delta,
                        ])
                        .into_json(),
                    )
                }
                Ok(item)
            })?;
            self.notify(
                &state.peer,
                "item/commandExecution/outputDelta",
                J::object([
                    ("threadId", field(&state.thread, "id").clone()),
                    ("turnId", turn.clone().into_json()),
                    ("itemId", id.into_json()),
                    ("delta", delta.into_json()),
                ]),
                out,
            );
            return Ok(());
        }
        let result = super::items::result_text(field(e, "content"));
        let duration = state
            .item_started
            .get(&id)
            .map(|started| J::Number(maximum(0.0, (self.millis)() - started)))
            .unwrap_or(J::Null);
        let failed = truthy(field(e, "isError"));
        let exit =
            super::items::exit_code(field(e, "content")).unwrap_or(if failed { 1.0 } else { 0.0 });
        let wrapped = super::items::result_wrap(field(e, "content"));
        let wrapped_error = super::items::error_wrap(field(e, "content"));
        self.store.update_item(turn, &id, |mut item| {
            if ["commandExecution", "fileChange", "mcpToolCall"]
                .iter()
                .any(|t| is(field(&item, "type"), t))
            {
                item.insert(
                    "status",
                    if failed {
                        "failed".into()
                    } else {
                        "completed".into()
                    },
                )
            }
            if is(field(&item, "type"), "commandExecution") {
                item.insert(
                    "aggregatedOutput",
                    coalesce(
                        field(&item, "aggregatedOutput"),
                        &result.clone().into_json(),
                    ),
                );
                item.insert("exitCode", J::Number(exit));
                item.insert("durationMs", duration.clone())
            }
            if is(field(&item, "type"), "mcpToolCall") {
                item.insert("result", if failed { J::Null } else { wrapped.clone() });
                item.insert(
                    "error",
                    if failed {
                        wrapped_error.clone()
                    } else {
                        J::Null
                    },
                );
                item.insert("durationMs", duration.clone())
            }
            if is(field(&item, "type"), "webSearch") {
                item.insert(
                    "action",
                    super::items::web_action(field(&item, "query"), &result),
                )
            }
            Ok(item)
        })?;
        if let Some(item) = self.item(turn, &id)? {
            if is(field(&item, "type"), "commandExecution")
                && !result.0.is_empty()
                && !state.output_seen.contains(&id)
            {
                self.notify(
                    &state.peer,
                    "item/commandExecution/outputDelta",
                    J::object([
                        ("threadId", field(&state.thread, "id").clone()),
                        ("turnId", turn.clone().into_json()),
                        ("itemId", id.into_json()),
                        ("delta", result.into_json()),
                    ]),
                    out,
                )
            }
            self.item_event(state, turn, &item, false, out)
        }
        out.push(Effect::Diff {
            turn_id: turn.clone(),
            cwd: string(field(&state.thread, "cwd")),
        });
        Ok(())
    }
    pub fn diff_result(&self, turn: &JsString, diff: JsString) -> Result<Vec<Effect>> {
        let mut out = vec![];
        let Some(state) = self.turns.get(turn) else {
            return Ok(out);
        };
        if diff.0.is_empty()
            || self.stopped
            || !self
                .store
                .get_turn(turn)?
                .is_some_and(|t| is(field(&t, "status"), "inProgress"))
        {
            return Ok(out);
        }
        self.store.update_turn_diff(turn, &diff)?;
        self.notify(
            &state.peer,
            "turn/diff/updated",
            J::object([
                ("threadId", field(&state.thread, "id").clone()),
                ("turnId", turn.clone().into_json()),
                ("diff", diff.into_json()),
            ]),
            &mut out,
        );
        Ok(out)
    }
    /// Finalize orphaned children before the original asynchronous final diff
    /// barrier. A concurrent interrupt still owns the terminal state.
    pub fn prepare_success(&mut self, turn: &JsString) -> Result<Vec<Effect>> {
        let mut out = vec![];
        let Some(mut state) = self.turns.remove(turn) else {
            return Ok(out);
        };
        let thread = string(field(&state.thread, "id"));
        if self.stopped
            || self.active.get(&thread) != Some(turn)
            || !self
                .store
                .get_turn(turn)?
                .is_some_and(|t| is(field(&t, "status"), "inProgress"))
        {
            return Ok(out);
        }
        let result = self.finalize_subagents(
            &mut state,
            turn,
            "Subagent ended without a terminal task result.",
            &mut out,
        );
        self.turns.insert(turn.clone(), state);
        result?;
        Ok(out)
    }
    pub fn runtime_finished(&mut self, turn: &JsString, result: Result<()>) -> Result<Vec<Effect>> {
        let mut out = vec![];
        let Some(mut state) = self.turns.remove(turn) else {
            return Ok(out);
        };
        let thread = string(field(&state.thread, "id"));
        if self.stopped
            || self.active.get(&thread) != Some(turn)
            || !self
                .store
                .get_turn(turn)?
                .is_some_and(|t| is(field(&t, "status"), "inProgress"))
        {
            return Ok(out);
        }
        if result.is_ok() {
            self.finalize_subagents(
                &mut state,
                turn,
                "Subagent ended without a terminal task result.",
                &mut out,
            )?;
        }
        if result.is_ok() && !field(&state.params, "outputSchema").is_null() && !state.has_text {
            let text =
                commands::structured_fallback(field(&state.params, "outputSchema"), &state.prompt)?;
            // The pinned fallback is an agent message even in plan mode.
            let plan_mode = state.plan_mode;
            state.plan_mode = false;
            self.on_event(
                &mut state,
                turn,
                &J::object([("type", "text_delta".into()), ("delta", text.into_json())]),
                &mut out,
            )?;
            state.plan_mode = plan_mode;
        }
        self.complete_message(
            &mut state,
            turn,
            if result.is_ok() {
                "final_answer"
            } else {
                "commentary"
            },
            &mut out,
        )?;
        if result.is_err() {
            self.finalize_subagents(
                &mut state,
                turn,
                "Subagent ended without a terminal task result.",
                &mut out,
            )?;
        }
        if let Some(id) = state.plan.take() {
            if let Some(item) = self.item(turn, &id)? {
                self.item_event(&state, turn, &item, false, &mut out)
            }
        }
        match result {
            Ok(()) => {
                let completed = self
                    .store
                    .complete_turn(turn, &"completed".into(), &J::Null)?
                    .unwrap();
                self.active.remove(&thread);
                self.status(
                    &state.peer,
                    &thread,
                    J::object([("type", "idle".into())]),
                    &mut out,
                )?;
                self.notify(
                    &state.peer,
                    "turn/completed",
                    J::object([
                        ("threadId", thread.into_json()),
                        ("turn", policy::turn_view(&completed, "notLoaded")),
                    ]),
                    &mut out,
                )
            }
            Err(error) => {
                let error = J::object([("message", error.message.into_json())]);
                let completed = self
                    .store
                    .complete_turn(turn, &"failed".into(), &error)?
                    .unwrap();
                self.notify(
                    &state.peer,
                    "error",
                    J::object([
                        ("threadId", thread.clone().into_json()),
                        ("turnId", turn.clone().into_json()),
                        ("willRetry", false.into()),
                        ("error", error),
                    ]),
                    &mut out,
                );
                self.notify(
                    &state.peer,
                    "turn/completed",
                    J::object([
                        ("threadId", thread.clone().into_json()),
                        ("turn", policy::turn_view(&completed, "notLoaded")),
                    ]),
                    &mut out,
                );
                self.active.remove(&thread);
                self.status(
                    &state.peer,
                    &thread,
                    J::object([("type", "idle".into())]),
                    &mut out,
                )?;
            }
        }
        Ok(out)
    }
    pub fn stop(&mut self) -> Result<()> {
        if self.stopped {
            return Ok(());
        }
        self.stopped = true;
        let mut states = std::mem::take(&mut self.turns);
        for (turn, state) in &mut states {
            if self
                .store
                .get_turn(turn)?
                .is_some_and(|t| is(field(&t, "status"), "inProgress"))
            {
                state.peer = self
                    .peers
                    .iter()
                    .find(|(t, _)| field(&state.thread, "id").as_js_string() == Some(t))
                    .map(|(_, p)| p.clone())
                    .unwrap_or_else(|| JsString::from("shutdown"));
                self.finalize_subagents(state, turn, "server stopped", &mut vec![])?
            }
        }
        for (thread, turn) in &self.active {
            self.store.complete_turn(
                turn,
                &"interrupted".into(),
                &J::object([("message", "server stopped".into())]),
            )?;
            self.store
                .update_thread_status(thread, &J::object([("type", "idle".into())]))?;
        }
        self.active.clear();
        self.turns.clear();
        Ok(())
    }
}
