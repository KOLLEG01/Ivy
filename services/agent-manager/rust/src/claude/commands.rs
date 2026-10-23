//! Local workflow commands and the pinned empty structured-output fallback.
use super::{error::Result, value::*};
use ivy_contracts::{IvyJson as J, JsString};
use std::collections::BTreeMap;

pub enum WorkflowCommand {
    List,
    Run(JsString),
}
fn whitespace(unit: u16) -> bool {
    JsString(vec![unit]).trim().0.is_empty()
}
pub fn workflow_command(prompt: &JsString) -> Option<WorkflowCommand> {
    let start = prompt
        .0
        .iter()
        .position(|c| !whitespace(*c))
        .unwrap_or(prompt.0.len());
    let prefix = JsString::from("/workflows");
    let rest = prompt.0[start..].strip_prefix(prefix.0.as_slice())?;
    if rest.first().is_some_and(|c| !whitespace(*c)) {
        return None;
    }
    let task = JsString(rest.to_vec()).trim();
    Some(if task.0.is_empty() {
        WorkflowCommand::List
    } else {
        WorkflowCommand::Run(task)
    })
}
pub fn collapse_whitespace(text: &JsString) -> JsString {
    let mut out = vec![];
    let mut space = false;
    for unit in &text.0 {
        if whitespace(*unit) {
            if !space {
                out.push(32)
            }
            space = true
        } else {
            out.push(*unit);
            space = false
        }
    }
    JsString(out)
}
fn strip_list_prefix(text: JsString) -> JsString {
    let start = text
        .0
        .iter()
        .position(|c| !matches!(*c, 45 | 42 | 48..=57 | 46 | 0x3001 | 41) && !whitespace(*c))
        .unwrap_or(text.0.len());
    slice(&text, start, text.0.len())
}
fn concise(prompt: &JsString) -> JsString {
    let source = prompt
        .0
        .split(|c| *c == 10)
        .map(|line| JsString(line.to_vec()).trim())
        .filter(|line| !line.0.is_empty())
        .next_back()
        .unwrap_or_else(|| prompt.trim());
    let value = match source.0.iter().rposition(|c| *c == 58 || *c == 0xff1a) {
        Some(index) => slice(&source, index + 1, source.0.len()).trim(),
        None => source,
    };
    slice(&strip_list_prefix(value), 0, 80).trim()
}
fn fallback_value(schema: &J, prompt: &JsString) -> J {
    if schema.as_object().is_none() {
        return prompt.clone().into_json();
    }
    if is(field(schema, "type"), "string") {
        return concise(prompt).into_json();
    }
    if is(field(schema, "type"), "array") {
        let default = J::object([("type", "string".into())]);
        let item = if field(schema, "items").as_object().is_some() {
            field(schema, "items")
        } else {
            &default
        };
        let mut values: Vec<_> = prompt
            .0
            .split(|c| *c == 10)
            .map(|line| strip_list_prefix(JsString(line.to_vec()).trim()))
            .filter(|line| !line.0.is_empty())
            .collect();
        if values.is_empty() && !prompt.trim().0.is_empty() {
            values.push(prompt.trim())
        }
        return J::Array(
            values
                .iter()
                .take(10)
                .map(|value| fallback_value(item, value))
                .collect(),
        );
    }
    if !is(field(schema, "type"), "object") {
        return J::Null;
    }
    let properties = field(schema, "properties").as_object();
    let required: Vec<_> = if let Some(required) = field(schema, "required").as_array() {
        required
            .iter()
            .filter_map(J::as_js_string)
            .cloned()
            .collect()
    } else {
        properties
            .map(|p| p.iter().map(|(key, _)| key.clone()).collect())
            .unwrap_or_default()
    };
    let mut result = BTreeMap::new();
    for key in required {
        let property = properties
            .and_then(|p| p.iter().find(|(k, _)| k == &key).map(|(_, v)| v))
            .unwrap_or(&NULL);
        result.insert(key, fallback_value(property, prompt));
    }
    J::object(result)
}
pub fn structured_fallback(schema: &J, prompt: &JsString) -> Result<JsString> {
    Ok(encode(&fallback_value(schema, prompt))?.into())
}

impl super::protocol::Adapter {
    pub(super) fn complete_workflow_list(
        &mut self,
        turn: &JsString,
    ) -> Result<Vec<super::protocol::Effect>> {
        let mut out = vec![];
        let Some(state) = self.turns.remove(turn) else {
            return Ok(out);
        };
        let thread = string(field(&state.thread, "id"));
        let id = (self.ids)();
        let mut item = J::object([
            ("type", "agentMessage".into()),
            ("id", id.clone().into_json()),
            ("text", "".into()),
            ("phase", J::Null),
            ("memoryCitation", J::Null),
        ]);
        self.store.append_item(turn, &item)?;
        self.item_event(&state, turn, &item, true, &mut out);
        let text = self.workflow_list_text(&thread, turn)?;
        item.insert("text", text.clone().into_json());
        self.store.update_item(turn, &id, |_| Ok(item.clone()))?;
        self.notify(
            &state.peer,
            "item/agentMessage/delta",
            J::object([
                ("threadId", thread.clone().into_json()),
                ("turnId", turn.clone().into_json()),
                ("itemId", id.into_json()),
                ("delta", text.into_json()),
            ]),
            &mut out,
        );
        self.item_event(&state, turn, &item, false, &mut out);
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
                ("turn", super::policy::turn_view(&completed, "notLoaded")),
            ]),
            &mut out,
        );
        Ok(out)
    }
    fn workflow_list_text(&self, thread: &JsString, current: &JsString) -> Result<JsString> {
        let mut runs = vec![];
        for turn in self.store.list_turns(thread)? {
            if field(&turn, "id").as_js_string() == Some(current) {
                continue;
            }
            let user = field(&turn, "items")
                .as_array()
                .unwrap_or(&[])
                .iter()
                .find(|item| is(field(item, "type"), "userMessage"));
            let prompt = user
                .map(|item| super::policy::text_input(field(item, "content")))
                .unwrap_or_default();
            let Some(WorkflowCommand::Run(prompt)) = workflow_command(&prompt) else {
                continue;
            };
            let mut ids = vec![];
            for item in field(&turn, "items").as_array().unwrap_or(&[]) {
                if !is(field(item, "type"), "collabAgentToolCall")
                    || !is(field(item, "tool"), "spawnAgent")
                {
                    continue;
                }
                for id in field(item, "receiverThreadIds").as_array().unwrap_or(&[]) {
                    let id = string(id);
                    if !ids.contains(&id) {
                        ids.push(id)
                    }
                }
            }
            let mut agents = vec![];
            for id in ids {
                let child = self.store.get_thread(&id)?.unwrap_or(J::Null);
                let turns = self.store.list_turns(&id)?;
                let status = turns.last().map(|t| field(t, "status")).unwrap_or(&NULL);
                agents.push(concat(&[
                    &string(&coalesce(
                        field(&child, "agentNickname"),
                        &slice(&id, 0, 12).into_json(),
                    )),
                    &" (".into(),
                    &string(&coalesce(field(&child, "agentRole"), &"subagent".into())),
                    &", ".into(),
                    &string(&coalesce(
                        status,
                        &coalesce(field(field(&child, "status"), "type"), &"unknown".into()),
                    )),
                    &")".into(),
                ]));
            }
            runs.push((turn, prompt, agents));
        }
        if runs.is_empty() {
            return Ok("No workflow runs in this task.\n\nStart one with `/workflows <task>`. The adapter runs it through Claude ultracode and exposes its agents as reviewable Codex subagent tasks.".into());
        }
        let mut lines = vec![JsString::from("Workflow runs in this task:")];
        for (turn, prompt, agents) in runs.into_iter().rev().take(20) {
            lines.push(concat(&[
                &"- ".into(),
                &slice(&string(field(&turn, "id")), 0, 12),
                &" · ".into(),
                &string(field(&turn, "status")),
                &" · ".into(),
                &JsString::from(agents.len().to_string()),
                &" agent(s) · ".into(),
                &slice(&collapse_whitespace(&prompt), 0, 120),
            ]));
            if !agents.is_empty() {
                lines.push(concat(&[&"  Agents: ".into(), &join(&agents, ", ")]))
            }
        }
        lines.push("".into());
        lines.push("Open the Agent items in the workflow turn to review each child task.".into());
        Ok(join(&lines, "\n"))
    }
}
