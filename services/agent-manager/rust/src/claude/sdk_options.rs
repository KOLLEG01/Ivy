//! SDK options and prompt envelopes derived from the pinned adapter. The
//! executing-account home and complete admitted environment are explicit.
use super::{
    error::{AdapterError, Result},
    value::*,
};
use ivy_contracts::{IvyJson as J, JsString};
use ivy_host_runtime::node_path;
use std::{collections::BTreeSet, fs};
pub struct SdkEnvironment {
    pub home: JsString,
    pub environment: J,
}
pub struct QueryOptions {
    pub params: J,
    pub auto_allow: bool,
}
impl SdkEnvironment {
    fn env(&self, key: &str) -> &J {
        field(&self.environment, key)
    }
    fn nonempty(&self, key: &str) -> Option<JsString> {
        self.env(key)
            .as_js_string()
            .filter(|s| !s.0.is_empty())
            .cloned()
    }
    pub fn assert_subscription(&self, cwd: &JsString) -> Result<()> {
        if !is(self.env("CLAUDE_CODEX_SUBSCRIPTION_ONLY"), "1") {
            return Ok(());
        }
        let paid = [
            "ANTHROPIC_API_KEY",
            "ANTHROPIC_AUTH_TOKEN",
            "ANTHROPIC_BASE_URL",
            "CLAUDE_CODE_USE_BEDROCK",
            "CLAUDE_CODE_USE_VERTEX",
            "CLAUDE_CODE_USE_FOUNDRY",
        ];
        let conflict = || {
            AdapterError::new(
                "Claude subscription mode conflicts with API or cloud gateway environment settings",
            )
        };
        let config = self.nonempty("CLAUDE_CONFIG_DIR").ok_or_else(conflict)?;
        if paid.iter().any(|key| self.nonempty(key).is_some()) {
            return Err(conflict());
        }
        let config = node_path::filesystem_path(&config);
        let cwd = node_path::filesystem_path(cwd);
        for path in [
            config.join("settings.json"),
            config.join("settings.local.json"),
            cwd.join(".claude/settings.json"),
            cwd.join(".claude/settings.local.json"),
        ] {
            if !path.exists() {
                continue;
            }
            let parsed = decode(&fs::read(path)?)?;
            if parsed.as_object().is_none() {
                return Err(AdapterError::new("Claude settings must contain an object"));
            }
            let env = field(&parsed, "env");
            if paid.iter().any(|key| env.get(key).is_some())
                || parsed.get("apiKeyHelper").is_some()
                || is(field(&parsed, "forceLoginMethod"), "console")
            {
                return Err(AdapterError::new(
                    "Claude subscription mode conflicts with API or cloud gateway settings",
                ));
            }
        }
        Ok(())
    }
    pub fn discovery(&self, cwd: &JsString, timeout_ms: u64) -> Result<J> {
        self.assert_subscription(cwd)?;
        let mut options = J::object([
            ("cwd", cwd.clone().into_json()),
            (
                "settingSources",
                J::Array(["user", "project", "local"].map(J::from).to_vec()),
            ),
            ("persistSession", false.into()),
            ("tools", J::Array(vec![])),
            ("mcpServers", object()),
        ]);
        self.cli(&mut options)?;
        Ok(J::object([
            ("options", options),
            ("timeoutMs", J::Number(timeout_ms as f64)),
        ]))
    }
    fn cli(&self, options: &mut J) -> Result<()> {
        let path = self.nonempty("CLAUDE_CODEX_CLI").ok_or_else(|| {
            AdapterError::new("SDK bridge requires the admitted Claude executable")
        })?;
        options.insert("pathToClaudeCodeExecutable", path.into_json());
        Ok(())
    }
    pub fn query(&self, context: &J) -> Result<QueryOptions> {
        let cwd = string(field(context, "cwd"));
        self.assert_subscription(&cwd)?;
        let mut options = J::object([
            (
                "settingSources",
                J::Array(["user", "project", "local"].map(J::from).to_vec()),
            ),
            ("includePartialMessages", true.into()),
            ("includeHookEvents", true.into()),
            ("cwd", cwd.clone().into_json()),
        ]);
        for key in ["model", "effort", "outputFormat"] {
            if nonempty_string(field(context, key)) || field(context, key).as_object().is_some() {
                options.insert(key, field(context, key).clone())
            }
        }
        if let Some(resume) = self.resume(field(context, "claudeSessionId"), Some(&cwd)) {
            options.insert("resume", resume.into_json());
            if field(context, "forkSession").as_bool().unwrap_or(false) {
                options.insert("forkSession", true.into())
            }
        }
        for (source, target) in [
            ("addDirs", "additionalDirectories"),
            ("allowedTools", "allowedTools"),
        ] {
            if field(context, source)
                .as_array()
                .is_some_and(|a| !a.is_empty())
            {
                options.insert(target, field(context, source).clone())
            }
        }
        if !field(context, "mcpServers").is_null()
            && (field(context, "mcpServers").as_object().is_some()
                || field(context, "mcpServers").as_array().is_some())
        {
            options.insert("mcpServers", field(context, "mcpServers").clone())
        }
        let override_mode = self.env("CLAUDE_CODEX_PERMISSION_MODE");
        let override_mode = if [
            "default",
            "acceptEdits",
            "bypassPermissions",
            "plan",
            "dontAsk",
            "auto",
        ]
        .iter()
        .any(|m| is(override_mode, m))
        {
            override_mode.clone()
        } else {
            J::Null
        };
        let mode = if !override_mode.is_null() {
            override_mode.clone()
        } else if field(context, "planMode").as_bool().unwrap_or(false) {
            "plan".into()
        } else if is(field(context, "sandboxMode"), "danger-full-access")
            || is(field(context, "approvalPolicy"), "never")
        {
            "default".into()
        } else if is(field(context, "approvalPolicy"), "on-failure") {
            "acceptEdits".into()
        } else {
            "default".into()
        };
        options.insert("permissionMode", mode.clone());
        if is(&mode, "bypassPermissions") {
            options.insert("allowDangerouslySkipPermissions", true.into())
        }
        let prompt = workflow_prompt(&string(field(context, "prompt")));
        if prompt != string(field(context, "prompt")) {
            options.insert(
                "settings",
                J::object([
                    ("enableWorkflows", true.into()),
                    ("workflowKeywordTriggerEnabled", true.into()),
                ]),
            )
        }
        let callback = !is(&mode, "plan") && !is(&mode, "bypassPermissions");
        let auto_allow = (override_mode.is_null()
            && (is(field(context, "approvalPolicy"), "never")
                || is(field(context, "sandboxMode"), "danger-full-access")))
            || is(&mode, "dontAsk");
        let managed = self
            .nonempty("CLAUDE_CODEX_INSTRUCTIONS_FILE")
            .map(|p| node_path::filesystem_path(&p))
            .filter(|p| p.exists())
            .map(fs::read)
            .transpose()?
            .map(|b| JsString::from(String::from_utf8_lossy(&b).into_owned()))
            .unwrap_or_else(|| "".into());
        let lines = managed.0.split(|u| *u == 10).collect::<Vec<_>>();
        let local = lines.iter().any(|l| {
            let prefix = JsString::from("@AGENTS.md");
            l.starts_with(&prefix.0) && JsString(l[prefix.0.len()..].to_vec()).trim().0.is_empty()
        });
        let mut stripped = vec![];
        for line in lines {
            let prefix = JsString::from("@AGENTS.md");
            if line.starts_with(&prefix.0)
                && JsString(line[prefix.0.len()..].to_vec())
                    .trim()
                    .0
                    .is_empty()
            {
                continue;
            }
            stripped.push(JsString(line.to_vec()))
        }
        let local_path = node_path::filesystem_path(&cwd).join("AGENTS.md");
        let local = if local && local_path.exists() {
            JsString::from(String::from_utf8_lossy(&fs::read(local_path)?).into_owned())
        } else {
            "".into()
        };
        let sections = [
            join(&stripped, "\n").trim(),
            local.trim(),
            string(&coalesce(
                field(context, "systemPromptAddendum"),
                &"".into(),
            ))
            .trim(),
        ]
        .into_iter()
        .filter(|s| !s.0.is_empty())
        .collect::<Vec<_>>();
        let mut system = J::object([
            ("type", "preset".into()),
            ("preset", "claude_code".into()),
            ("snapshot", false.into()),
        ]);
        if !sections.is_empty() {
            system.insert("append", join(&sections, "\n\n").into_json())
        }
        options.insert("systemPrompt", system);
        self.cli(&mut options)?;
        let mut content = vec![];
        let images = field(context, "imageInputs").as_array().unwrap_or(&[]);
        let content = if images.is_empty() {
            prompt.into_json()
        } else {
            if !prompt.0.is_empty() {
                content.push(J::object([
                    ("type", "text".into()),
                    ("text", prompt.into_json()),
                ]))
            }
            for image in images {
                content.push(J::object([
                    ("type", "image".into()),
                    (
                        "source",
                        if is(field(image, "kind"), "base64") {
                            J::object([
                                ("type", "base64".into()),
                                ("media_type", field(image, "mediaType").clone()),
                                ("data", field(image, "data").clone()),
                            ])
                        } else {
                            J::object([
                                ("type", "url".into()),
                                ("url", field(image, "data").clone()),
                            ])
                        },
                    ),
                ]))
            }
            J::Array(content)
        };
        let message = J::object([
            ("type", "user".into()),
            (
                "message",
                J::object([("role", "user".into()), ("content", content)]),
            ),
            ("parent_tool_use_id", J::Null),
            ("origin", J::object([("kind", "human".into())])),
        ]);
        Ok(QueryOptions {
            params: J::object([
                ("turnId", field(context, "turnId").clone()),
                ("options", options),
                ("messages", J::Array(vec![message])),
                ("permissionCallback", callback.into()),
            ]),
            auto_allow,
        })
    }
    pub fn resume(&self, value: &J, cwd: Option<&JsString>) -> Option<JsString> {
        if !nonempty_string(value) {
            return None;
        }
        let value = string(value);
        if ["agent-http:", "agentapi:", "claude-p:"]
            .iter()
            .any(|p| value.0.starts_with(&JsString::from(*p).0))
        {
            return None;
        }
        let Some(cwd) = cwd.filter(|s| !s.0.is_empty()) else {
            return Some(value);
        };
        let slug = |p: &JsString| {
            // Claude's persisted project keys use DOS/UNC spelling. A native
            // canonical path may carry a Windows verbatim prefix for the same
            // directory; that prefix must not create a second project key.
            #[cfg(windows)]
            let ordinary = {
                use std::path::{Component, Prefix};
                match node_path::to_native(p).components().next() {
                    Some(Component::Prefix(prefix)) => match prefix.kind() {
                        Prefix::VerbatimDisk(_) => JsString(p.0[4..].to_vec()),
                        Prefix::VerbatimUNC(_, _) => JsString([&[92, 92][..], &p.0[8..]].concat()),
                        _ => p.clone(),
                    },
                    _ => p.clone(),
                }
            };
            #[cfg(windows)]
            let p = &ordinary;
            JsString(
                p.0.iter()
                    .map(|u| {
                        if (48..=57).contains(u)
                            || (65..=90).contains(u)
                            || (97..=122).contains(u)
                            || *u == 45
                        {
                            *u
                        } else {
                            45
                        }
                    })
                    .collect(),
            )
        };
        let mut slugs = vec![slug(cwd)];
        let cwd_path = node_path::filesystem_path(cwd);
        if let Ok(real) = ivy_host_runtime::paths::realpath(&cwd_path) {
            let real = node_path::from_native(&real);
            let real = slug(&real);
            if !slugs.contains(&real) {
                slugs.push(real)
            }
        }
        let mut roots = vec![node_path::filesystem_path(&self.home).join(".claude")];
        if let Some(root) = self.nonempty("CLAUDE_CONFIG_DIR") {
            roots.push(node_path::filesystem_path(&root))
        }
        let extras = self
            .nonempty("CLAUDE_CONFIG_DIRS")
            .or_else(|| self.nonempty("CLAUDE_ROUTER_DIR"))
            .unwrap_or_else(|| "".into());
        for path in extras.0.split(|u| *u == 44 || *u == 58) {
            let path = JsString(path.to_vec()).trim();
            if !path.0.is_empty() {
                let path = node_path::filesystem_path(&path);
                if path.exists() {
                    roots.push(path)
                }
            }
        }
        let cache = node_path::filesystem_path(&self.home).join(".cache");
        if cache.exists() {
            if let Ok(entries) = fs::read_dir(cache) {
                for entry in entries.flatten() {
                    let router = entry.path().join("claude-router");
                    if router.exists() {
                        if let Ok(items) = fs::read_dir(router) {
                            for item in items.flatten() {
                                roots.push(item.path())
                            }
                        }
                    }
                }
            }
        }
        let mut seen = BTreeSet::new();
        roots.retain(|r| seen.insert(node_path::from_native(r)));
        let filename = concat(&[&value, &".jsonl".into()]);
        let mut source = None;
        for root in &roots {
            for slug in &slugs {
                let path = root
                    .join("projects")
                    .join(node_path::filesystem_path(slug))
                    .join(node_path::filesystem_path(&filename));
                if path.exists() {
                    source = Some(path);
                    break;
                }
            }
            if source.is_some() {
                break;
            }
            let path = root
                .join("sessions")
                .join(node_path::filesystem_path(&filename));
            if path.exists() {
                source = Some(path);
                break;
            }
        }
        let source = source?;
        for root in &roots {
            for slug in &slugs {
                let dir = root.join("projects").join(node_path::filesystem_path(slug));
                let target = dir.join(node_path::filesystem_path(&filename));
                if target == source {
                    continue;
                }
                let _ = (|| -> std::io::Result<()> {
                    if !dir.exists() {
                        fs::create_dir_all(&dir)?
                    }
                    if !target.exists() {
                        fs::copy(&source, &target)?;
                    }
                    Ok(())
                })();
            }
        }
        Some(value)
    }
}
pub fn workflow_prompt(prompt: &JsString) -> JsString {
    if let Some(super::commands::WorkflowCommand::Run(task)) =
        super::commands::workflow_command(prompt)
    {
        concat(&[&"ultracode: ".into(),&task,&"\n\nKeep the launched workflow attached to this turn: wait for every workflow task to finish before returning the final response.".into()])
    } else {
        prompt.clone()
    }
}
