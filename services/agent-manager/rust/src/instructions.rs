//! Managed instruction composition. Retrieval and file ownership remain with
//! the environment owner; this projection never modifies local instruction bytes.
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString,
    hash::{digest, hash_json},
};

#[derive(Debug, PartialEq)]
pub struct ComposedInstructions {
    pub body: Option<Vec<u8>>,
    pub version: String,
}

fn document(source: &IvyJson) -> Option<&IvyJson> {
    source.get("document")
}
fn flag(source: Option<&IvyJson>, key: &str) -> bool {
    source.and_then(document).and_then(|d| d.get(key)) == Some(&IvyJson::Bool(true))
}
fn inherits(source: &IvyJson) -> bool {
    document(source).and_then(|d| d.get("enabled")) == Some(&IvyJson::Null)
}

/// Inputs are the original validated document/envelope objects. Keep their
/// exact values in the version identity, including disabled/inherited sources.
pub fn compose_instructions(
    global: Option<&IvyJson>,
    host: Option<&IvyJson>,
    local: Option<&[u8]>,
    defaults: Option<&IvyJson>,
    discovered: &JsString,
) -> IvyResult<ComposedInstructions> {
    let fallback = defaults
        .map(|d| -> IvyResult<IvyJson> {
            Ok(IvyJson::object([
                ("document", d.clone()),
                ("objectId", "project-default".into()),
                ("revision", IvyJson::Number(0.0)),
                ("contentHash", hash_json(d)?.into()),
            ]))
        })
        .transpose()?;
    let hive = global.filter(|s| !inherits(s)).or(fallback.as_ref());
    let effective_host = host.filter(|s| !inherits(s));
    let selected = effective_host.or(hive);
    let active = flag(selected, "enabled");
    let include_local = flag(selected, "includeLocal");
    let local = local.filter(|bytes| !bytes.is_empty());
    let include_bytes = active && include_local && local.is_some();
    let mut parts: Vec<JsString> = Vec::new();
    if include_bytes {
        let text = std::str::from_utf8(local.unwrap()).map_err(|_| {
            IvyError::new(
                "target_conflict",
                "Local AGENTS.md is not valid UTF-8; its bytes remain unchanged.",
            )
        })?;
        parts.push("## Local user instructions".into());
        // TextDecoder's default ignoreBOM=false consumes a leading UTF-8 BOM.
        parts.push(text.strip_prefix('\u{feff}').unwrap_or(text).into());
    }
    if active {
        for source in [hive, effective_host]
            .into_iter()
            .flatten()
            .filter(|s| flag(Some(s), "enabled"))
        {
            let d = document(source).ok_or_else(IvyError::internal)?;
            let mut text = d
                .get("text")
                .and_then(IvyJson::as_js_string)
                .cloned()
                .ok_or_else(IvyError::internal)?;
            if d.get("hostId") == Some(&IvyJson::Null) {
                let heading = JsString::from("# Actions attributed to the user");
                let suffix = text.0.get(heading.0.len()..);
                if text.0.starts_with(&heading.0)
                    && suffix.is_some_and(|s| {
                        s.is_empty() || s.starts_with(&[10]) || s.starts_with(&[13, 10])
                    })
                {
                    text.0.insert(0, b'#' as u16);
                }
            } else {
                parts.push("## Host instructions (take precedence over Hive instructions)".into());
            }
            parts.push(text);
        }
        parts.push("## Ivy MCP".into());
        parts.push(discovered.clone());
        if include_local {
            parts.push("@AGENTS.md".into());
        }
    }
    let body = if active {
        let mut units = Vec::new();
        for (i, part) in parts.iter().enumerate() {
            if i != 0 {
                units.extend([10, 10]);
            }
            units.extend_from_slice(&part.0);
        }
        units.push(10);
        let bytes = String::from_utf16_lossy(&units).into_bytes();
        if bytes.len() > 32768 {
            return Err(IvyError::new(
                "limit_exceeded",
                "Composed instructions exceed 32 KiB; shorten the documents instead of silently truncating them.",
            ));
        }
        Some(bytes)
    } else {
        None
    };
    let version = hash_json(&IvyJson::object([
        ("defaults", defaults.cloned().unwrap_or(IvyJson::Null)),
        ("global", global.cloned().unwrap_or(IvyJson::Null)),
        ("host", host.cloned().unwrap_or(IvyJson::Null)),
        (
            "localHash",
            if include_bytes {
                digest(local.unwrap()).into()
            } else {
                IvyJson::Null
            },
        ),
        (
            "discoveredInstructions",
            if active {
                IvyJson::String(discovered.clone())
            } else {
                IvyJson::Null
            },
        ),
    ]))?;
    Ok(ComposedInstructions { body, version })
}
