//! Capability-neutral child history and the pinned spawn/wait lifecycle.
use super::{
    error::{AdapterError, Result},
    policy,
    protocol::{Adapter, Effect, TurnState},
    value::*,
};
use ivy_contracts::{IvyJson as J, JsString};
#[derive(Clone)]
pub(super) struct Child {
    thread: JsString,
    turn: JsString,
    wait: JsString,
    path: JsString,
    prompt: JsString,
}
impl Adapter {
    fn completed_activity(&self, peer: &JsString) -> bool {
        self.env
            .subagent_completed
            .unwrap_or_else(|| self.features.get(peer).copied().unwrap_or(false))
    }
    fn child_item_event(
        &self,
        peer: &JsString,
        thread: &JsString,
        turn: &JsString,
        item: &J,
        start: bool,
        out: &mut Vec<Effect>,
    ) {
        let mut params = J::object([
            ("threadId", thread.clone().into_json()),
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
            peer,
            if start {
                "item/started"
            } else {
                "item/completed"
            },
            params,
            out,
        );
    }
    fn activity(
        &self,
        state: &TurnState,
        turn: &JsString,
        child: &Child,
        kind: &str,
        out: &mut Vec<Effect>,
    ) -> Result<()> {
        if !self.completed_activity(&state.peer) && kind != "interrupted" {
            return Ok(());
        }
        let item = J::object([
            ("type", "subAgentActivity".into()),
            ("id", (self.ids)().into_json()),
            ("kind", kind.into()),
            ("agentThreadId", child.thread.clone().into_json()),
            ("agentPath", child.path.clone().into_json()),
        ]);
        if kind == "interrupted" {
            self.store.append_item(turn, &item)?;
        }
        self.item_event(state, turn, &item, true, out);
        self.item_event(state, turn, &item, false, out);
        Ok(())
    }
    fn collab(
        &self,
        id: JsString,
        tool: &str,
        status: &str,
        state: &TurnState,
        receivers: Vec<J>,
        prompt: J,
        model: J,
        effort: J,
        agents: J,
    ) -> J {
        J::object([
            ("type", "collabAgentToolCall".into()),
            ("id", id.into_json()),
            ("tool", tool.into()),
            ("status", status.into()),
            ("senderThreadId", field(&state.thread, "id").clone()),
            ("receiverThreadIds", J::Array(receivers)),
            ("prompt", prompt),
            ("model", model),
            ("reasoningEffort", effort),
            ("agentsStates", agents),
        ])
    }
    pub(super) fn spawn_subagent(
        &mut self,
        state: &mut TurnState,
        turn: &JsString,
        e: &J,
        out: &mut Vec<Effect>,
    ) -> Result<()> {
        let input = field(e, "input");
        let prompt = string(&coalesce(
            field(input, "prompt"),
            &coalesce(field(input, "description"), &"".into()),
        ));
        let subtype = if field(input, "subagent_type").as_js_string().is_some() {
            field(input, "subagent_type").clone()
        } else {
            J::Null
        };
        let model = if field(input, "model").as_js_string().is_some() {
            field(input, "model").clone()
        } else {
            field(&state.thread, "model").clone()
        };
        let child_id = (self.ids)();
        let nickname = concat(&[
            &"agent-".into(),
            &slice(
                &JsString(child_id.0.iter().copied().filter(|u| *u != 45).collect()),
                0,
                12,
            ),
        ]);
        let path = concat(&[&"/root/".into(), &nickname]);
        let now = (self.clock)();
        let mut thread = J::object([
            ("id", child_id.clone().into_json()),
            ("sessionId", field(&state.thread, "sessionId").clone()),
            ("forkedFromId", field(&state.thread, "id").clone()),
            ("isPinned", false.into()),
            ("sectionId", J::Null),
            ("sectionEnteredAt", J::Null),
            ("sectionPosition", J::Null),
            ("preview", slice(&prompt, 0, 200).into_json()),
            ("name", J::Null),
            ("archived", false.into()),
            ("cwd", field(&state.thread, "cwd").clone()),
            ("model", model.clone()),
            (
                "reasoningEffort",
                field(&state.thread, "reasoningEffort").clone(),
            ),
            (
                "modelProvider",
                field(&state.thread, "modelProvider").clone(),
            ),
            ("claudeSessionId", J::Null),
            (
                "source",
                policy::session_source(field(&state.thread, "source")),
            ),
            ("createdAt", now.into_json()),
            ("updatedAt", now.into_json()),
            (
                "status",
                J::object([("type", "active".into()), ("activeFlags", J::Array(vec![]))]),
            ),
            ("ephemeral", true.into()),
            ("threadSource", "subagent".into()),
            ("agentRole", coalesce(&subtype, &"general-purpose".into())),
            ("agentNickname", nickname.into_json()),
            ("runtimeBackend", "claude".into()),
            ("codexSessionId", J::Null),
        ]);
        for key in [
            "approvalPolicy",
            "sandboxMode",
            "baseInstructions",
            "developerInstructions",
            "personality",
        ] {
            thread.insert(key, field(&state.thread, key).clone())
        }
        self.store.upsert_thread(&thread)?;
        self.bind(child_id.clone(), state.peer.clone());
        let child_turn = (self.ids)();
        let user = J::object([
            ("type", "userMessage".into()),
            ("id", (self.ids)().into_json()),
            (
                "content",
                J::Array(vec![J::object([
                    ("type", "text".into()),
                    ("text", prompt.clone().into_json()),
                    ("text_elements", J::Array(vec![])),
                ])]),
            ),
        ]);
        let child_record = J::object([
            ("id", child_turn.clone().into_json()),
            ("threadId", child_id.clone().into_json()),
            ("status", "inProgress".into()),
            ("startedAt", now.into_json()),
            ("completedAt", J::Null),
            ("durationMs", J::Null),
            ("items", J::Array(vec![user.clone()])),
            ("diff", "".into()),
            ("error", J::Null),
        ]);
        self.store.upsert_turn(&child_record)?;
        self.notify(
            &state.peer,
            "thread/started",
            J::object([("thread", self.to_thread(&thread, &[])?)]),
            out,
        );
        self.notify(
            &state.peer,
            "turn/started",
            J::object([
                ("threadId", child_id.clone().into_json()),
                ("turn", policy::turn_view(&child_record, "notLoaded")),
            ]),
            out,
        );
        self.child_item_event(&state.peer, &child_id, &child_turn, &user, true, out);
        self.child_item_event(&state.peer, &child_id, &child_turn, &user, false, out);
        let spawn_id = (self.ids)();
        let effort = policy::collab_effort(field(&state.thread, "reasoningEffort"));
        let prompt_value = if prompt.0.is_empty() {
            J::Null
        } else {
            prompt.clone().into_json()
        };
        let spawn = self.collab(
            spawn_id.clone(),
            "spawnAgent",
            "inProgress",
            state,
            vec![],
            prompt_value.clone(),
            model.clone(),
            effort.clone(),
            object(),
        );
        self.store.append_item(turn, &spawn)?;
        self.item_event(state, turn, &spawn, true, out);
        let spawned = self.collab(
            spawn_id.clone(),
            "spawnAgent",
            "completed",
            state,
            vec![child_id.clone().into_json()],
            prompt_value,
            model,
            effort,
            J::object([(
                child_id.clone(),
                J::object([("status", "running".into()), ("message", J::Null)]),
            )]),
        );
        self.store
            .update_item(turn, &spawn_id, |_| Ok(spawned.clone()))?;
        self.item_event(state, turn, &spawned, false, out);
        let wait = (self.ids)();
        let child = Child {
            thread: child_id.clone(),
            turn: child_turn,
            wait: wait.clone(),
            path,
            prompt,
        };
        self.activity(state, turn, &child, "started", out)?;
        let waiting = self.collab(
            wait.clone(),
            "wait",
            "inProgress",
            state,
            vec![child_id.into_json()],
            J::Null,
            J::Null,
            J::Null,
            object(),
        );
        self.store.append_item(turn, &waiting)?;
        self.item_event(state, turn, &waiting, true, out);
        state.item_ids.insert(string(field(e, "toolUseId")), wait);
        let first = state.subagents.is_empty();
        state.subagents.push((string(field(e, "toolUseId")), child));
        if first {
            if let Some(timeout_ms) = self.env.subagent_watchdog_ms.filter(|ms| *ms > 0) {
                out.push(Effect::Watchdog {
                    turn_id: turn.clone(),
                    timeout_ms,
                })
            }
        }
        Ok(())
    }
    fn child_complete(
        &mut self,
        state: &TurnState,
        child: &Child,
        text: JsString,
        status: &str,
        error: &J,
        duration: Option<f64>,
        out: &mut Vec<Effect>,
    ) -> Result<J> {
        let turn = if let Some(turn) = self.store.get_turn(&child.turn)? {
            turn
        } else {
            let turn = J::object([
                ("id", child.turn.clone().into_json()),
                ("threadId", child.thread.clone().into_json()),
                ("status", "inProgress".into()),
                ("startedAt", (self.clock)().into_json()),
                ("completedAt", J::Null),
                ("durationMs", J::Null),
                (
                    "items",
                    J::Array(vec![J::object([
                        ("type", "userMessage".into()),
                        ("id", (self.ids)().into_json()),
                        (
                            "content",
                            J::Array(vec![J::object([
                                ("type", "text".into()),
                                ("text", child.prompt.clone().into_json()),
                                ("text_elements", J::Array(vec![])),
                            ])]),
                        ),
                    ])]),
                ),
                ("diff", "".into()),
                ("error", J::Null),
            ]);
            self.store.upsert_turn(&turn)?;
            turn
        };
        let id = (self.ids)();
        let mut item = J::object([
            ("type", "agentMessage".into()),
            ("id", id.clone().into_json()),
            ("text", "".into()),
            ("phase", J::Null),
            ("memoryCitation", J::Null),
        ]);
        self.store.append_item(&child.turn, &item)?;
        self.child_item_event(&state.peer, &child.thread, &child.turn, &item, true, out);
        item.insert("text", text.clone().into_json());
        self.store
            .update_item(&child.turn, &id, |_| Ok(item.clone()))?;
        if !text.0.is_empty() {
            self.notify(
                &state.peer,
                "item/agentMessage/delta",
                J::object([
                    ("threadId", child.thread.clone().into_json()),
                    ("turnId", child.turn.clone().into_json()),
                    ("itemId", id.into_json()),
                    ("delta", text.into_json()),
                ]),
                out,
            )
        }
        self.child_item_event(&state.peer, &child.thread, &child.turn, &item, false, out);
        let mut completed = self
            .store
            .complete_turn(&child.turn, &status.into(), error)?
            .unwrap_or(turn);
        if let Some(duration) = duration {
            completed.insert("durationMs", J::Number(duration));
            self.store.upsert_turn(&completed)?
        }
        self.status(
            &state.peer,
            &child.thread,
            J::object([("type", "idle".into())]),
            out,
        )?;
        let last = if status == "completed" && error.is_null() {
            field(&completed, "items")
                .as_array()
                .unwrap_or(&[])
                .iter()
                .rev()
                .find(|i| {
                    is(field(i, "type"), "agentMessage")
                        && !string(field(i, "text")).trim().0.is_empty()
                })
                .cloned()
        } else {
            None
        };
        let mut public = policy::turn_view(&completed, "notLoaded");
        if let Some(last) = last {
            public.insert("items", J::Array(vec![last]));
            public.insert("itemsView", "summary".into())
        }
        self.notify(
            &state.peer,
            "turn/completed",
            J::object([
                ("threadId", child.thread.clone().into_json()),
                ("turn", public),
            ]),
            out,
        );
        Ok(completed)
    }
    pub(super) fn complete_subagent(
        &mut self,
        state: &mut TurnState,
        turn: &JsString,
        e: &J,
        out: &mut Vec<Effect>,
    ) -> Result<()> {
        let tool = string(field(e, "toolUseId"));
        let Some(index) = state.subagents.iter().position(|(t, _)| *t == tool) else {
            return Ok(());
        };
        let child = state.subagents[index].1.clone();
        let trailer = parse_trailer(&super::items::result_text(field(e, "content")));
        let failed = truthy(field(e, "isError"));
        let status = if failed { "failed" } else { "completed" };
        let completed = self.child_complete(
            state,
            &child,
            trailer.text,
            status,
            &if failed {
                J::object([("message", "subagent failed".into())])
            } else {
                J::Null
            },
            trailer.usage.map(|u| u.2),
            out,
        )?;
        if let Some(mut thread) = self.store.get_thread(&child.thread)? {
            if let Some(id) = trailer.agent {
                thread.insert("agentNickname", id.into_json())
            }
            self.store.upsert_thread(&thread)?
        }
        if failed || self.completed_activity(&state.peer) {
            self.activity(
                state,
                turn,
                &child,
                if failed { "interrupted" } else { "completed" },
                out,
            )?
        }
        if let Some((total, _, _)) = trailer.usage {
            if total != 0.0 {
                let breakdown = J::object([
                    ("totalTokens", J::Number(total)),
                    ("inputTokens", J::Number(0.0)),
                    ("cachedInputTokens", J::Number(0.0)),
                    ("outputTokens", J::Number(total)),
                    ("reasoningOutputTokens", J::Number(0.0)),
                ]);
                self.notify(
                    &state.peer,
                    "thread/tokenUsage/updated",
                    J::object([
                        ("threadId", child.thread.clone().into_json()),
                        ("turnId", field(&completed, "id").clone()),
                        (
                            "tokenUsage",
                            J::object([
                                ("total", breakdown.clone()),
                                ("last", breakdown),
                                ("modelContextWindow", J::Null),
                            ]),
                        ),
                    ]),
                    out,
                );
                self.token_usage(
                    state,
                    turn,
                    &J::object([
                        ("input_tokens", J::Number(0.0)),
                        ("output_tokens", J::Number(total)),
                        ("cache_read_input_tokens", J::Number(0.0)),
                        ("cache_creation_input_tokens", J::Number(0.0)),
                    ]),
                    out,
                )
            }
        }
        let agents = J::object([(
            child.thread.clone(),
            J::object([
                (
                    "status",
                    if failed {
                        "errored".into()
                    } else {
                        "completed".into()
                    },
                ),
                ("message", J::Null),
            ]),
        )]);
        let wait = self.collab(
            child.wait.clone(),
            "wait",
            status,
            state,
            vec![child.thread.clone().into_json()],
            J::Null,
            J::Null,
            J::Null,
            agents.clone(),
        );
        self.store
            .update_item_and_move_to_end(turn, &child.wait, |_| Ok(wait.clone()))?;
        self.item_event(state, turn, &wait, false, out);
        if failed && !self.completed_activity(&state.peer) {
            self.close_child(state, turn, &child, status, agents, out)?
        }
        state.subagents.remove(index);
        if state.subagents.is_empty() && state.workflow_tools.is_empty() {
            out.push(Effect::CancelWatchdog {
                turn_id: turn.clone(),
            })
        }
        Ok(())
    }
    fn close_child(
        &self,
        state: &TurnState,
        turn: &JsString,
        child: &Child,
        status: &str,
        agents: J,
        out: &mut Vec<Effect>,
    ) -> Result<()> {
        let id = (self.ids)();
        let mut item = self.collab(
            id.clone(),
            "closeAgent",
            "inProgress",
            state,
            vec![child.thread.clone().into_json()],
            J::Null,
            J::Null,
            J::Null,
            object(),
        );
        self.store.append_item(turn, &item)?;
        self.item_event(state, turn, &item, true, out);
        item.insert("status", status.into());
        item.insert("agentsStates", agents);
        self.store.update_item(turn, &id, |_| Ok(item.clone()))?;
        self.item_event(state, turn, &item, false, out);
        Ok(())
    }
    pub(super) fn finalize_subagents(
        &mut self,
        state: &mut TurnState,
        turn: &JsString,
        message: &str,
        out: &mut Vec<Effect>,
    ) -> Result<()> {
        let children = std::mem::take(&mut state.subagents);
        for (_, child) in children {
            self.child_complete(
                state,
                &child,
                message.into(),
                "failed",
                &J::object([("message", message.into())]),
                None,
                out,
            )?;
            self.activity(state, turn, &child, "interrupted", out)?;
            let agents = J::object([(
                child.thread.clone(),
                J::object([("status", "errored".into()), ("message", message.into())]),
            )]);
            let wait = self.collab(
                child.wait.clone(),
                "wait",
                "failed",
                state,
                vec![child.thread.clone().into_json()],
                J::Null,
                J::Null,
                J::Null,
                agents.clone(),
            );
            self.store
                .update_item_and_move_to_end(turn, &child.wait, |_| Ok(wait.clone()))?;
            self.item_event(state, turn, &wait, false, out);
            if !self.completed_activity(&state.peer) {
                self.close_child(state, turn, &child, "failed", agents, out)?
            }
        }
        out.push(Effect::CancelWatchdog {
            turn_id: turn.clone(),
        });
        Ok(())
    }
    pub fn watchdog_elapsed(&mut self, turn: &JsString) -> Result<Vec<Effect>> {
        let mut out = vec![];
        let Some(mut state) = self.turns.remove(turn) else {
            return Ok(out);
        };
        if state.subagents.is_empty() && state.workflow_tools.is_empty() {
            self.turns.insert(turn.clone(), state);
            return Ok(out);
        }
        let seconds = self.env.subagent_watchdog_ms.unwrap_or(0).div_ceil(1000);
        let message = format!(
            "Subagent did not publish a terminal result within {seconds} {}.",
            if seconds == 1 { "second" } else { "seconds" }
        );
        self.complete_message(&mut state, turn, "commentary", &mut out)?;
        self.finalize_subagents(&mut state, turn, &message, &mut out)?;
        self.turns.insert(turn.clone(), state);
        out.extend(self.runtime_finished(turn, Err(AdapterError::new(message)))?);
        Ok(out)
    }
}
struct Trailer {
    text: JsString,
    agent: Option<JsString>,
    usage: Option<(f64, f64, f64)>,
}
fn trim_end(v: &JsString) -> JsString {
    let mut end = v.0.len();
    while end > 0 && JsString(vec![v.0[end - 1]]).trim().0.is_empty() {
        end -= 1
    }
    slice(v, 0, end)
}
fn parse_trailer(value: &JsString) -> Trailer {
    let mut text = value.clone();
    let mut usage = None;
    let trimmed = trim_end(&text);
    let begin = JsString::from("<usage>");
    let end = JsString::from("</usage>");
    if trimmed.0.ends_with(&end.0) {
        if let Some(index) = trimmed.0.windows(begin.0.len()).rposition(|v| v == begin.0) {
            let body = slice(
                &trimmed,
                index + begin.0.len(),
                trimmed.0.len() - end.0.len(),
            );
            let mut rest = body.0.as_slice();
            let mut numbers = vec![];
            let mut valid = true;
            for (label, last) in [
                ("total_tokens:", false),
                ("tool_uses:", false),
                ("duration_ms:", true),
            ] {
                while rest
                    .first()
                    .is_some_and(|u| JsString(vec![*u]).trim().0.is_empty())
                {
                    rest = &rest[1..]
                }
                let prefix = JsString::from(label);
                if !rest.starts_with(&prefix.0) {
                    valid = false;
                    break;
                }
                rest = &rest[prefix.0.len()..];
                while rest
                    .first()
                    .is_some_and(|u| JsString(vec![*u]).trim().0.is_empty())
                {
                    rest = &rest[1..]
                }
                let length = rest.iter().take_while(|u| (48..=57).contains(*u)).count();
                if length == 0 {
                    valid = false;
                    break;
                }
                numbers.push(number(&J::String(JsString(rest[..length].to_vec()))));
                rest = &rest[length..];
                let whitespace = rest
                    .iter()
                    .take_while(|u| JsString(vec![**u]).trim().0.is_empty())
                    .count();
                if !last && !rest[..whitespace].contains(&10) {
                    valid = false;
                    break;
                }
                rest = &rest[whitespace..];
            }
            if valid && rest.is_empty() && numbers.len() == 3 {
                usage = Some((numbers[0], numbers[1], numbers[2]));
                text = trim_end(&slice(&trimmed, 0, index))
            }
        }
    }
    let lower = text.to_english_lowercase();
    let prefix = JsString::from("agentid:");
    let mut agent = None;
    for index in (0..lower.0.len()).rev() {
        if !lower.0[index..].starts_with(&prefix.0) {
            continue;
        }
        let mut at = index + prefix.0.len();
        while text
            .0
            .get(at)
            .is_some_and(|u| JsString(vec![*u]).trim().0.is_empty())
        {
            at += 1
        }
        let length = lower.0[at..]
            .iter()
            .take_while(|u| (48..=57).contains(*u) || (97..=102).contains(*u))
            .count();
        if !(8..=32).contains(&length) {
            continue;
        }
        let stop = at + length;
        if lower
            .0
            .get(stop)
            .is_some_and(|u| (48..=57).contains(u) || (97..=122).contains(u) || *u == 95)
        {
            continue;
        }
        let tail = &text.0[stop..];
        if let Some(newline) = tail.iter().position(|u| *u == 10) {
            if !JsString(tail[newline..].to_vec()).trim().0.is_empty() {
                continue;
            }
        }
        agent = Some(slice(&text, at, stop));
        let cut = if index > 0 && text.0[index - 1] == 10 {
            index - 1
        } else {
            index
        };
        text = trim_end(&slice(&text, 0, cut));
        break;
    }
    Trailer { text, agent, usage }
}
