//! Migration of retained diagnostic incidents into visible service/error groups.
//! Requests remain opaque so grouping never changes an original delivery identity.
use ivy_contracts::{IvyError, IvyJson as Json, IvyResult, JsString, hash::hash_json};

fn flag(value: &Json, key: &str) -> bool {
    value.get(key).and_then(Json::as_bool) == Some(true)
}
fn optional<'a>(value: &'a Json, key: &str) -> &'a Json {
    static MISSING: Json = Json::Null;
    value.get(key).unwrap_or(&MISSING)
}
fn present(value: &Json) -> bool {
    !value.is_null()
}
fn number(value: &Json, key: &str) -> IvyResult<f64> {
    value.required(key)?.as_f64().ok_or_else(IvyError::internal)
}
fn operation(pending: &Json) -> Option<&Json> {
    pending.get("request").and_then(|v| v.get("operationId"))
}
pub fn notice_key(entry: &Json) -> IvyResult<String> {
    hash_json(&Json::object([
        ("code", entry.required("code")?.clone()),
        ("label", entry.required("label")?.clone()),
    ]))
}

pub fn diagnostic_notice(value: &Json) -> IvyResult<Json> {
    let resource = optional(value, "resource");
    let mut caller = match resource
        .get("callerPrincipalId")
        .and_then(Json::as_js_string)
    {
        Some(caller) => caller.clone(),
        None => {
            let source = value
                .required("source")?
                .as_js_string()
                .ok_or_else(IvyError::internal)?;
            let prefix = JsString::from("service:");
            if source.0.starts_with(&prefix.0) {
                JsString(source.0[prefix.0.len()..].to_vec())
            } else {
                source.clone()
            }
        }
    };
    if let Some(target) = resource.get("serviceNodeId").and_then(Json::as_js_string) {
        if target != &caller {
            caller.0.extend(" → ".encode_utf16());
            caller.0.extend(&target.0);
        }
    }
    Ok(Json::object([
        ("code", value.required("code")?.clone()),
        ("label", Json::String(caller)),
    ]))
}

fn retain_receipts(state: &mut Json, entry: &Json, kept: &[Json]) -> IvyResult<()> {
    for kind in ["alert", "recovery"] {
        let pending = optional(entry, kind);
        if !present(pending) || flag(pending, "confirmed") {
            continue;
        }
        let op = operation(pending).ok_or_else(IvyError::internal)?;
        if kept.contains(op) {
            continue;
        }
        let mut receipt = entry.clone();
        receipt.insert("active", Json::Bool(false));
        receipt.insert("receiptOnly", Json::Bool(true));
        receipt.insert("alert", Json::Null);
        receipt.insert("recovery", Json::Null);
        receipt.insert(kind, pending.clone());
        state.insert(format!("receipt:{}", hash_json(op)?), receipt);
    }
    Ok(())
}

/// Group the saved journal before reconciling a complete current diagnostic page
/// set. Receipt-only rows survive repeated grouping, including during quiet hours.
pub fn grouped_notice_state(saved: &Json) -> IvyResult<Json> {
    let entries = saved.as_object().ok_or_else(IvyError::internal)?;
    let mut state = Json::Object(Vec::new());
    let mut groups: Vec<(String, Vec<(&JsString, &Json)>)> = Vec::new();
    for (old_key, entry) in entries {
        if flag(entry, "receiptOnly") {
            retain_receipts(&mut state, entry, &[])?;
            continue;
        }
        let key = notice_key(entry)?;
        if let Some((_, items)) = groups.iter_mut().find(|(k, _)| k == &key) {
            items.push((old_key, entry));
        } else {
            groups.push((key, vec![(old_key, entry)]));
        }
    }
    for (key, entries) in groups {
        let open: Vec<_> = entries
            .iter()
            .copied()
            .filter(|(_, e)| flag(e, "active") || !flag(optional(e, "recovery"), "queued"))
            .collect();
        let pool = if open.is_empty() { &entries } else { &open };
        let prior = pool
            .iter()
            .find(|(k, _)| **k == JsString::from(key.as_str()))
            .unwrap_or(&pool[0])
            .1;
        let active = entries.iter().any(|(_, e)| flag(e, "active"));
        let choose = |kind| {
            let existing = optional(prior, kind);
            if present(existing) {
                existing.clone()
            } else {
                pool.iter()
                    .map(|(_, e)| optional(e, kind))
                    .find(|v| present(v))
                    .cloned()
                    .unwrap_or(Json::Null)
            }
        };
        let alert = choose("alert");
        let recovery = if active {
            Json::Null
        } else {
            choose("recovery")
        };
        let mut first = f64::INFINITY;
        let mut resolved = f64::NEG_INFINITY;
        for (_, entry) in pool {
            first = first.min(number(entry, "firstSeen")?);
            resolved = resolved.max(if optional(entry, "resolvedAt").is_null() {
                number(entry, "firstSeen")?
            } else {
                number(entry, "resolvedAt")?
            });
        }
        let kept: Vec<_> = [&alert, &recovery]
            .into_iter()
            .filter_map(|v| operation(v).cloned())
            .collect();
        state.insert(
            key,
            Json::object([
                ("code", prior.required("code")?.clone()),
                ("label", prior.required("label")?.clone()),
                ("firstSeen", Json::Number(first)),
                ("active", Json::Bool(active)),
                (
                    "resolvedAt",
                    if active {
                        Json::Null
                    } else {
                        Json::Number(resolved)
                    },
                ),
                ("alert", alert),
                ("recovery", recovery),
            ]),
        );
        for (_, entry) in entries {
            retain_receipts(&mut state, entry, &kept)?;
        }
    }
    Ok(state)
}
