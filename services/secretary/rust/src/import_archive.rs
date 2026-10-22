//! Bounded archival into the already sealed source. Normal readers need no
//! legacy fallback; their current heads and reference-reachable rows survive.
use crate::{
    assignment_import::{import_event_gap, import_schedule_progress},
    assignment_scheduler::AssignmentScheduler,
    assignment_schema::assignment_name,
    import_record, import_state,
    inbox_evidence::InboxEvidence,
    inbox_recovery::RecoveryScan,
    journal::Document,
    retained_import::{ImportContext, failure, map_prefix, mapped, pin, source_local},
    store::Record,
};
use ivy_contracts::{
    IvyError, IvyJson, IvyResult,
    hash::{digest, hash_json},
    json::{canonical, parse},
};
use ivy_sdk::clock::parse_date;
use ivy_store::{
    rusqlite::{OptionalExtension, params},
    sqlite,
};
use std::{cell::RefCell, collections::HashSet};
const FRAME: usize = 33_554_432;
// Only these one-time conversions establish the baseline/current mappings.
const KEYS: &str = "'secretary/schedule-cursor','secretary/event-subscription','secretary/event-gap','secretary/assignment-handoff','secretary/assignment-voice','secretary/follow-up','secretary/follow-up-context'";
fn eligible(key: &str) -> bool {
    matches!(
        key,
        "secretary/schedule-cursor"
            | "secretary/event-subscription"
            | "secretary/event-gap"
            | "secretary/assignment-handoff"
            | "secretary/assignment-voice"
            | "secretary/follow-up"
            | "secretary/follow-up-context"
    )
}
fn map_key(state: &IvyJson, id: &str) -> IvyResult<String> {
    Ok(format!("{}{}", map_prefix(state)?, id))
}
fn same(a: &IvyJson, b: &IvyJson) -> IvyResult<bool> {
    Ok(canonical(a, FRAME)? == canonical(b, FRAME)?)
}
fn history_count(map: &IvyJson) -> i64 {
    map.get("history")
        .and_then(|v| v.get("count"))
        .and_then(IvyJson::as_f64)
        .unwrap_or(0.0) as i64
}
fn history_bytes(map: &IvyJson) -> i64 {
    map.get("history")
        .and_then(|v| v.get("bytes"))
        .and_then(IvyJson::as_f64)
        .unwrap_or(0.0) as i64
}
fn add_history(map: &mut IvyJson, count: i64, bytes: i64) {
    map.insert(
        "history",
        IvyJson::object([
            (
                "count",
                IvyJson::Number((history_count(map) + count) as f64),
            ),
            (
                "bytes",
                IvyJson::Number((history_bytes(map) + bytes) as f64),
            ),
        ]),
    );
}
pub(crate) fn heads(
    ctx: &ImportContext<'_>,
    state: &IvyJson,
    after: Option<&str>,
) -> IvyResult<Option<String>> {
    let rows=ctx.journal.db.prepare(&format!("SELECT id,key,name,current_revision,value_json FROM technical_documents WHERE key IN ({KEYS}) AND (? IS NULL OR id>?) ORDER BY id LIMIT ?")).map_err(sqlite::error)?.query_map(params![after,after,ctx.page_size as i64+1],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,i64>(3)?,r.get::<_,String>(4)?))).map_err(sqlite::error)?.collect::<Result<Vec<_>,_>>().map_err(sqlite::error)?;
    let more = rows.len() > ctx.page_size;
    let transaction = ctx
        .journal
        .db
        .unchecked_transaction()
        .map_err(sqlite::error)?;
    for (id, key, name, revision, raw) in rows.iter().take(ctx.page_size) {
        let source = source_local(ctx.source, id)?;
        if source.get("key") != Some(&key.clone().into())
            || source.get("name") != Some(&name.clone().into())
            || source.required("pin")?.get("revision") != Some(&IvyJson::Number(*revision as f64))
            || source.get("rawHash") != Some(&digest(raw.as_bytes()).into())
        {
            return Err(failure(
                "The original local head no longer matches its sealed source.",
            ));
        }
        let key = map_key(state, id)?;
        if import_state::read(ctx.journal, &key)?.is_none() {
            import_state::save(
                ctx.journal,
                &key,
                &IvyJson::object([
                    ("source", source.clone()),
                    (
                        "target",
                        IvyJson::object([
                            ("kind", "local-head".into()),
                            ("pin", source.required("pin")?.clone()),
                            ("rawHash", source.required("rawHash")?.clone()),
                        ]),
                    ),
                ]),
            )?;
        }
    }
    transaction.commit().map_err(sqlite::error)?;
    Ok(if more {
        Some(rows[ctx.page_size - 1].0.clone())
    } else {
        None
    })
}
/// Archive only old baseline revisions, keeping the exact original current head
/// as a verified local target before any conversion can need additional room.
pub(crate) fn history(ctx: &ImportContext<'_>, state: &IvyJson) -> IvyResult<Option<String>> {
    let (after_id, after_revision) =
        if let Some(cursor) = state.get("cursor").filter(|v| !v.is_null()) {
            let value = parse(&cursor.as_str()?, FRAME)?;
            let parts = value
                .as_array()
                .filter(|v| v.len() == 2)
                .ok_or_else(IvyError::internal)?;
            (
                parts[0].as_str()?,
                parts[1].as_f64().ok_or_else(IvyError::internal)? as i64,
            )
        } else {
            (String::new(), 0)
        };
    let sql = format!(
        "SELECT r.id,r.revision,r.value_json FROM technical_revisions r JOIN technical_documents d ON d.id=r.id WHERE d.key IN ({KEYS}) AND r.revision<d.current_revision AND (r.id>? OR (r.id=? AND r.revision>?)) ORDER BY r.id,r.revision LIMIT ?"
    );
    let rows = ctx
        .journal
        .db
        .prepare(&sql)
        .map_err(sqlite::error)?
        .query_map(
            params![after_id, after_id, after_revision, ctx.page_size as i64 + 1],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, String>(2)?,
                ))
            },
        )
        .map_err(sqlite::error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sqlite::error)?;
    let more = rows.len() > ctx.page_size;
    let transaction = ctx
        .journal
        .db
        .unchecked_transaction()
        .map_err(sqlite::error)?;
    for (id, revision, raw) in rows.iter().take(ctx.page_size) {
        if *revision < 1 {
            return Err(failure("A retained technical revision is invalid."));
        }
        let source = source_local(ctx.source, id)?;
        let original: String = ctx
            .source
            .query_row(
                "SELECT value_json FROM technical_revisions WHERE id=? AND revision=?",
                params![id, revision],
                |r| r.get(0),
            )
            .map_err(sqlite::error)?;
        let head: (i64, String) = ctx
            .journal
            .db
            .query_row(
                "SELECT current_revision,value_json FROM technical_documents WHERE id=?",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(sqlite::error)?;
        if original != *raw
            || source.required("pin")?.get("revision") != Some(&IvyJson::Number(head.0 as f64))
            || source.get("rawHash") != Some(&digest(head.1.as_bytes()).into())
        {
            return Err(failure(
                "The technical history no longer matches its sealed baseline.",
            ));
        }
        let key = map_key(state, id)?;
        let mut map = import_state::read(ctx.journal, &key)?.unwrap_or_else(|| {
            IvyJson::object([
                ("source", source.clone()),
                (
                    "target",
                    IvyJson::object([
                        ("kind", "local-head".into()),
                        ("pin", source.required("pin").unwrap().clone()),
                        ("rawHash", source.required("rawHash").unwrap().clone()),
                    ]),
                ),
            ])
        });
        if map.get("source") != Some(&source) {
            return Err(failure("The original history mapping changed."));
        }
        add_history(&mut map, 1, raw.len() as i64);
        import_state::save(ctx.journal, &key, &map)?;
        if ctx
            .journal
            .db
            .execute(
                "DELETE FROM technical_revisions WHERE id=? AND revision=? AND value_json=?",
                params![id, revision, raw],
            )
            .map_err(sqlite::error)?
            != 1
        {
            return Err(failure("The technical history changed during archival."));
        }
    }
    transaction.commit().map_err(sqlite::error)?;
    Ok(if more {
        let last = &rows[ctx.page_size - 1];
        Some(canonical(
            &IvyJson::Array(vec![last.0.clone().into(), IvyJson::Number(last.1 as f64)]),
            FRAME,
        )?)
    } else {
        None
    })
}
/// Scan each current local head once in bounded pages. History in the sealed
/// baseline stays readable; live inbound references retain their original head.
pub(crate) fn references(
    ctx: &ImportContext<'_>,
    state: &IvyJson,
    technical: bool,
    after: Option<&str>,
) -> IvyResult<Option<String>> {
    let table = if technical {
        "technical_documents"
    } else {
        "documents"
    };
    let rows = ctx
        .journal
        .db
        .prepare(&format!(
            "SELECT id,value_json FROM {table} WHERE (? IS NULL OR id>?) ORDER BY id LIMIT ?"
        ))
        .map_err(sqlite::error)?
        .query_map(params![after, after, ctx.page_size as i64 + 1], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })
        .map_err(sqlite::error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sqlite::error)?;
    let ids = ctx
        .journal
        .db
        .prepare(&format!(
            "SELECT id FROM technical_documents WHERE key IN ({KEYS}) LIMIT ?"
        ))
        .map_err(sqlite::error)?
        .query_map([ctx.journal.limits.count + 1], |r| r.get::<_, String>(0))
        .map_err(sqlite::error)?
        .collect::<Result<HashSet<_>, _>>()
        .map_err(sqlite::error)?;
    if ids.len() as i64 > ctx.journal.limits.count {
        return Err(failure(
            "The retained technical head count exceeds the journal budget.",
        ));
    }
    let more = rows.len() > ctx.page_size;
    let transaction = ctx
        .journal
        .db
        .unchecked_transaction()
        .map_err(sqlite::error)?;
    for (id, raw) in rows.iter().take(ctx.page_size) {
        let value = parse(raw, FRAME)?;
        let mut pending = vec![&value];
        let mut found = HashSet::new();
        while let Some(value) = pending.pop() {
            match value {
                IvyJson::String(text) => {
                    if let Ok(text) = text.to_utf8() {
                        if ids.contains(&text) && (!technical || &text != id) {
                            found.insert(text);
                        }
                    }
                }
                IvyJson::Array(values) => pending.extend(values),
                IvyJson::Object(values) => pending.extend(values.iter().map(|(_, v)| v)),
                _ => {}
            }
        }
        for target in found {
            let key = map_key(state, &target)?;
            if let Some(mut map) = import_state::read(ctx.journal, &key)? {
                map.insert("referenced", true.into());
                import_state::save(ctx.journal, &key, &map)?;
            }
        }
    }
    transaction.commit().map_err(sqlite::error)?;
    Ok(if more {
        Some(rows[ctx.page_size - 1].0.clone())
    } else {
        None
    })
}
pub(crate) async fn archived_assignment(
    ctx: &ImportContext<'_>,
    state: &IvyJson,
    scheduler: &AssignmentScheduler<'_, '_, '_>,
    row: &IvyJson,
) -> IvyResult<()> {
    let selected = import_record::read(
        scheduler.evidence.store,
        Record::Assignment,
        row.required("objectId")?,
    )
    .await?;
    let id = selected
        .required("value")?
        .required("assignmentId")?
        .as_js_string()
        .ok_or_else(IvyError::internal)?;
    let values = row.required("values")?;
    if values.get("data:/assignmentId") != Some(&IvyJson::String(id.clone()))
        || values.get("object.name") != Some(&assignment_name(id)?.into())
    {
        return Err(failure("The archived assignment identity changed."));
    }
    if let Some(imported) = import_schedule_progress(
        scheduler,
        &selected,
        state
            .required("at")?
            .as_f64()
            .ok_or_else(IvyError::internal)? as i64,
    )
    .await?
    {
        mapped(
            ctx,
            state,
            &imported.source.pin.object_id,
            IvyJson::object([
                ("kind", "schedule-progress".into()),
                ("pin", imported.after.required("pin")?.clone()),
                ("hash", hash_json(imported.after.required("value")?)?.into()),
            ]),
        )?;
    }
    import_event_gap(scheduler, &selected, &RefCell::new(RecoveryScan::default())).await?;
    if let Some(current) = ctx.journal.named(
        "secretary/event-gap",
        &String::from_utf16_lossy(&id.0),
        true,
    )? {
        mapped(
            ctx,
            state,
            &current.pin.object_id,
            IvyJson::object([
                ("kind", "current-event-gap".into()),
                ("pin", pin(&current)),
                ("hash", hash_json(&current.value)?.into()),
            ]),
        )?;
    }
    Ok(())
}
async fn absent_assignment(
    ctx: &ImportContext<'_>,
    state: &IvyJson,
    evidence: &InboxEvidence<'_, '_>,
    row: &Document,
    key: &str,
) -> IvyResult<()> {
    let valid = match key {
        "secretary/schedule-cursor" => ["updatedAt", "checkedAt"].iter().all(|k| {
            row.value
                .get(k)
                .and_then(IvyJson::as_js_string)
                .is_some_and(|v| parse_date(v).is_finite())
        }),
        "secretary/event-gap" => row
            .value
            .get("prunedThroughSequence")
            .and_then(IvyJson::as_f64)
            .is_some_and(|n| {
                n.is_finite() && n >= 0.0 && n.fract() == 0.0 && n <= 9_007_199_254_740_991.0
            }),
        _ => false,
    };
    if !valid
        || ivy_store::receipts::has_unresolved_outcome(&row.value)?
        || row.value.get("operationId").is_some()
        || row.value.get("request").is_some()
    {
        return Err(failure(
            "A retained orphan has no verifiable inert assignment state.",
        ));
    }
    let request = IvyJson::object([
        ("contractKey", Record::Assignment.key().into()),
        ("includeArchived", true.into()),
        ("limit", IvyJson::Number(2.0)),
        (
            "where",
            IvyJson::object([
                ("op", "and".into()),
                (
                    "args",
                    IvyJson::Array(vec![
                        IvyJson::object([
                            ("op", "eq".into()),
                            ("field", "object.parentId".into()),
                            ("value", IvyJson::String(evidence.store.root.clone())),
                        ]),
                        IvyJson::object([
                            ("op", "eq".into()),
                            ("field", "data:/assignmentId".into()),
                            ("value", row.name.clone().into()),
                        ]),
                    ]),
                ),
            ]),
        ),
    ]);
    let result = ctx
        .client
        .request("objects.query", request.clone(), ctx.cancellation.clone())
        .await?;
    if !result
        .required("items")?
        .as_array()
        .ok_or_else(IvyError::internal)?
        .is_empty()
        || result.get("nextCursor").is_some_and(|v| !v.is_null())
    {
        return Err(failure("An unmapped original assignment still exists."));
    }
    mapped(
        ctx,
        state,
        &row.pin.object_id,
        IvyJson::object([("kind", "absent-assignment".into()), ("query", request)]),
    )
}
async fn verify(
    ctx: &ImportContext<'_>,
    evidence: &InboxEvidence<'_, '_>,
    row: &Document,
    target: &IvyJson,
) -> IvyResult<bool> {
    let mut keep = false;
    let kind = target.required("kind")?.as_str()?;
    let record = match kind.as_str() {
        "schedule-progress" => Some(Record::ScheduleProgress),
        "delivery" | "voice" => Some(Record::Execution),
        _ => None,
    };
    if let Some(record) = record {
        let current = import_record::read(
            evidence.store,
            record,
            target.required("pin")?.required("objectId")?,
        )
        .await?;
        if current.get("pin") != target.get("pin")
            || target.get("hash") != Some(&hash_json(current.required("value")?)?.into())
        {
            return Err(failure(
                "The current import target changed before archival.",
            ));
        }
        if matches!(kind.as_str(), "delivery" | "voice")
            && ivy_store::receipts::has_unresolved_outcome(&row.value)?
        {
            keep = current
                .required("value")?
                .get(&kind)
                .is_none_or(|value| !same(value, &row.value).unwrap_or(false));
        }
    } else if matches!(
        kind.as_str(),
        "context-link" | "follow-up-context" | "event-gap" | "current-event-gap"
    ) {
        if target.get("pin") != Some(&pin(row))
            || target.get("hash") != Some(&hash_json(&row.value)?.into())
        {
            return Err(failure(
                "The current local import target changed before archival.",
            ));
        }
    } else if matches!(
        kind.as_str(),
        "event-subscription" | "archived-subscription"
    ) {
        let receipt = target.required("receipt")?;
        let requests = receipt
            .required("requests")?
            .as_array()
            .ok_or_else(IvyError::internal)?;
        if receipt.get("completed").and_then(IvyJson::as_f64) != Some(requests.len() as f64) {
            return Err(failure("Original subscription retirement is unresolved."));
        }
    } else if kind != "absent-assignment" {
        return Err(failure(
            "A retained legacy record has no verified import target.",
        ));
    }
    let _ = ctx;
    Ok(keep)
}
pub(crate) async fn archive(
    ctx: &ImportContext<'_>,
    state: &IvyJson,
    evidence: &InboxEvidence<'_, '_>,
    row: &Document,
) -> IvyResult<()> {
    let key: String = ctx
        .journal
        .db
        .query_row(
            "SELECT key FROM technical_documents WHERE id=?",
            [&row.pin.object_id],
            |r| r.get(0),
        )
        .map_err(sqlite::error)?;
    if !eligible(&key) {
        return Ok(());
    }
    let mapping_key = map_key(state, &row.pin.object_id)?;
    let mapping = import_state::read(ctx.journal, &mapping_key)?;
    if mapping
        .as_ref()
        .and_then(|v| v.get("target"))
        .and_then(|t| t.get("kind"))
        .is_none_or(|v| v == &"local-head".into())
    {
        if key == "secretary/follow-up-context"
            && (row
                .value
                .get("threadId")
                .and_then(IvyJson::as_js_string)
                .is_some_and(|s| !s.0.is_empty())
                || row.value.get("start").is_some_and(|v| !v.is_null()))
        {
            mapped(
                ctx,
                state,
                &row.pin.object_id,
                IvyJson::object([
                    ("kind", "follow-up-context".into()),
                    ("pin", pin(row)),
                    ("hash", hash_json(&row.value)?.into()),
                ]),
            )?;
        } else {
            absent_assignment(ctx, state, evidence, row, &key).await?;
        }
    }
    let mut map = import_state::read(ctx.journal, &mapping_key)?.ok_or_else(IvyError::internal)?;
    if map.get("source") != Some(&source_local(ctx.source, &row.pin.object_id)?) {
        return Err(failure("The archived source identity changed."));
    }
    let unresolved_shadow = verify(ctx, evidence, row, map.required("target")?).await?;
    evidence.verify_owner().await?;
    let source_revision = map
        .required("source")?
        .required("pin")?
        .required("revision")?
        .as_f64()
        .ok_or_else(IvyError::internal)? as i64;
    let obsolete = matches!(
        key.as_str(),
        "secretary/schedule-cursor"
            | "secretary/event-subscription"
            | "secretary/assignment-handoff"
            | "secretary/assignment-voice"
    ) || map.required("target")?.get("kind") == Some(&"absent-assignment".into());
    let remove = obsolete && map.get("referenced") != Some(&true.into()) && !unresolved_shadow;
    let original: Option<String> = ctx
        .journal
        .db
        .query_row(
            "SELECT value_json FROM technical_revisions WHERE id=? AND revision=?",
            params![row.pin.object_id, source_revision],
            |r| r.get(0),
        )
        .optional()
        .map_err(sqlite::error)?;
    if original.is_none() && map.get("archive").is_none() {
        return Err(failure(
            "The original current revision is missing before archival.",
        ));
    }
    let transaction = ctx
        .journal
        .db
        .unchecked_transaction()
        .map_err(sqlite::error)?;
    if let Some(raw) = original {
        let source: String = ctx
            .source
            .query_row(
                "SELECT value_json FROM technical_revisions WHERE id=? AND revision=?",
                params![row.pin.object_id, source_revision],
                |r| r.get(0),
            )
            .map_err(sqlite::error)?;
        if source != raw {
            return Err(failure(
                "The archived revision differs from its sealed original.",
            ));
        }
        if remove || row.pin.revision > source_revision {
            ctx.journal
                .db
                .execute(
                    "DELETE FROM technical_revisions WHERE id=? AND revision=?",
                    params![row.pin.object_id, source_revision],
                )
                .map_err(sqlite::error)?;
            add_history(&mut map, 1, raw.len() as i64);
        }
    }
    if remove {
        if row.pin.revision != source_revision {
            return Err(failure(
                "An obsolete local head changed after its source mapping.",
            ));
        }
        if ctx
            .journal
            .db
            .execute(
                "DELETE FROM technical_documents WHERE id=? AND key=? AND current_revision=?",
                params![row.pin.object_id, key, row.pin.revision],
            )
            .map_err(sqlite::error)?
            != 1
        {
            return Err(failure("The obsolete local head changed during archival."));
        }
    }
    map.insert(
        "archive",
        IvyJson::object([
            (
                "mode",
                if remove {
                    "sealed-source"
                } else {
                    "current-head"
                }
                .into(),
            ),
            ("currentPin", if remove { IvyJson::Null } else { pin(row) }),
            ("unresolvedShadow", unresolved_shadow.into()),
        ]),
    );
    import_state::save(ctx.journal, &mapping_key, &map)?;
    transaction.commit().map_err(sqlite::error)?;
    Ok(())
}
