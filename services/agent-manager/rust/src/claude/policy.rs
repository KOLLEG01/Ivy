//! Pinned adapter policy and representation helpers. No SDK calls or discovery.
use super::{
    error::{AdapterError, Result},
    value::*,
};
use ivy_contracts::{IvyJson as J, JsString};

fn one_of(v: &J, values: &[&str], trim: bool) -> J {
    let Some(s) = v.as_js_string() else {
        return J::Null;
    };
    let s = if trim { s.trim() } else { s.clone() };
    if values.iter().any(|v| s == JsString::from(*v)) {
        J::String(s)
    } else {
        J::Null
    }
}
pub fn approval(v: &J) -> J {
    let normalized = one_of(
        v,
        &[
            "untrusted",
            "on-failure",
            "on-request",
            "never",
            "unless-trusted",
        ],
        true,
    );
    if is(&normalized, "unless-trusted") {
        "untrusted".into()
    } else {
        normalized
    }
}
pub fn sandbox(v: &J) -> J {
    one_of(
        v,
        &["read-only", "workspace-write", "danger-full-access"],
        true,
    )
}
pub fn personality(v: &J) -> J {
    one_of(
        v,
        &["none", "friendly", "pragmatic", "cynic", "robot", "nerd"],
        true,
    )
}
pub fn thread_source(v: &J) -> J {
    one_of(v, &["user", "subagent", "memory_consolidation"], true)
}
pub fn session_source(v: &J) -> J {
    if v.as_js_string().is_none() {
        return "appServer".into();
    }
    let s = one_of(
        v,
        &[
            "cli",
            "vscode",
            "exec",
            "appServer",
            "unknown",
            "app_server",
            "app-server",
        ],
        true,
    );
    if is(&s, "app_server") || is(&s, "app-server") {
        "appServer".into()
    } else if s.is_null() {
        "unknown".into()
    } else {
        s
    }
}
pub fn null_if_empty(v: &J) -> J {
    v.as_js_string()
        .filter(|v| !v.0.is_empty())
        .cloned()
        .map(J::String)
        .unwrap_or(J::Null)
}
pub fn effort(v: &J) -> J {
    if is(v, "minimal") {
        "low".into()
    } else {
        one_of(v, &["low", "medium", "high", "xhigh", "max"], false)
    }
}
pub fn collab_effort(v: &J) -> J {
    one_of(
        v,
        &["none", "minimal", "low", "medium", "high", "xhigh"],
        true,
    )
}
pub fn effort_from_params(p: &J, fallback: &J) -> J {
    let settings = field(field(p, "collaborationMode"), "settings");
    let choices = [
        field(settings, "reasoning_effort"),
        field(p, "effort"),
        field(field(p, "config"), "model_reasoning_effort"),
        fallback,
    ];
    effort(
        choices
            .into_iter()
            .find(|v| v.as_js_string().is_some())
            .unwrap_or(&NULL),
    )
}
pub fn model_from_params(p: &J, fallback: &J) -> J {
    [
        field(field(field(p, "collaborationMode"), "settings"), "model"),
        field(p, "model"),
        field(field(p, "config"), "model"),
        fallback,
    ]
    .into_iter()
    .find(|v| v.as_js_string().is_some_and(|s| !s.0.is_empty()))
    .cloned()
    .unwrap_or_else(|| "".into())
}
pub fn profile_id(v: &J) -> J {
    v.as_js_string()
        .map(|v| v.trim())
        .filter(|v| !v.0.is_empty() && v.0.len() <= 128)
        .map(J::String)
        .unwrap_or(J::Null)
}
pub fn profile_from_params(p: &J) -> J {
    let direct = profile_id(field(p, "permissions"));
    if direct.is_null() {
        profile_id(field(field(p, "activePermissionProfile"), "id"))
    } else {
        direct
    }
}
pub fn profile_policy(id: &J) -> (J, J) {
    if is(id, ":read-only") {
        ("on-request".into(), "read-only".into())
    } else if is(id, ":workspace") {
        ("on-request".into(), "workspace-write".into())
    } else if is(id, ":danger-full-access") {
        ("never".into(), "danger-full-access".into())
    } else {
        (J::Null, J::Null)
    }
}
pub fn thread_profile(t: &J) -> J {
    let explicit = profile_id(field(t, "permissionProfileId"));
    if !explicit.is_null() {
        return explicit;
    }
    if is(field(t, "sandboxMode"), "read-only") {
        ":read-only".into()
    } else if is(field(t, "sandboxMode"), "danger-full-access") {
        ":danger-full-access".into()
    } else if is(field(t, "sandboxMode"), "workspace-write")
        && is(field(t, "approvalPolicy"), "on-request")
    {
        ":workspace".into()
    } else {
        J::Null
    }
}
pub fn turn_sandbox(p: &J) -> J {
    if field(p, "sandbox").as_js_string().is_some() {
        return sandbox(field(p, "sandbox"));
    }
    match field(field(p, "sandboxPolicy"), "type")
        .as_str()
        .ok()
        .as_deref()
    {
        Some("dangerFullAccess") => "danger-full-access".into(),
        Some("readOnly") => "read-only".into(),
        Some("workspaceWrite" | "externalSandbox") => "workspace-write".into(),
        _ => J::Null,
    }
}
pub fn has_legacy_permissions(p: &J) -> bool {
    field(p, "approvalPolicy").as_js_string().is_some()
        || field(p, "sandbox").as_js_string().is_some()
        || field(p, "sandboxPolicy").as_object().is_some()
}
pub fn apply_permissions(t: &mut J, p: &J, turn: bool) {
    let id = profile_from_params(p);
    let (policy, mode) = profile_policy(&id);
    if !id.is_null() {
        t.insert("permissionProfileId", id);
        if !policy.is_null() {
            t.insert("approvalPolicy", policy)
        }
        if !mode.is_null() {
            t.insert("sandboxMode", mode)
        }
    } else {
        if has_legacy_permissions(p) {
            t.insert("permissionProfileId", J::Null)
        }
        if field(p, "approvalPolicy").as_js_string().is_some() {
            t.insert("approvalPolicy", approval(field(p, "approvalPolicy")))
        }
        if turn {
            let mode = turn_sandbox(p);
            if !mode.is_null() {
                t.insert("sandboxMode", mode)
            }
        } else if field(p, "sandbox").as_js_string().is_some() {
            t.insert("sandboxMode", sandbox(field(p, "sandbox")))
        }
    }
}
pub fn sandbox_envelope(t: &J) -> J {
    if is(field(t, "sandboxMode"), "read-only") {
        J::object([("type", "readOnly".into()), ("networkAccess", false.into())])
    } else if is(field(t, "sandboxMode"), "danger-full-access") {
        J::object([("type", "dangerFullAccess".into())])
    } else {
        J::object([
            ("type", "workspaceWrite".into()),
            ("writableRoots", J::Array(vec![field(t, "cwd").clone()])),
            ("networkAccess", true.into()),
            ("excludeTmpdirEnvVar", false.into()),
            ("excludeSlashTmp", false.into()),
        ])
    }
}
pub fn permission_profiles(p: &J) -> Result<J> {
    let profiles = [":read-only", ":workspace", ":danger-full-access"].map(|id| {
        J::object([
            ("id", id.into()),
            ("description", J::Null),
            ("allowed", true.into()),
        ])
    });
    let start = if field(p, "cursor").is_null() {
        0.0
    } else {
        number(field(p, "cursor"))
    };
    if !start.is_finite() || start.fract() != 0.0 || !(0.0..=3.0).contains(&start) {
        return Err(AdapterError::new("invalid permission profile cursor"));
    }
    let limit = field(p, "limit")
        .as_f64()
        .filter(|n| n.is_finite())
        .map(|n| n.floor().max(1.0))
        .unwrap_or(3.0);
    let end = (start + limit).min(3.0);
    Ok(J::object([
        ("data", J::Array(array_slice(&profiles, start, end))),
        (
            "nextCursor",
            if end < 3.0 {
                string(&J::Number(end)).into_json()
            } else {
                J::Null
            },
        ),
    ]))
}
pub fn decision(v: &J) -> J {
    let d = field(v, "decision");
    let known = one_of(
        d,
        &["accept", "acceptForSession", "decline", "cancel"],
        false,
    );
    if !known.is_null() {
        known
    } else if d.get("acceptWithExecpolicyAmendment").is_some()
        || d.get("applyNetworkPolicyAmendment").is_some()
    {
        "acceptForSession".into()
    } else {
        "decline".into()
    }
}
pub fn text_input(v: &J) -> JsString {
    let mut parts = Vec::new();
    for item in v.as_array().unwrap_or(&[]) {
        let text = match field(item, "type").as_str().ok().as_deref() {
            Some("text") => coalesce(field(item, "text"), &"".into()),
            Some("mention") => J::String(concat(&[
                &"@".into(),
                &string(&coalesce(
                    field(item, "path"),
                    &coalesce(field(item, "name"), &"".into()),
                )),
            ])),
            Some("localImage") => J::String(concat(&[
                &"[local image: ".into(),
                &string(&coalesce(field(item, "path"), &"".into())),
                &"]".into(),
            ])),
            Some("image") => "[image]".into(),
            Some("skill") => J::String(concat(&[
                &"/".into(),
                &string(&coalesce(field(item, "name"), &"skill".into())),
            ])),
            _ => J::Null,
        };
        if truthy(&text) {
            parts.push(string(&text))
        }
    }
    join(&parts, "\n")
}
pub fn turn_view(turn: &J, view: &str) -> J {
    let all = field(turn, "items").as_array().unwrap_or(&[]);
    let items = match view {
        "full" => all.to_vec(),
        "summary" => {
            let first = all.iter().find(|i| is(field(i, "type"), "userMessage"));
            let last = all
                .iter()
                .rev()
                .find(|i| is(field(i, "type"), "agentMessage"));
            let mut items = first.into_iter().cloned().collect::<Vec<_>>();
            if let Some(last) = last {
                if first.is_none_or(|first| field(first, "id") != field(last, "id")) {
                    items.push(last.clone())
                }
            }
            items
        }
        _ => vec![],
    };
    let mut out = J::object([
        ("id", field(turn, "id").clone()),
        ("items", J::Array(items)),
        ("itemsView", view.into()),
    ]);
    for key in ["status", "error", "startedAt", "completedAt", "durationMs"] {
        out.insert(key, field(turn, key).clone())
    }
    out
}
pub fn is_subagent(v: &J) -> bool {
    let s = string(v).trim().to_english_lowercase();
    ["task", "agent", "subagent", "spawn_agent", "spawnagent"]
        .iter()
        .any(|x| s == JsString::from(*x))
}
pub fn system_prompt(base: &J, developer: &J, personality: &J, desktop: bool) -> J {
    let mut sections = vec![];
    if desktop {
        sections.push(JsString::from(include_str!(
            "../../../../../instructions/claude/desktop-presentation.txt"
        )))
    }
    for (prefix, value) in [
        ("# Project instructions\n", base),
        ("# Developer instructions\n", developer),
    ] {
        let text = string(&coalesce(value, &"".into())).trim();
        if !text.0.is_empty() {
            sections.push(concat(&[&prefix.into(), &text]))
        }
    }
    let cue = match personality.as_str().ok().as_deref() {
        Some("friendly") => {
            "Personality: friendly. Communicate in a warm, encouraging tone; default to plain language and short paragraphs."
        }
        Some("pragmatic") => {
            "Personality: pragmatic. Be concise and direct; lead with the answer, skip pleasantries, prefer concrete code or commands."
        }
        Some("cynic") => {
            "Personality: cynic. Be terse and wry; surface trade-offs and risks plainly; avoid hype."
        }
        Some("robot") => {
            "Personality: robot. Reply in clipped, structured prose; prefer bullet points and exact field names; minimise filler."
        }
        Some("nerd") => {
            "Personality: nerd. Get into mechanism and detail; explain underlying assumptions and edge cases when relevant."
        }
        _ => "",
    };
    if !cue.is_empty() {
        sections.push(cue.into())
    }
    if sections.is_empty() {
        J::Null
    } else {
        join(&sections, "\n\n").into_json()
    }
}
