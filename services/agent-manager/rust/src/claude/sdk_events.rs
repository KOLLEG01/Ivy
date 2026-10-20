//! Rust translation of SDK 0.3.283 messages. The SDK bridge sends only raw
//! messages; response dedup, subagent suppression and completion stay here.
use super::{
    error::{AdapterError, Result},
    value::*,
    workflow::{EventSink, Journal, Launch, WorkflowFiles, WorkflowPaths},
};
use ivy_contracts::{IvyJson as J, JsString};
use std::collections::BTreeSet;
use tokio::time::{Duration, Instant};

struct Streamed {
    index: f64,
    kind: &'static str,
    text: JsString,
}
struct Deferred {
    success: bool,
    text: J,
    session: J,
}
struct Task {
    id: JsString,
    tool: JsString,
    name: JsString,
    description: JsString,
    prompt: JsString,
    monitor: Option<Journal>,
    aggregate: bool,
    terminal: bool,
    monitor_failed: bool,
}
pub struct SdkTurn {
    context: J,
    sink: EventSink,
    files: WorkflowFiles,
    paths: WorkflowPaths,
    assistant: Option<JsString>,
    streamed: Vec<Streamed>,
    active: Vec<JsString>,
    completed: BTreeSet<JsString>,
    workflow_tools: BTreeSet<JsString>,
    launches: Vec<(JsString, Launch)>,
    skipped: BTreeSet<JsString>,
    tasks: Vec<Task>,
    structured: JsString,
    deferred: Option<Deferred>,
    failure: Option<JsString>,
    outcome: Option<Result<()>>,
}
fn text(value: &J) -> JsString {
    string(&coalesce(value, &"".into()))
}
fn nonempty(value: &J) -> Option<JsString> {
    value.as_js_string().filter(|s| !s.0.is_empty()).cloned()
}
fn task_tool(id: &JsString) -> JsString {
    concat(&[&"workflow-task:".into(), id])
}
fn agent_tool(task: &JsString, agent: &JsString) -> JsString {
    concat(&[&"workflow-agent:".into(), task, &":".into(), agent])
}
fn scalar(value: &J) -> J {
    if value.as_f64().is_some_and(f64::is_finite) {
        value.clone()
    } else {
        J::Null
    }
}
impl SdkTurn {
    pub fn new(
        context: J,
        home: &JsString,
        environment: &J,
        files: WorkflowFiles,
        sink: EventSink,
    ) -> Self {
        let paths = WorkflowPaths::new(home, environment, &string(field(&context, "cwd")));
        Self {
            context,
            sink,
            files,
            paths,
            assistant: None,
            streamed: vec![],
            active: vec![],
            completed: BTreeSet::new(),
            workflow_tools: BTreeSet::new(),
            launches: vec![],
            skipped: BTreeSet::new(),
            tasks: vec![],
            structured: JsString::default(),
            deferred: None,
            failure: None,
            outcome: None,
        }
    }
    pub fn is_finished(&self) -> bool {
        self.outcome.is_some()
    }
    pub fn outcome(&self) -> Option<Result<()>> {
        self.outcome.clone()
    }
    async fn emit(&self, event: J) -> Result<()> {
        (self.sink)(event).await
    }
    fn structured(&self) -> bool {
        truthy(field(&self.context, "outputFormat"))
    }
    fn suppressed(&self) -> bool {
        !self.active.is_empty()
            || self.tasks.iter().any(|task| {
                task.monitor
                    .as_ref()
                    .is_some_and(|j| !j.active_ids().is_empty())
            })
    }
    fn add_active(&mut self, id: JsString) {
        if !self.active.contains(&id) {
            self.active.push(id)
        }
    }
    fn remove_active(&mut self, id: &JsString) -> bool {
        if let Some(index) = self.active.iter().position(|v| v == id) {
            self.active.remove(index);
            true
        } else {
            false
        }
    }
    pub async fn message(&mut self, message: &J) -> Result<()> {
        if self.is_finished() {
            return Ok(());
        }
        match field(message, "type").as_str().ok().as_deref() {
            Some("system") => self.system(message).await,
            Some("stream_event") => self.stream(message).await,
            Some("assistant") => self.assistant(message).await,
            Some("user") => self.user(message).await,
            Some("result") => self.result(message).await,
            _ => self.other(message).await,
        }
    }
    async fn boundary(&mut self, id: Option<JsString>, stream: bool) -> Result<()> {
        if id
            .as_ref()
            .map(|id| Some(id) == self.assistant.as_ref())
            .unwrap_or(!stream)
        {
            return Ok(());
        }
        self.assistant = id;
        self.streamed.clear();
        if !self.structured() {
            self.emit(J::object([("type", "message_boundary".into())]))
                .await?
        }
        Ok(())
    }
    fn record(&mut self, index: &J, kind: &'static str, text: JsString) {
        let index = index.as_f64().unwrap_or(0.0);
        if let Some(block) = self
            .streamed
            .iter_mut()
            .find(|b| b.index == index || b.index.is_nan() && index.is_nan())
        {
            if block.kind == kind {
                block.text.0.extend(text.0)
            } else {
                block.kind = kind;
                block.text = text
            }
        } else {
            self.streamed.push(Streamed { index, kind, text })
        }
    }
    fn suffix(&mut self, kind: &str, text: JsString) -> JsString {
        if let Some(index) = self
            .streamed
            .iter()
            .position(|b| b.kind == kind && text.0.starts_with(&b.text.0))
        {
            let block = self.streamed.remove(index);
            slice(&text, block.text.0.len(), text.0.len())
        } else {
            text
        }
    }
    async fn delta(&mut self, kind: &str, delta: JsString) -> Result<()> {
        if kind == "text_delta" && self.structured() {
            self.structured.0.extend(delta.0);
            return Ok(());
        }
        if !delta.0.is_empty() {
            self.emit(J::object([
                ("type", kind.into()),
                ("delta", delta.into_json()),
            ]))
            .await?
        }
        Ok(())
    }
    async fn stream(&mut self, message: &J) -> Result<()> {
        let event = field(message, "event");
        if event.is_null() || truthy(field(message, "parent_tool_use_id")) || self.suppressed() {
            return Ok(());
        }
        if is(field(event, "type"), "message_start") {
            return self
                .boundary(nonempty(field(field(event, "message"), "id")), true)
                .await;
        }
        // Tool starts carry empty input. Only completed assistant blocks create
        // tools; content_block_start has no native effect in the pinned runtime.
        if !is(field(event, "type"), "content_block_delta") {
            return Ok(());
        }
        let delta = field(event, "delta");
        let (kind, key, event_kind) = if is(field(delta, "type"), "text_delta") {
            ("text", "text", "text_delta")
        } else if is(field(delta, "type"), "thinking_delta") {
            ("thinking", "thinking", "reasoning_delta")
        } else {
            return Ok(());
        };
        let value = text(field(delta, key));
        if value.0.is_empty() {
            return Ok(());
        }
        self.record(field(event, "index"), kind, value.clone());
        self.delta(event_kind, value).await
    }
    async fn assistant(&mut self, message: &J) -> Result<()> {
        let inner = field(message, "message");
        if inner.is_null() {
            return Ok(());
        }
        let nested = truthy(field(message, "parent_tool_use_id"));
        if !nested && !self.suppressed() {
            self.boundary(nonempty(field(inner, "id")), false).await?
        }
        for block in field(inner, "content").as_array().unwrap_or(&[]) {
            if is(field(block, "type"), "text") || is(field(block, "type"), "thinking") {
                if nested || self.suppressed() {
                    continue;
                }
                let thinking = is(field(block, "type"), "thinking");
                let kind = if thinking { "thinking" } else { "text" };
                let delta = self.suffix(
                    kind,
                    text(field(block, if thinking { "thinking" } else { "text" })),
                );
                self.delta(
                    if thinking {
                        "reasoning_delta"
                    } else {
                        "text_delta"
                    },
                    delta,
                )
                .await?;
            } else if is(field(block, "type"), "tool_use") {
                let id = text(field(block, "id"));
                if id.0.is_empty() {
                    continue;
                }
                let name = text(field(block, "name"));
                let input = if truthy(field(block, "input")) {
                    field(block, "input").clone()
                } else {
                    object()
                };
                let parent = nested || self.suppressed();
                let subagent = super::policy::is_subagent(&name.clone().into_json());
                if subagent {
                    self.add_active(id.clone())
                }
                if parent && !subagent
                    || name == JsString::from("StructuredOutput")
                    || name == JsString::from("AskUserQuestion")
                {
                    continue;
                }
                self.emit(J::object([
                    ("type", "tool_use".into()),
                    ("toolUseId", id.clone().into_json()),
                    ("toolName", name.clone().into_json()),
                    ("input", input),
                ]))
                .await?;
                if name == JsString::from("Workflow") {
                    self.workflow_tools.insert(id);
                }
            }
        }
        Ok(())
    }
    async fn user(&mut self, message: &J) -> Result<()> {
        let inner = field(message, "message");
        if inner.is_null() {
            return Ok(());
        }
        let content = field(inner, "content").as_array().unwrap_or(&[]);
        let count = content
            .iter()
            .filter(|b| is(field(b, "type"), "tool_result"))
            .count();
        let mut attached = false;
        for block in content {
            if !is(field(block, "type"), "tool_result") {
                continue;
            }
            let id = text(field(block, "tool_use_id"));
            if id.0.is_empty() {
                continue;
            }
            let workflow = self.workflow_tools.remove(&id);
            self.remove_active(&id);
            let mut event = J::object([
                ("type", "tool_result".into()),
                ("toolUseId", id.clone().into_json()),
                ("isError", truthy(field(block, "is_error")).into()),
            ]);
            if let Some(value) = block.get("content") {
                event.insert("content", value.clone())
            }
            self.emit(event).await?;
            if !attached && workflow && count == 1 {
                attached = true;
                let value = coalesce(
                    field(message, "tool_use_result"),
                    field(message, "toolUseResult"),
                );
                if let Some(launch) = self.paths.parse(&value) {
                    self.attach_launch(id, launch).await?
                }
            }
        }
        Ok(())
    }
    async fn result(&mut self, message: &J) -> Result<()> {
        if self.deferred.is_some() || self.is_finished() {
            return Ok(());
        }
        let success = is(field(message, "subtype"), "success")
            && !truthy(field(message, "is_error"))
            && self.failure.is_none();
        let result = self
            .failure
            .clone()
            .map(JsonScalar::into_json)
            .unwrap_or_else(|| {
                if field(message, "result").is_null() {
                    J::Null
                } else {
                    string(field(message, "result")).into_json()
                }
            });
        let session = if field(message, "session_id").is_null() {
            J::Null
        } else {
            string(field(message, "session_id")).into_json()
        };
        let usage = field(message, "usage");
        if usage.as_object().is_some_and(|o| !o.is_empty())
            || usage.as_array().is_some_and(|a| !a.is_empty())
        {
            self.emit(J::object([
                ("type", "usage".into()),
                ("usage", usage.clone()),
            ]))
            .await?;
        }
        self.emit(J::object([
            ("type", "metrics".into()),
            ("durationMs", scalar(field(message, "duration_ms"))),
            ("apiDurationMs", scalar(field(message, "duration_api_ms"))),
            ("numTurns", scalar(field(message, "num_turns"))),
            ("costUsd", scalar(field(message, "total_cost_usd"))),
        ]))
        .await?;
        if self.structured() && !self.structured.0.is_empty() {
            self.emit(J::object([
                ("type", "text_delta".into()),
                ("delta", self.structured.trim().into_json()),
            ]))
            .await?
        }
        self.deferred = Some(Deferred {
            success,
            text: result,
            session,
        });
        if !success {
            self.stop_workflows().await?
        }
        self.finish(false).await
    }
    async fn other(&self, message: &J) -> Result<()> {
        let kind = field(message, "type");
        if is(kind, "rate_limit") || is(kind, "rate_limit_event") {
            let info = field(message, "rate_limit_info");
            if truthy(info) {
                self.emit(J::object([
                    ("type", "rate_limit".into()),
                    ("info", info.clone()),
                ]))
                .await?
            }
            if is(field(info, "status"), "allowed") {
                return Ok(());
            }
            let explicit = nonempty(field(message, "message"));
            if explicit.is_none()
                && !is(field(info, "status"), "allowed_warning")
                && !is(field(info, "status"), "rejected")
                && is(kind, "rate_limit_event")
            {
                return Ok(());
            }
            let notice = explicit.unwrap_or_else(|| rate_notice(info));
            return self
                .emit(J::object([
                    ("type", "notice".into()),
                    ("level", "warning".into()),
                    ("message", notice.into_json()),
                ]))
                .await;
        }
        if ["hook", "hook_event", "system_hook_event"]
            .iter()
            .any(|s| is(kind, s))
        {
            self.emit(J::object([
                ("type", "hook".into()),
                (
                    "hookName",
                    string(&coalesce(
                        field(message, "hook_event_name"),
                        &coalesce(field(message, "hook_name"), &"hook".into()),
                    ))
                    .into_json(),
                ),
                (
                    "status",
                    nonempty(field(message, "status"))
                        .or_else(|| nonempty(field(message, "subtype")))
                        .map(JsonScalar::into_json)
                        .unwrap_or(J::Null),
                ),
                (
                    "decision",
                    nonempty(field(message, "decision"))
                        .or_else(|| nonempty(field(message, "permission_decision")))
                        .map(JsonScalar::into_json)
                        .unwrap_or(J::Null),
                ),
                (
                    "message",
                    nonempty(field(message, "message"))
                        .or_else(|| nonempty(field(message, "reason")))
                        .map(JsonScalar::into_json)
                        .unwrap_or(J::Null),
                ),
            ]))
            .await?;
        }
        Ok(())
    }
    fn task(&mut self, id: &JsString) -> usize {
        if let Some(index) = self.tasks.iter().position(|t| t.id == *id) {
            return index;
        }
        self.tasks.push(Task {
            id: id.clone(),
            tool: JsString::default(),
            name: JsString::default(),
            description: JsString::default(),
            prompt: JsString::default(),
            monitor: None,
            aggregate: false,
            terminal: false,
            monitor_failed: false,
        });
        self.tasks.len() - 1
    }
    fn describe(&mut self, index: usize, message: &J) {
        let task = &mut self.tasks[index];
        let source = text(field(message, "tool_use_id"));
        if !source.0.is_empty() && task.tool.0.is_empty() {
            task.tool = source
        }
        let name = text(field(message, "workflow_name")).trim();
        if !name.0.is_empty() {
            task.name = name
        }
        let description = text(field(message, "description")).trim();
        if !description.0.is_empty() {
            task.description = description
        }
        let prompt = text(field(message, "prompt")).trim();
        if !prompt.0.is_empty() {
            task.prompt = prompt
        } else if task.prompt.0.is_empty() {
            task.prompt = if !task.description.0.is_empty() {
                task.description.clone()
            } else {
                task.name.clone()
            }
        }
    }
    async fn attach_launch(&mut self, tool: JsString, launch: Launch) -> Result<()> {
        if self.skipped.contains(&launch.task_id) {
            return Ok(());
        }
        if let Some(index) = self.tasks.iter().position(|t| t.id == launch.task_id) {
            return self.attach(index, tool, launch).await;
        }
        if self.tasks.iter().any(|t| t.tool == tool) {
            return Ok(());
        }
        if let Some((_, value)) = self.launches.iter_mut().find(|(id, _)| *id == tool) {
            *value = launch
        } else {
            self.launches.push((tool, launch))
        };
        Ok(())
    }
    async fn attach(&mut self, index: usize, tool: JsString, launch: Launch) -> Result<()> {
        let state = &mut self.tasks[index];
        if launch.task_id != state.id || !state.tool.0.is_empty() && state.tool != tool {
            return Ok(());
        }
        if state.tool.0.is_empty() {
            state.tool = tool
        }
        if !launch.workflow_name.0.is_empty() {
            state.name = launch.workflow_name.clone()
        }
        if !launch.summary.0.is_empty() {
            state.description = launch.summary.clone()
        }
        if state.prompt.0.is_empty() {
            state.prompt = if !state.description.0.is_empty() {
                state.description.clone()
            } else {
                state.name.clone()
            }
        }
        if state.terminal || state.aggregate || state.monitor.is_some() {
            return Ok(());
        }
        state.monitor = Some(Journal::new(launch, self.files.clone()));
        self.flush(index).await
    }
    async fn attach_deferred(&mut self, index: usize, source: &JsString) -> Result<()> {
        let found = if !source.0.is_empty() {
            self.launches.iter().position(|(id, _)| id == source)
        } else {
            let matching: Vec<_> = self
                .launches
                .iter()
                .enumerate()
                .filter(|(_, (_, l))| l.task_id == self.tasks[index].id)
                .map(|(i, _)| i)
                .collect();
            if matching.len() == 1 {
                Some(matching[0])
            } else {
                None
            }
        };
        if let Some(found) = found {
            let (tool, launch) = self.launches.remove(found);
            self.attach(index, tool, launch).await?
        }
        Ok(())
    }
    fn discard(&mut self, id: &JsString, tool: &JsString) {
        self.launches.retain(|(k, l)| k != tool && l.task_id != *id)
    }
    fn projected(&self, index: usize) -> Vec<JsString> {
        let state = &self.tasks[index];
        let mut ids = state
            .monitor
            .as_ref()
            .map(Journal::active_ids)
            .unwrap_or_default();
        let prefix = agent_tool(&state.id, &JsString::default());
        for id in &self.active {
            if id.0.starts_with(&prefix.0) {
                let id = slice(id, prefix.0.len(), id.0.len());
                if !id.0.is_empty() && !ids.contains(&id) {
                    ids.push(id)
                }
            }
        }
        ids
    }
    async fn close_agents(
        &mut self,
        index: usize,
        ids: Vec<JsString>,
        message: &JsString,
        error: bool,
    ) -> Result<()> {
        for id in ids {
            let tool = agent_tool(&self.tasks[index].id, &id);
            self.remove_active(&tool);
            self.completed.insert(tool.clone());
            self.emit(J::object([
                ("type", "tool_result".into()),
                ("toolUseId", tool.into_json()),
                (
                    "content",
                    concat(&[message, &"\nagentId: ".into(), &id]).into_json(),
                ),
                ("isError", error.into()),
            ]))
            .await?;
        }
        Ok(())
    }
    async fn close_aggregate(
        &mut self,
        index: usize,
        message: &JsString,
        error: bool,
    ) -> Result<()> {
        let tool = task_tool(&self.tasks[index].id);
        if !self.remove_active(&tool) {
            return Ok(());
        }
        self.completed.insert(tool.clone());
        self.emit(J::object([
            ("type", "tool_result".into()),
            ("toolUseId", tool.into_json()),
            ("content", message.clone().into_json()),
            ("isError", error.into()),
        ]))
        .await
    }
    async fn aggregate(&mut self, index: usize) -> Result<()> {
        let state = &mut self.tasks[index];
        let tool = task_tool(&state.id);
        if state.terminal || state.aggregate || self.completed.contains(&tool) {
            return Ok(());
        }
        state.aggregate = true;
        let event = J::object([
            ("type", "tool_use".into()),
            ("toolUseId", tool.clone().into_json()),
            ("toolName", "Agent".into()),
            (
                "input",
                J::object([
                    (
                        "description",
                        if state.name.0.is_empty() {
                            "Workflow".into()
                        } else {
                            concat(&[&"Workflow: ".into(), &state.name]).into_json()
                        },
                    ),
                    (
                        "prompt",
                        [&state.prompt, &state.description, &state.name]
                            .into_iter()
                            .find(|s| !s.0.is_empty())
                            .cloned()
                            .unwrap_or_else(|| "Workflow".into())
                            .into_json(),
                    ),
                    ("subagent_type", "workflow".into()),
                ]),
            ),
        ]);
        self.add_active(tool.clone());
        if let Err(e) = self.emit(event).await {
            self.tasks[index].aggregate = false;
            self.remove_active(&tool);
            return Err(e);
        }
        Ok(())
    }
    async fn flush(&mut self, index: usize) -> Result<()> {
        if self.tasks[index].terminal {
            return Ok(());
        }
        let Some(monitor) = self.tasks[index].monitor.as_mut() else {
            return Ok(());
        };
        let result = monitor.poll(&self.paths, &self.sink).await;
        if let Err(error) = result {
            let ids = monitor.active_ids();
            self.tasks[index].monitor = None;
            self.tasks[index].monitor_failed = true;
            self.tasks[index].terminal = true;
            let message = concat(&[&"Workflow monitoring failed: ".into(), &error.message]);
            if self.failure.is_none() {
                self.failure = Some(message.clone())
            }
            self.close_agents(index, ids, &message, true).await?;
            self.close_aggregate(index, &message, true).await?;
            self.workflow_tools.remove(&self.tasks[index].tool);
            self.emit(J::object([
                ("type", "notice".into()),
                ("level", "warning".into()),
                ("message", message.clone().into_json()),
            ]))
            .await?;
            if let Some(deferred) = &mut self.deferred {
                self.completed.insert(task_tool(&self.tasks[index].id));
                deferred.success = false;
                if deferred.text.is_null() {
                    deferred.text = message.into_json()
                }
            }
            self.finish(true).await?;
        }
        Ok(())
    }
    pub async fn poll(&mut self) -> Result<()> {
        if self.is_finished() {
            return Ok(());
        }
        for index in 0..self.tasks.len() {
            self.flush(index).await?
        }
        if self.failure.is_some() {
            self.finish(true).await?
        }
        Ok(())
    }
    async fn hidden(&mut self, index: usize) -> Result<()> {
        let ids = self.projected(index);
        self.tasks[index].terminal = true;
        self.tasks[index].monitor = None;
        self.close_agents(
            index,
            ids,
            &"Workflow transcript was hidden before the agent completed.".into(),
            false,
        )
        .await?;
        self.close_aggregate(index, &"Workflow transcript was hidden.".into(), false)
            .await?;
        self.finish(false).await
    }
    async fn system(&mut self, message: &J) -> Result<()> {
        let subtype = field(message, "subtype");
        if is(subtype, "init") {
            let id = text(field(message, "session_id"));
            if !id.0.is_empty() {
                self.emit(J::object([
                    ("type", "session".into()),
                    ("claudeSessionId", id.into_json()),
                ]))
                .await?
            }
            return Ok(());
        }
        if is(subtype, "permission_denied") {
            return self
                .emit(J::object([
                    ("type", "notice".into()),
                    ("level", "warning".into()),
                    (
                        "message",
                        concat(&[
                            &"Permission denied for ".into(),
                            &string(&coalesce(field(message, "tool_name"), &"tool".into())),
                        ])
                        .into_json(),
                    ),
                ]))
                .await;
        }
        let id = text(field(message, "task_id"));
        if id.0.is_empty() {
            return Ok(());
        }
        let source = text(field(message, "tool_use_id"));
        if is(subtype, "task_started") && is(field(message, "task_type"), "local_workflow") {
            if field(message, "skip_transcript").as_bool() == Some(true) {
                self.skipped.insert(id.clone());
                self.discard(&id, &source);
                if let Some(index) = self.tasks.iter().position(|t| t.id == id) {
                    self.hidden(index).await?
                } else {
                    self.finish(false).await?
                }
                return Ok(());
            }
            if self.completed.contains(&task_tool(&id)) {
                return Ok(());
            }
            let index = self.task(&id);
            self.describe(index, message);
            self.attach_deferred(index, &source).await?;
            return Ok(());
        }
        if !is(subtype, "task_notification") {
            return Ok(());
        }
        let known = self.tasks.iter().position(|t| t.id == id);
        let hint = is(field(message, "task_type"), "local_workflow")
            || !text(field(message, "workflow_name")).trim().0.is_empty();
        if known.is_none()
            && !hint
            && !self.launches.iter().any(|(_, l)| l.task_id == id)
            && !self.workflow_tools.contains(&source)
        {
            return Ok(());
        }
        if self.skipped.contains(&id) {
            self.discard(&id, &JsString::default());
            if let Some(index) = known {
                self.hidden(index).await?
            } else {
                self.finish(false).await?
            }
            return Ok(());
        }
        let index = if let Some(index) = known {
            index
        } else {
            let index = self.task(&id);
            self.describe(index, message);
            self.attach_deferred(index, &source).await?;
            index
        };
        if field(message, "skip_transcript").as_bool() == Some(true) {
            self.skipped.insert(id.clone());
            let tool = self.tasks[index].tool.clone();
            self.discard(&id, &tool);
            return self.hidden(index).await;
        }
        let tool = task_tool(&id);
        if self.completed.contains(&tool) {
            self.tasks[index].terminal = true;
            return self.finish(false).await;
        }
        let status = text(field(message, "status"));
        let summary = text(field(message, "summary")).trim();
        let summary = if summary.0.is_empty() {
            concat(&[
                &"Workflow ".into(),
                &if status.0.is_empty() {
                    "finished".into()
                } else {
                    status.clone()
                },
            ])
        } else {
            summary
        };
        let trailer = usage_trailer(field(message, "usage"));
        if self.tasks[index].monitor.is_some()
            && !self.tasks[index].aggregate
            && !self.tasks[index].monitor_failed
        {
            // Polling is owned by this turn. No separate timer or stale monitor
            // callback survives a terminal signal or iterator cancellation.
            let _ = tokio::time::timeout(Duration::from_millis(500), self.flush(index)).await;
            let deadline = Instant::now() + Duration::from_millis(3000);
            let mut stable = Instant::now();
            let mut counts = (0, 0);
            while self.tasks[index].monitor.is_some()
                && !self.tasks[index].monitor_failed
                && Instant::now() <= deadline
            {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if tokio::time::timeout(remaining, self.flush(index))
                    .await
                    .is_err()
                {
                    break;
                }
                let Some(monitor) = &self.tasks[index].monitor else {
                    break;
                };
                let current = (monitor.started_count(), monitor.completed_count());
                if current != counts {
                    counts = current;
                    stable = Instant::now()
                }
                if current.0 > 0
                    && current.1 >= current.0
                    && stable.elapsed() >= Duration::from_millis(500)
                {
                    break;
                }
                if Instant::now() >= deadline {
                    break;
                }
                tokio::time::sleep(
                    Duration::from_millis(100)
                        .min(deadline.saturating_duration_since(Instant::now())),
                )
                .await;
            }
            if let Some(monitor) = self.tasks[index].monitor.take() {
                let ids = monitor.active_ids();
                let count = monitor.started_count();
                if count > 0 || !ids.is_empty() {
                    self.tasks[index].terminal = true;
                    let prefix = if status == JsString::from("completed") {
                        "Workflow completed before the individual transcript result became visible.\n"
                    } else {
                        "Workflow ended before the agent published an individual result.\n"
                    };
                    let reason = concat(&[&prefix.into(), &summary]);
                    self.close_agents(index, ids, &reason, status != JsString::from("completed"))
                        .await?;
                    self.completed.insert(tool);
                    let name = if self.tasks[index].name.0.is_empty() {
                        concat(&[&"Workflow ".into(), &id])
                    } else {
                        self.tasks[index].name.clone()
                    };
                    self.emit(J::object([
                        ("type", "notice".into()),
                        (
                            "level",
                            if status == JsString::from("completed") {
                                "info".into()
                            } else {
                                "warning".into()
                            },
                        ),
                        (
                            "message",
                            concat(&[&name, &": ".into(), &summary, &trailer]).into_json(),
                        ),
                    ]))
                    .await?;
                    return self.finish(false).await;
                }
            }
        }
        self.aggregate(index).await?;
        if !self.remove_active(&tool) {
            return Ok(());
        }
        self.completed.insert(tool.clone());
        self.tasks[index].terminal = true;
        self.emit(J::object([
            ("type", "tool_result".into()),
            ("toolUseId", tool.into_json()),
            ("content", concat(&[&summary, &trailer]).into_json()),
            ("isError", (status != JsString::from("completed")).into()),
        ]))
        .await?;
        self.finish(false).await
    }
    fn pending_workflows(&self) -> bool {
        !self.workflow_tools.is_empty()
            || !self.launches.is_empty()
            || self.tasks.iter().any(|t| !t.terminal)
    }
    async fn stop_workflows(&mut self) -> Result<()> {
        for index in 0..self.tasks.len() {
            let ids = self.projected(index);
            self.tasks[index].terminal = true;
            self.tasks[index].monitor = None;
            self.close_agents(
                index,
                ids,
                &"Workflow monitoring stopped before the agent published an individual result."
                    .into(),
                true,
            )
            .await?;
            self.close_aggregate(
                index,
                &"Workflow monitoring stopped before the workflow published a terminal result."
                    .into(),
                true,
            )
            .await?;
        }
        self.launches.clear();
        Ok(())
    }
    async fn finish(&mut self, force: bool) -> Result<()> {
        if self.is_finished() || self.deferred.is_none() {
            return Ok(());
        }
        if !force && self.deferred.as_ref().unwrap().success && self.pending_workflows() {
            return Ok(());
        }
        let value = self.deferred.take().unwrap();
        let outcome = if value.success {
            Ok(())
        } else {
            Err(AdapterError::new(if value.text.is_null() {
                "Claude turn failed".into()
            } else {
                string(&value.text)
            }))
        };
        // Set the lifetime fence before awaiting the final consumer callback.
        self.outcome = Some(outcome.clone());
        if let Err(error) = self
            .emit(J::object([
                ("type", "completed".into()),
                ("success", value.success.into()),
                ("result", value.text),
                ("claudeSessionId", value.session),
            ]))
            .await
        {
            self.outcome = Some(Err(error))
        }
        Ok(())
    }
    pub async fn end(&mut self, error: Option<AdapterError>) -> Result<()> {
        if self.is_finished() {
            return Ok(());
        }
        if let Some(error) = error {
            let cleanup = self.stop_workflows().await;
            self.outcome = Some(Err(error.clone()));
            let _ = self
                .emit(J::object([
                    ("type", "error".into()),
                    ("message", error.message.into_json()),
                ]))
                .await;
            cleanup?;
            return Ok(());
        }
        if self.deferred.is_some() {
            if self.pending_workflows() {
                self.stop_workflows().await?
            }
            self.finish(true).await?;
            if self.is_finished() {
                return Ok(());
            }
        }
        self.stop_workflows().await?;
        self.outcome = Some(Ok(()));
        if let Err(error) = self
            .emit(J::object([
                ("type", "completed".into()),
                ("success", true.into()),
                ("result", J::Null),
            ]))
            .await
        {
            self.outcome = Some(Err(error))
        }
        Ok(())
    }
}
fn usage_trailer(value: &J) -> JsString {
    if value.as_object().is_none() {
        return JsString::default();
    }
    let values = [
        field(value, "total_tokens"),
        field(value, "tool_uses"),
        field(value, "duration_ms"),
    ];
    if values
        .iter()
        .any(|v| v.as_f64().is_none_or(|n| !n.is_finite()))
    {
        return JsString::default();
    }
    concat(&[
        &"\n<usage>total_tokens: ".into(),
        &string(values[0]),
        &"\ntool_uses: ".into(),
        &string(values[1]),
        &"\nduration_ms: ".into(),
        &string(values[2]),
        &"</usage>".into(),
    ])
}
fn rate_notice(info: &J) -> JsString {
    let window = JsString(
        text(&coalesce(field(info, "rateLimitType"), &"usage".into()))
            .0
            .into_iter()
            .map(|c| if c == 95 { 32 } else { c })
            .collect(),
    );
    let status = if is(field(info, "status"), "allowed_warning") {
        concat(&[
            &"Claude usage is nearing the ".into(),
            &window,
            &" limit".into(),
        ])
    } else {
        concat(&[&"Claude ".into(), &window, &" limit reached".into()])
    };
    let used = field(info, "utilization")
        .as_f64()
        .map(|n| {
            concat(&[
                &" (".into(),
                &string(&J::Number(round(n * 100.0))),
                &"% used)".into(),
            ])
        })
        .unwrap_or_default();
    let reset = field(info, "resetsAt")
        .as_f64()
        .and_then(|n| {
            let ms = n * 1000.0;
            if !ms.is_finite() || ms.abs() > 8_640_000_000_000_000.0 {
                return None;
            }
            chrono::DateTime::from_timestamp_millis(ms.trunc() as i64).map(|date| {
                let year = chrono::Datelike::year(&date);
                let value = if (0..=9999).contains(&year) {
                    date.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
                } else {
                    format!("{year:+07}{}", date.format("-%m-%dT%H:%M:%S%.3fZ"))
                };
                JsString::from(format!(" Resets at {value}."))
            })
        })
        .unwrap_or_default();
    concat(&[&status, &used, &".".into(), &reset])
}
