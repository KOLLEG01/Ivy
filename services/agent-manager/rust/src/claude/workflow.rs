//! Workflow journal admission and projection. Filesystem trust/identity I/O is
//! an explicit Host binding; no unchecked filesystem reader is substituted.
use super::{error::Result, value::*};
use ivy_contracts::{IvyJson as J, JsString};
use ivy_host_runtime::node_path::{self, Context, Style};
use std::{collections::BTreeSet, future::Future, pin::Pin, sync::Arc};

pub type IoFuture<T> = Pin<Box<dyn Future<Output = Result<T>> + Send>>;
pub type EventSink = Arc<dyn Fn(J) -> IoFuture<()> + Send + Sync>;
#[derive(Clone)]
pub struct WorkflowFiles {
    /// Node realpath spelling; missing file/ancestor returns None. Other errors
    /// retain their original diagnostic. No account or cwd fallback is allowed.
    pub realpath: Arc<dyn Fn(JsString) -> IoFuture<Option<JsString>> + Send + Sync>,
    /// Read one regular UTF-8 file within the verified directory. The binding
    /// must preserve the original canonical-before/open-no-follow/stat/size/
    /// canonical-after/lstat/dev+ino fence and Node UTF-8 replacement behavior.
    /// ENOENT/ENOTDIR returns None. Every other failure is an error.
    pub read_regular:
        Arc<dyn Fn(JsString, JsString, usize) -> IoFuture<Option<JsString>> + Send + Sync>,
}
#[derive(Clone, Debug)]
pub struct Launch {
    pub task_id: JsString,
    pub run_id: JsString,
    pub transcript_dir: JsString,
    pub transcript_root: JsString,
    pub workflow_name: JsString,
    pub summary: JsString,
}
pub struct WorkflowPaths {
    context: Context,
    roots: Vec<JsString>,
}
impl WorkflowPaths {
    pub fn new(home: &JsString, environment: &J, cwd: &JsString) -> Self {
        let style = Style::native();
        let drive_cwds = environment
            .as_object()
            .map(|pairs| {
                pairs
                    .iter()
                    .filter_map(|(k, v)| {
                        let units = &k.0;
                        (units.len() == 3 && units[0] == 61 && units[2] == 58)
                            .then(|| {
                                v.as_js_string()
                                    .map(|v| (JsString(units[1..].to_vec()), v.clone()))
                            })
                            .flatten()
                    })
                    .collect()
            })
            .unwrap_or_default();
        let context = Context {
            style,
            cwd: cwd.clone(),
            drive_cwds,
        };
        let configured = string_value(field(environment, "CLAUDE_CONFIG_DIR"));
        let env_home = string_value(field(environment, "HOME"));
        let home = if env_home.0.is_empty() {
            home.clone()
        } else {
            env_home
        };
        let root = if configured.0.is_empty() {
            node_path::join(style, &[home, ".claude".into()])
        } else {
            configured
        };
        let root = context.resolve(&[cwd.clone(), root]);
        Self {
            context,
            roots: vec![root],
        }
    }
    pub fn parse(&self, value: &J) -> Option<Launch> {
        value.as_object()?;
        let alias = |camel: &str, snake: &str| coalesce(field(value, camel), field(value, snake));
        let kind = alias("taskType", "task_type");
        if !is(field(value, "status"), "async_launched")
            || !(kind.is_null() || is(&kind, "workflow") || is(&kind, "local_workflow"))
        {
            return None;
        }
        let task_id = string_value(&alias("taskId", "task_id"));
        let run_id = string_value(&alias("runId", "run_id"));
        let directory = string_value(&alias("transcriptDir", "transcript_dir"));
        let transcript_dir = node_path::normalize(self.context.style, &directory);
        if task_id.0.is_empty()
            || run_id.0.is_empty()
            || directory.0.is_empty()
            || !self.valid_directory(&transcript_dir, &run_id)
        {
            return None;
        }
        let mut roots = self.roots.clone();
        roots.sort_by_key(|r| std::cmp::Reverse(r.0.len()));
        let transcript_root = roots
            .into_iter()
            .find(|r| self.inside(r, &transcript_dir))?;
        Some(Launch {
            task_id,
            run_id,
            transcript_dir,
            transcript_root,
            workflow_name: string_value(&alias("workflowName", "workflow_name")),
            summary: string_value(field(value, "summary")),
        })
    }
    fn inside(&self, root: &JsString, path: &JsString) -> bool {
        let child = self.context.relative(root, path);
        let separator = if self.context.style == Style::Windows {
            92
        } else {
            47
        };
        !child.0.is_empty()
            && child.0 != [46, 46]
            && !child.0.starts_with(&[46, 46, separator])
            && !node_path::is_absolute(self.context.style, &child)
    }
    fn valid_directory(&self, path: &JsString, run: &JsString) -> bool {
        let normalized = node_path::normalize(self.context.style, path);
        if !node_path::is_absolute(self.context.style, path)
            || node_path::basename(self.context.style, &normalized) != *run
        {
            return false;
        }
        let separator = if self.context.style == Style::Windows {
            92
        } else {
            47
        };
        let parts: Vec<_> = normalized
            .0
            .split(|c| *c == separator)
            .filter(|p| !p.is_empty())
            .collect();
        let Some(index) = parts
            .iter()
            .rposition(|part| *part == "workflows".encode_utf16().collect::<Vec<_>>())
        else {
            return false;
        };
        index > 0
            && index == parts.len() - 2
            && parts[index - 1] == "subagents".encode_utf16().collect::<Vec<_>>()
    }
}
#[derive(Debug)]
pub enum JournalEvent {
    Started {
        agent_id: JsString,
        prompt: JsString,
        description: JsString,
    },
    Result {
        agent_id: JsString,
        content: JsString,
        is_error: bool,
    },
}
pub struct Journal {
    pub launch: Launch,
    files: WorkflowFiles,
    verified: Option<JsString>,
    started: Vec<JsString>,
    completed: BTreeSet<JsString>,
}
impl Journal {
    pub fn new(launch: Launch, files: WorkflowFiles) -> Self {
        Self {
            launch,
            files,
            verified: None,
            started: vec![],
            completed: BTreeSet::new(),
        }
    }
    pub fn started_count(&self) -> usize {
        self.started.len()
    }
    pub fn completed_count(&self) -> usize {
        self.completed.len()
    }
    pub fn active_ids(&self) -> Vec<JsString> {
        self.started
            .iter()
            .filter(|id| !self.completed.contains(*id))
            .cloned()
            .collect()
    }
    async fn directory(&mut self, paths: &WorkflowPaths) -> Result<Option<JsString>> {
        if let Some(value) = &self.verified {
            return Ok(Some(value.clone()));
        }
        let (root, dir) = tokio::try_join!(
            (self.files.realpath)(self.launch.transcript_root.clone()),
            (self.files.realpath)(self.launch.transcript_dir.clone())
        )?;
        let (Some(root), Some(dir)) = (root, dir) else {
            return Ok(None);
        };
        if !paths.inside(&root, &dir) || !paths.valid_directory(&dir, &self.launch.run_id) {
            return Err(super::error::AdapterError::new(
                "Workflow transcript directory is outside the trusted Claude config root",
            ));
        }
        self.verified = Some(dir.clone());
        Ok(Some(dir))
    }
    async fn read(&self, dir: &JsString, name: JsString, limit: usize) -> Result<Option<JsString>> {
        let file = node_path::join(Style::native(), &[dir.clone(), name]);
        (self.files.read_regular)(dir.clone(), file, limit).await
    }
    /// Commit each observed lifecycle only after its consumer accepted it.
    pub async fn poll(&mut self, paths: &WorkflowPaths, sink: &EventSink) -> Result<()> {
        let Some(dir) = self.directory(paths).await? else {
            return Ok(());
        };
        let Some(text) = self
            .read(&dir, "journal.jsonl".into(), 8 * 1024 * 1024)
            .await?
        else {
            return Ok(());
        };
        for entry in lines(&text) {
            let kind = field(&entry, "type");
            let id = string_value(field(&entry, "agentId"));
            if (!is(kind, "started") && !is(kind, "result")) || !valid_agent_id(&id) {
                continue;
            }
            if !self.started.contains(&id) {
                let Some(transcript) = self.read(&dir, agent_file(&id), 4 * 1024 * 1024).await?
                else {
                    continue;
                };
                let prompt = lines(&transcript).into_iter().find_map(|record| {
                    let message = field(&record, "message");
                    is(field(message, "role"), "user")
                        .then(|| message_text(field(message, "content")))
                        .filter(|s| !s.0.is_empty())
                });
                let Some(prompt) = prompt else { continue };
                let first = prompt
                    .0
                    .split(|c| *c == 10)
                    .map(|line| JsString(line.to_vec()).trim())
                    .find(|line| !line.0.is_empty())
                    .unwrap_or_else(|| concat(&[&"Workflow agent ".into(), &id]));
                let event = JournalEvent::Started {
                    agent_id: id.clone(),
                    prompt,
                    description: slice(&first, 0, 120),
                };
                self.emit(&event, sink).await?;
                self.accept(&event);
            }
            if !is(kind, "result") || self.completed.contains(&id) {
                continue;
            }
            let result = field(&entry, "result");
            let status = string(&coalesce(field(result, "status"), &"".into()))
                .trim()
                .to_english_lowercase();
            let failed = truthy(field(result, "error"))
                || ["error", "errored", "failed", "failure"]
                    .iter()
                    .any(|s| status == JsString::from(*s));
            let mut body = result_text(result)?;
            if body.0.is_empty() {
                if let Some(transcript) = self.read(&dir, agent_file(&id), 4 * 1024 * 1024).await? {
                    for record in lines(&transcript) {
                        let message = field(&record, "message");
                        if !is(field(message, "role"), "assistant") {
                            continue;
                        }
                        let text = message_text(field(message, "content"));
                        if !text.0.is_empty() {
                            body = text
                        }
                    }
                }
            }
            if body.0.is_empty() {
                continue;
            }
            let event = JournalEvent::Result {
                agent_id: id.clone(),
                content: concat(&[&body, &"\nagentId: ".into(), &id]),
                is_error: failed,
            };
            self.emit(&event, sink).await?;
            self.accept(&event);
        }
        Ok(())
    }
    async fn emit(&self, event: &JournalEvent, sink: &EventSink) -> Result<()> {
        let (id, body) = match event {
            JournalEvent::Started {
                agent_id,
                prompt,
                description,
            } => (
                agent_id,
                J::object([
                    ("type", "tool_use".into()),
                    ("toolName", "Agent".into()),
                    (
                        "input",
                        J::object([
                            ("description", description.clone().into_json()),
                            ("prompt", prompt.clone().into_json()),
                            ("subagent_type", "workflow".into()),
                        ]),
                    ),
                ]),
            ),
            JournalEvent::Result {
                agent_id,
                content,
                is_error,
            } => (
                agent_id,
                J::object([
                    ("type", "tool_result".into()),
                    ("content", content.clone().into_json()),
                    ("isError", (*is_error).into()),
                ]),
            ),
        };
        let mut body = body;
        body.insert(
            "toolUseId",
            concat(&[
                &"workflow-agent:".into(),
                &self.launch.task_id,
                &":".into(),
                id,
            ])
            .into_json(),
        );
        (sink)(body).await
    }
    pub fn accept(&mut self, event: &JournalEvent) {
        match event {
            JournalEvent::Started { agent_id, .. } => {
                if !self.started.contains(agent_id) {
                    self.started.push(agent_id.clone())
                }
            }
            JournalEvent::Result { agent_id, .. } => {
                self.completed.insert(agent_id.clone());
            }
        }
    }
}
fn agent_file(id: &JsString) -> JsString {
    concat(&[&"agent-".into(), id, &".jsonl".into()])
}
fn valid_agent_id(id: &JsString) -> bool {
    (8..=32).contains(&id.0.len()) && id.0.iter().all(|c| matches!(c,48..=57|65..=70|97..=102))
}
fn lines(text: &JsString) -> Vec<J> {
    text.0
        .split(|c| *c == 10)
        .filter_map(|line| {
            let line = JsString(line.to_vec()).trim();
            if line.0.is_empty() {
                return None;
            }
            decode(String::from_utf16_lossy(&line.0).as_bytes())
                .ok()
                .filter(|v| v.as_object().is_some())
        })
        .collect()
}
fn string_value(v: &J) -> JsString {
    v.as_js_string().map(JsString::trim).unwrap_or_default()
}
fn message_text(value: &J) -> JsString {
    if let Some(s) = value.as_js_string() {
        return s.trim();
    }
    let Some(blocks) = value.as_array() else {
        return JsString::default();
    };
    join(
        &blocks
            .iter()
            .filter_map(|b| is(field(b, "type"), "text").then(|| string_value(field(b, "text"))))
            .filter(|s| !s.0.is_empty())
            .collect::<Vec<_>>(),
        "\n",
    )
    .trim()
}
fn result_text(value: &J) -> Result<JsString> {
    if let Some(s) = value.as_js_string() {
        return Ok(s.trim());
    }
    if value.is_null() {
        return Ok(JsString::default());
    }
    // Reuse the exact compact JSON codec for numbers, UTF-16 and key ordering;
    // add only JSON.stringify's two-space presentation outside string tokens.
    let compact = encode(value)?;
    let bytes = compact.as_bytes();
    let mut out = String::new();
    let mut depth = 0;
    let mut quoted = false;
    let mut escaped = false;
    for (index, c) in compact.char_indices() {
        if quoted {
            out.push(c);
            if escaped {
                escaped = false
            } else if c == '\\' {
                escaped = true
            } else if c == '"' {
                quoted = false
            }
            continue;
        }
        match c {
            '"' => {
                quoted = true;
                out.push(c)
            }
            '{' | '[' => {
                out.push(c);
                depth += 1;
                if bytes.get(index + 1) != Some(&(if c == '{' { b'}' } else { b']' })) {
                    out.push('\n');
                    out.push_str(&"  ".repeat(depth));
                }
            }
            '}' | ']' => {
                depth -= 1;
                if index > 0 && bytes[index - 1] != (if c == '}' { b'{' } else { b'[' }) {
                    out.push('\n');
                    out.push_str(&"  ".repeat(depth));
                }
                out.push(c)
            }
            ',' => {
                out.push_str(",\n");
                out.push_str(&"  ".repeat(depth));
            }
            ':' => out.push_str(": "),
            _ => out.push(c),
        }
    }
    Ok(out.into())
}
