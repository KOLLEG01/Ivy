//! Projection of the pinned public provider/loop descriptors and selection.
//! This describes the selected backend; it never admits or starts a provider.
use super::{
    error::{AdapterError, Result},
    value::*,
};
use ivy_contracts::{IvyJson as J, JsString};
use std::sync::OnceLock;

/// Load the pinned adapter's existing config document. Missing, unreadable or
/// invalid saved configuration leaves the admitted defaults intact.
pub(super) fn load(env: &mut super::protocol::Environment) {
    let options = selectable(&env.environment, &env.discovered_router_models);
    let requested = field(&env.environment, "CLAUDE_CODEX_DEFAULT_MODEL");
    let disabled = is(
        field(&env.environment, "CLAUDE_CODEX_DISABLE_CODEX_PROXY"),
        "1",
    );
    env.default_model = if nonempty_string(requested)
        && ((disabled && !super::protocol::Adapter::codex_model(requested))
            || options.iter().any(|(id, _)| id == requested))
    {
        requested.clone()
    } else {
        options
            .iter()
            .find(|(_, default)| *default)
            .or_else(|| options.first())
            .map(|(id, _)| id.clone())
            .unwrap_or_else(|| "sonnet".into())
    };
    let effort = super::policy::effort(field(&env.environment, "CLAUDE_CODEX_DEFAULT_EFFORT"));
    env.default_effort = coalesce(&effort, &"medium".into());
    let Ok(bytes) = std::fs::read(&env.config_path) else {
        return;
    };
    let Ok(saved) = decode(&bytes) else { return };
    let mut repair = false;
    if let Some(raw) = field(&saved, "model")
        .as_js_string()
        .filter(|s| !s.0.is_empty())
    {
        let value = J::String(raw.clone());
        let selected = if disabled
            && !raw.trim().0.is_empty()
            && !super::protocol::Adapter::codex_model(&value)
        {
            value.clone()
        } else if let Some((id, _)) = options.iter().find(|(id, _)| *id == value) {
            id.clone()
        } else if let Some((id, _)) = options
            .iter()
            .find(|(id, _)| string(id).to_english_lowercase() == raw.to_english_lowercase())
        {
            id.clone()
        } else {
            env.default_model.clone()
        };
        repair = selected != value;
        env.default_model = selected;
    }
    if field(&saved, "model_reasoning_effort")
        .as_js_string()
        .is_some()
    {
        env.default_effort = coalesce(
            &super::policy::effort(field(&saved, "model_reasoning_effort")),
            &env.default_effort,
        );
    }
    if field(&saved, "overrides").as_object().is_some() {
        env.config_overrides = field(&saved, "overrides").clone();
    }
    if repair {
        // The original repairs in memory even when this best-effort write fails.
        let saved = J::object([
            ("model", env.default_model.clone()),
            ("model_reasoning_effort", env.default_effort.clone()),
            ("overrides", env.config_overrides.clone()),
        ]);
        let _ = persist(&env.config_path, &saved);
    }
}

fn selectable(environment: &J, discovered: &[JsString]) -> Vec<(J, bool)> {
    if let Some(raw) = field(environment, "CLAUDE_CODEX_MODELS")
        .as_js_string()
        .filter(|s| !s.0.is_empty())
    {
        if let Some(J::Array(ref rows)) = super::models::json_value(&J::String(raw.clone())) {
            return rows
                .iter()
                .map(|row| {
                    if row.as_js_string().is_some() {
                        (row.clone(), false)
                    } else if row.as_object().is_some() || row.as_array().is_some() {
                        (
                            if field(row, "id").as_js_string().is_some() {
                                field(row, "id").clone()
                            } else {
                                string(&coalesce(field(row, "model"), &"sonnet".into())).into_json()
                            },
                            field(row, "isDefault").as_bool() == Some(true),
                        )
                    } else {
                        ("sonnet".into(), false)
                    }
                })
                .collect();
        }
        return raw
            .0
            .split(|c| *c == 44)
            .map(|part| JsString(part.to_vec()).trim())
            .filter(|s| !s.0.is_empty())
            .map(|s| (s.into_json(), false))
            .collect();
    }
    // These are the pinned saved-config aliases, not the live model/list API.
    let mut result = ["sonnet", "opus", "fable", "haiku", "sonnet-1m", "opus-plan"]
        .into_iter()
        .enumerate()
        .map(|(i, id)| (J::from(id), i == 0))
        .collect::<Vec<_>>();
    for id in discovered {
        let id = J::String(id.clone());
        if !result.iter().any(|(existing, _)| *existing == id) {
            result.push((id, false))
        }
    }
    result
}

/// Preserve JSON.stringify(value, null, 2) for this existing config document.
fn persist(path: &std::path::Path, value: &J) -> std::io::Result<()> {
    let compact = encode(value).map_err(std::io::Error::other)?;
    let chars = compact.chars().collect::<Vec<_>>();
    let mut text = String::new();
    let mut depth = 0usize;
    let mut quoted = false;
    let mut escaped = false;
    for (index, c) in chars.iter().copied().enumerate() {
        if quoted {
            text.push(c);
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
                text.push(c)
            }
            '{' | '[' => {
                text.push(c);
                if !matches!(chars.get(index + 1), Some('}' | ']')) {
                    depth += 1;
                    text.push('\n');
                    text.push_str(&"  ".repeat(depth))
                }
            }
            '}' | ']' => {
                if !matches!(
                    index.checked_sub(1).and_then(|i| chars.get(i)),
                    Some('{' | '[')
                ) {
                    depth -= 1;
                    text.push('\n');
                    text.push_str(&"  ".repeat(depth))
                }
                text.push(c)
            }
            ',' => {
                text.push(c);
                text.push('\n');
                text.push_str(&"  ".repeat(depth))
            }
            ':' => text.push_str(": "),
            _ => text.push(c),
        }
    }
    text.push('\n');
    if let Some(parent) = path.parent() {
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(parent)?;
    }
    let mut options = std::fs::OpenOptions::new();
    options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    use std::io::Write;
    options.open(path)?.write_all(text.as_bytes())
}

pub fn runtime_type(value: &J) -> Result<Option<&'static str>> {
    let raw = string(&coalesce(value, &"".into()))
        .trim()
        .to_english_lowercase();
    if raw.0.is_empty() {
        return Ok(None);
    }
    let kind = match raw.to_utf8().ok().as_deref() {
        Some("mock") => "mock",
        Some(
            "sdk" | "agent-sdk" | "agent-sdk-sidecar" | "sidecar" | "cloud-agent-sdk"
            | "agent-sdk-socket" | "socket" | "runtime-socket",
        ) => "agent-sdk-sidecar",
        Some("agent-http" | "channels" | "channel" | "http-channel") => "agent-http",
        Some("agentapi" | "agent-api") => "agentapi",
        Some("claude-p" | "claudep" | "pty-transcript") => "claude-p",
        Some("codex-proxy" | "codex-exec") => "codex-proxy",
        _ => {
            return Err(AdapterError::new(concat(&[
                &"unknown CLAUDE_CODEX_RUNTIME_TYPE: ".into(),
                &string(value),
            ])));
        }
    };
    Ok(Some(kind))
}
fn input(value: &J) -> Option<JsString> {
    value
        .as_js_string()
        .map(JsString::trim)
        .filter(|s| !s.0.is_empty())
}
fn aliases(config: &J, names: [&str; 3]) -> J {
    names
        .into_iter()
        .map(|name| field(config, name))
        .find(|value| !value.is_null())
        .cloned()
        .unwrap_or(J::Null)
}
fn providers() -> J {
    let mut values = vec![];
    for (id, name, family, loop_id, event, approval, steer, credentials, gateway, notes) in [
        (
            "claude-code",
            "Claude Code",
            "anthropic",
            "native-claude-code-sdk",
            "rich",
            "native-file-diff",
            true,
            vec![
                "user-api-key",
                "cloud-provider",
                "organization-gateway",
                "local-cli-auth",
            ],
            "official-or-organization-managed",
            [
                "Credentials stay on the user host, cloud provider chain, or organization gateway.",
                "Local CLI auth is local-only and not a productized credential distribution model.",
            ],
        ),
        (
            "codex",
            "Codex proxy",
            "openai",
            "codex-jsonl-proxy",
            "jsonl",
            "native-codex",
            false,
            vec!["local-cli-auth"],
            "none",
            [
                "Auth remains owned by the local Codex CLI on the user host.",
                "The adapter does not collect or redistribute Codex subscription/session credentials.",
            ],
        ),
    ] {
        values.push(J::object([
            ("id", id.into()),
            ("displayName", name.into()),
            ("status", "stable".into()),
            ("providerFamily", family.into()),
            ("loopId", loop_id.into()),
            ("eventFidelity", event.into()),
            ("approvalFidelity", approval.into()),
            ("supportsSteer", steer.into()),
            ("supportsInterrupt", true.into()),
            ("supportsResume", true.into()),
            (
                "allowedCredentialSources",
                J::Array(credentials.into_iter().map(J::from).collect()),
            ),
            ("gatewayPolicy", gateway.into()),
            (
                "complianceNotes",
                J::Array(notes.into_iter().map(J::from).collect()),
            ),
        ]));
    }
    J::Array(values)
}
fn redact(text: &JsString) -> JsString {
    static PATTERNS: OnceLock<Vec<regex::Regex>> = OnceLock::new();
    let patterns=PATTERNS.get_or_init(||[
        r"(?i)(?-u:\b)((?:[A-Z0-9_]*_)?(?:API[_-]?KEY|TOKEN|PASSWORD|SECRET))\s*[:=]\s*(?:Bearer\s+)?[A-Za-z0-9._~+/=-]{8,}",
        r"(?i)(?-u:\b)Bearer\s+[A-Za-z0-9._~+/=-]{8,}",
        r"(?i)(?-u:\b)sk-(?:ant|proj)?-[A-Za-z0-9_-]{8,}",
        r"(?i)(?-u:\b)gh[opsu]_[A-Za-z0-9_]{8,}",
        r"(?-u:\b)AKIA[0-9A-Z]{16}(?-u:\b)",
        r"-----BEGIN [A-Z ]*PRIVATE KEY-----(?s:.*?)-----END [A-Z ]*PRIVATE KEY-----",
    ].into_iter().map(|pattern| regex::Regex::new(pattern).expect("static redaction pattern")).collect());
    let mut text = String::from_utf16_lossy(&text.0);
    for (index, pattern) in patterns.iter().enumerate() {
        text = pattern
            .replace_all(&text, |captures: &regex::Captures<'_>| {
                if index == 0 {
                    format!("{}=[redacted]", &captures[1])
                } else {
                    "[redacted]".to_owned()
                }
            })
            .into_owned();
    }
    text.into()
}
pub fn provider_loop_config(environment: &J, config: &J) -> Result<J> {
    let legacy = runtime_type(&coalesce(
        field(environment, "CLAUDE_CODEX_RUNTIME_TYPE"),
        &coalesce(
            field(environment, "CLAUDE_CODEX_RUNTIME"),
            field(environment, "CLAUDE_CODEX_BACKEND"),
        ),
    ))?;
    let env_provider = input(field(environment, "CLAUDE_CODEX_PROVIDER"));
    let env_loop = input(field(environment, "CLAUDE_CODEX_AGENT_LOOP"));
    let by_env = env_provider.is_some() || env_loop.is_some();
    let (provider, agent_loop) = if by_env {
        (env_provider, env_loop)
    } else {
        (
            input(&aliases(
                config,
                [
                    "provider_loop_provider",
                    "providerLoopProvider",
                    "provider_loop_config.providerId",
                ],
            )),
            input(&aliases(
                config,
                [
                    "provider_loop_agent_loop",
                    "providerLoopAgentLoop",
                    "provider_loop_config.loopId",
                ],
            )),
        )
    };
    let match_provider = provider.as_ref().and_then(|value| {
        ["claude-code", "codex"]
            .iter()
            .position(|id| *value == JsString::from(*id))
    });
    let match_loop = agent_loop.as_ref().and_then(|value| {
        ["native-claude-code-sdk", "codex-jsonl-proxy"]
            .iter()
            .position(|id| *value == JsString::from(*id))
    });
    let mut issues = vec![];
    let issue = |field: &str, code: &str, message: JsString| {
        J::object([
            ("descriptorId", "provider-loop-selection".into()),
            ("field", field.into()),
            ("code", code.into()),
            ("message", message.into_json()),
        ])
    };
    if let Some(provider) = &provider {
        if match_provider.is_none() {
            issues.push(issue(
                "provider",
                "unknown-provider",
                concat(&[&"unknown provider selection: ".into(), &redact(provider)]),
            ))
        }
    }
    if let Some(agent_loop) = &agent_loop {
        if match_loop.is_none() {
            issues.push(issue(
                "loop",
                "unknown-loop",
                concat(&[
                    &"unknown agent loop selection: ".into(),
                    &redact(agent_loop),
                ]),
            ))
        }
    }
    if let (Some(provider), Some(agent_loop)) = (match_provider, match_loop) {
        if provider != agent_loop {
            issues.push(issue(
                "loop",
                "provider-loop-mismatch",
                format!(
                    "provider {} does not support agent loop {}",
                    ["claude-code", "codex"][provider],
                    ["native-claude-code-sdk", "codex-jsonl-proxy"][agent_loop]
                )
                .into(),
            ))
        }
    }
    let selected = if issues.is_empty() {
        match_provider.or(match_loop).unwrap_or(0)
    } else {
        0
    };
    let mock = is(field(environment, "CLAUDE_CODEX_MOCK"), "1");
    let kind = if mock {
        "mock"
    } else {
        legacy.unwrap_or(if selected == 1 {
            "codex-proxy"
        } else {
            "agent-sdk-sidecar"
        })
    };
    let source = if mock {
        "mock"
    } else if legacy.is_some() {
        "legacy-runtime"
    } else if provider.is_some() || agent_loop.is_some() {
        if by_env { "environment" } else { "config" }
    } else {
        "default"
    };
    Ok(J::object([
        ("providers", providers()),
        (
            "selection",
            J::object([
                ("providerId", ["claude-code", "codex"][selected].into()),
                (
                    "loopId",
                    ["native-claude-code-sdk", "codex-jsonl-proxy"][selected].into(),
                ),
                ("runtimeType", kind.into()),
                ("source", source.into()),
            ]),
        ),
        ("issues", J::Array(issues)),
    ]))
}
