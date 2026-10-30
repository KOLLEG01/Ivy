use super::*;

pub(super) fn metadata() -> &'static IvyJson {
    static DATA: OnceLock<IvyJson> = OnceLock::new();
    DATA.get_or_init(|| {
        parse(include_str!("discovery_metadata.json"), usize::MAX).expect("discovery metadata")
    })
}
pub(super) fn meta(key: &str) -> &'static IvyJson {
    metadata().get(key).expect("discovery metadata key")
}
pub(super) fn concat(values: &[&JsString]) -> JsString {
    JsString(values.iter().flat_map(|v| v.0.iter().copied()).collect())
}
pub(super) fn joined(values: &[JsString], separator: &str) -> JsString {
    let mut result = vec![];
    for (i, value) in values.iter().enumerate() {
        if i > 0 {
            result.extend(separator.encode_utf16());
        }
        result.extend_from_slice(&value.0);
    }
    JsString(result)
}
pub(super) fn starts(value: &JsString, prefix: &JsString) -> bool {
    value.0.starts_with(&prefix.0)
}
pub(super) fn includes(value: &JsString, part: &JsString) -> bool {
    part.0.is_empty() || value.0.windows(part.0.len()).any(|v| v == part.0)
}
pub(super) fn segment(value: &JsString, separator: u16) -> JsString {
    JsString(
        value
            .0
            .iter()
            .copied()
            .take_while(|v| *v != separator)
            .collect(),
    )
}
pub(super) fn default_group(group: &JsString) -> JsString {
    let mut value = vec![];
    for c in &group.0 {
        if *c == 47 {
            value.extend(" / ".encode_utf16());
        } else {
            value.push(*c);
        }
    }
    value.extend(" operations and tools.".encode_utf16());
    JsString(value)
}
pub(crate) fn collapse_whitespace(value: &JsString) -> JsString {
    let mut normalized = vec![];
    let mut space = false;
    for c in &value.0 {
        if JsString::trim_ecmascript_units(&[*c]).is_empty() {
            space = !normalized.is_empty();
        } else {
            if space {
                normalized.push(32);
            }
            space = false;
            normalized.push(*c);
        }
    }
    JsString(normalized)
}
pub(super) fn compact(value: &JsString) -> JsString {
    let normalized = collapse_whitespace(value).0;
    let mut out = vec![];
    let mut bytes = 0;
    let mut i = 0;
    while i < normalized.len() {
        let c = normalized[i];
        let pair = (0xd800..=0xdbff).contains(&c)
            && normalized
                .get(i + 1)
                .is_some_and(|v| (0xdc00..=0xdfff).contains(v));
        let width = if pair { 2 } else { 1 };
        let count = if pair {
            4
        } else if c < 128 {
            1
        } else if c < 2048 {
            2
        } else {
            3
        };
        if bytes + count > 217 {
            out.extend("...".encode_utf16());
            return JsString(out);
        }
        out.extend_from_slice(&normalized[i..i + width]);
        bytes += count;
        i += width;
    }
    if out.is_empty() {
        "Service capabilities.".into()
    } else {
        JsString(out)
    }
}
pub(super) fn words(value: &JsString, runtime: &dyn Runtime) -> IvyResult<Vec<JsString>> {
    let mut camel = vec![];
    for (i, c) in value.0.iter().enumerate() {
        if i > 0 && (97..=122).contains(&value.0[i - 1]) && (65..=90).contains(c) {
            camel.push(32);
        }
        camel.push(*c);
    }
    let lower = runtime.lowercase(&JsString(camel))?;
    let mut result = vec![];
    let mut word = vec![];
    let mut i = 0;
    while i < lower.0.len() {
        let c = lower.0[i];
        let pair = (0xd800..=0xdbff).contains(&c)
            && lower
                .0
                .get(i + 1)
                .is_some_and(|v| (0xdc00..=0xdfff).contains(v));
        let (code, width) = if pair {
            (
                0x10000 + ((u32::from(c) - 0xd800) << 10) + u32::from(lower.0[i + 1]) - 0xdc00,
                2,
            )
        } else {
            (u32::from(c), 1)
        };
        if runtime.word_character(code) {
            word.extend_from_slice(&lower.0[i..i + width]);
        } else if !word.is_empty() {
            add_word(&mut result, std::mem::take(&mut word));
        }
        i += width;
    }
    if !word.is_empty() {
        add_word(&mut result, word);
    }
    Ok(result)
}
fn add_word(result: &mut Vec<JsString>, word: Vec<u16>) {
    let value = JsString(word);
    let plain = text(&value);
    if [
        "a", "an", "the", "for", "of", "to", "in", "and", "or", "with", "der", "die", "das", "den",
        "dem", "ein", "eine", "einen", "und", "oder", "mit", "im", "auf", "für", "ich", "möchte",
        "bitte",
    ]
    .contains(&plain.as_str())
    {
        return;
    }
    let synonym = if plain.starts_with("archiv") {
        "archive"
    } else {
        match plain.as_str() {
            "thread" | "threads" | "tasks" | "aufgabe" | "aufgaben" => "task",
            "projects" | "projekt" | "projekte" => "project",
            "verbrauch" | "kontingent" => "usage",
            "limits" => "limit",
            "files" | "datei" | "dateien" => "file",
            "documents" | "dokumente" => "document",
            "nachricht" | "nachrichten" | "messages" => "message",
            "sprachchat" | "sprache" => "voice",
            "anruf" | "anrufe" => "call",
            "events" | "ereignis" | "ereignisse" => "event",
            "triggers" => "trigger",
            "schedules" | "scheduled" | "scheduling" | "zeitplan" | "zeitpläne" => "schedule",
            "reminders" | "erinnerung" | "erinnerungen" => "reminder",
            "ungelesen" | "ungelesene" | "ungelesenen" => "unread",
            "notifications" | "benachrichtigung" | "benachrichtigungen" => "notification",
            "suche" | "suchen" => "search",
            "finden" => "find",
            "lesen" => "read",
            "erstellen" => "create",
            _ => {
                result.push(value);
                return;
            }
        }
    };
    result.push(synonym.into());
}
pub(super) fn mcp_name(value: &IvyJson) -> Option<&IvyJson> {
    value.get("discovery").and_then(|v| v.get("mcp"))
}
pub(super) fn guide(value: &JsString, catalog: &crate::registry_catalog::Catalog) -> IvyResult<JsString> {
    let mut names: HashMap<JsString, JsString> = HashMap::new();
    for (method, name) in meta("coreMcpNames")
        .as_object()
        .ok_or_else(IvyError::internal)?
    {
        let public = meta("publicCoreMcpMethods")
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v.as_js_string() == Some(method));
        names.insert(
            concat(&[&"hive.".into(), method]),
            concat(&[
                name.as_js_string().ok_or_else(IvyError::internal)?,
                &if public { "" } else { " (ivy_dev only)" }.into(),
            ]),
        );
    }
    for ns in &catalog.namespaces {
        for binding in catalog.bindings(ns) {
            let tool = &binding.definition;
            if let Some(mcp) = mcp_name(tool).filter(|v| v.as_object().is_some()) {
                names.insert(
                    concat(&[&ns.name, &".".into(), string(tool, "name")?]),
                    concat(&[
                        string(mcp, "name")?,
                        &if is_text(mcp.get("surface"), "ivy_dev") {
                            " (ivy_dev only)"
                        } else {
                            ""
                        }
                        .into(),
                    ]),
                );
            }
        }
    }
    let letter = |v: u16| (65..=90).contains(&v) || (97..=122).contains(&v);
    let digit = |v: u16| (48..=57).contains(&v);
    let mut result = vec![];
    let mut i = 0;
    while i < value.0.len() {
        if letter(value.0[i]) {
            let mut end = i + 1;
            while end < value.0.len()
                && (letter(value.0[end]) || digit(value.0[end]) || value.0[end] == 45)
            {
                end += 1;
            }
            if value.0.get(end) == Some(&46) && value.0.get(end + 1).is_some_and(|v| letter(*v)) {
                end += 2;
                while end < value.0.len()
                    && (letter(value.0[end])
                        || digit(value.0[end])
                        || [46, 47].contains(&value.0[end]))
                {
                    end += 1;
                }
                let period = value.0[end - 1] == 46;
                let key = JsString(value.0[i..end - usize::from(period)].to_vec());
                if let Some(name) = names.get(&key) {
                    result.extend_from_slice(&name.0);
                    if period {
                        result.push(46);
                    }
                } else {
                    result.extend_from_slice(&value.0[i..end]);
                }
                i = end;
                continue;
            }
        }
        result.push(value.0[i]);
        i += 1;
    }
    Ok(JsString(result))
}
