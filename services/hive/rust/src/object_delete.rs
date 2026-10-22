use super::*;
use crate::store::{AuthenticatedContext, LedgerResult};
use rusqlite::{params, types::Value};
use std::collections::HashSet;

struct Selected {
    id: JsString,
    parent: Option<JsString>,
    owner: Option<JsString>,
    key: JsString,
    revision: i64,
    depth: i64,
}
fn text(value: &JsString) -> String {
    String::from_utf16_lossy(&value.0)
}
fn row(store: &Store, id: &JsString) -> IvyResult<Selected> {
    store.connection().query_row(
        "SELECT id,parent_id,owner_object_id,contract_key,current_revision,depth FROM objects WHERE id=?",
        [text(id)],
        |r| Ok(Selected {
            id: r.get::<_,String>(0)?.into(),
            parent: r.get::<_,Option<String>>(1)?.map(JsString::from),
            owner: r.get::<_,Option<String>>(2)?.map(JsString::from),
            key: r.get::<_,String>(3)?.into(),
            revision: r.get(4)?,
            depth: r.get(5)?,
        }),
    ).optional().map_err(|e| store.query_error(e))?
        .ok_or_else(|| IvyError::new("not_found", "Object not found."))
}
fn require(condition: bool, code: &str, message: &str) -> IvyResult<()> {
    if condition {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn ids(store: &Store, sql: &str, values: impl rusqlite::Params) -> IvyResult<Vec<JsString>> {
    store
        .connection()
        .prepare_cached(sql)
        .map_err(|e| store.query_error(e))?
        .query_map(values, |r| r.get::<_, String>(0))
        .map_err(|e| store.query_error(e))?
        .map(|v| v.map(JsString::from))
        .collect::<rusqlite::Result<_>>()
        .map_err(|e| store.query_error(e))
}
fn normalize_positions(store: &Store, parent: Option<&JsString>) -> IvyResult<()> {
    let rows = store
        .connection()
        .prepare_cached("SELECT id,position FROM objects WHERE parent_id IS ? ORDER BY position,id")
        .map_err(|e| store.query_error(e))?
        .query_map([parent.map(text)], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
        })
        .map_err(|e| store.query_error(e))?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|e| store.query_error(e))?;
    let mut update = store
        .connection()
        .prepare_cached("UPDATE objects SET position=? WHERE id=?")
        .map_err(|e| store.query_error(e))?;
    for (position, (id, previous)) in rows.into_iter().enumerate() {
        if previous != position as i64 {
            update
                .execute(params![position as i64, id])
                .map_err(|e| store.query_error(e))?;
        }
    }
    Ok(())
}

/// Permanent deletion for admitted arguments; all effects share the receipt transaction.
pub fn delete(
    store: &mut Store,
    context: &AuthenticatedContext,
    parameters: &IvyJson,
    now_ms: u64,
    now: &JsString,
) -> IvyResult<IvyJson> {
    store.mutate(context, &"objects.delete".into(), parameters, now_ms, |store, principal| {
        let id = parameters.get("objectId").and_then(IvyJson::as_js_string)
            .ok_or_else(||IvyError::new("invalid_arguments","Invalid object mutation arguments."))?;
        let root = row(store,id)?;
        require(["wiki/page","wiki/attachment","task-board/task","dashboards/dashboard"].into_iter().any(|key| root.key == JsString::from(key)),
            "invalid_arguments","Permanent deletion is available only for Wiki pages, Wiki attachments, TaskBoard tasks and dashboards.")?;
        require(parameters.get("expectedRevision").and_then(IvyJson::as_f64) == Some(root.revision as f64),
            "revision_conflict","The document changed. Refresh it before deleting.")?;
        let mut selected = HashSet::new();
        let mut selected_keys = Vec::new();
        let mut rows = Vec::new();
        let mut task_parents = Vec::new();
        let mut task_parent_set = HashSet::new();
        let mut queue = vec![id.clone()];
        let mut index = 0;
        while index < queue.len() {
            store.check_budget()?;
            let id = queue[index].clone();
            index += 1;
            if !selected.insert(id.clone()) { continue; }
            selected_keys.push(id.clone());
            let current = row(store,&id)?;
            if current.key == JsString::from("task-board/task") {
                let content = store.connection().query_row("SELECT content FROM revisions WHERE object_id=? AND revision=?",params![text(&id),current.revision],|r|r.get::<_,String>(0)).map_err(|e|store.query_error(e))?;
                let task = parse(&content,usize::MAX).map_err(|_|IvyError::internal())?;
                // JavaScript property access on a parsed null task fails before guards.
                if task.is_null() { return Err(IvyError::internal()); }
                require(["claim","publication"].iter().all(|name|task.get(name).is_none_or(IvyJson::is_null)),
                    "target_conflict","An active task or publication must finish before permanent deletion.")?;
                if task_parent_set.insert(current.parent.clone()) { task_parents.push(current.parent.clone()); }
                if !current.parent.as_ref().is_some_and(|parent| selected.contains(parent)) {
                    queue.extend(ids(store,"SELECT o.id FROM objects o JOIN revisions r
                       ON r.object_id=o.id AND r.revision=o.current_revision
                       WHERE o.contract_key IN ('task-board/run','task-board/result','task-board/review',
                         'task-board/history','task-board/delivery','task-board/comment-read-state')
                         AND o.parent_id IS ? AND r.encoding='json'
                         AND json_extract(r.content,'$.taskId')=? AND ivy_budget()",params![current.parent.as_ref().map(text),text(&id)])?);
                    if let Some(key) = task.get("taskKey").and_then(IvyJson::as_js_string).filter(|v| !v.0.is_empty()) {
                        let key = Value::Text(text(key));
                        queue.extend(ids(store,"SELECT o.id FROM objects o JOIN revisions r
                           ON r.object_id=o.id AND r.revision=o.current_revision
                           WHERE o.contract_key='task-board/task-key-allocation'
                             AND o.parent_id IS ? AND r.encoding='json'
                             AND json_extract(r.content,'$.taskKey')=? AND ivy_budget()",params![current.parent.as_ref().map(text),key])?);
                    }
                }
            }
            queue.extend(ids(store,"SELECT id FROM objects WHERE parent_id=? OR owner_object_id=?",params![text(&id),text(&id)])?);
            rows.push(current);
        }
        for parent in task_parents {
            let dependencies = store.connection().prepare_cached("SELECT o.id,dependency.value AS target FROM objects o JOIN revisions r
               ON r.object_id=o.id AND r.revision=o.current_revision
               JOIN json_each(CASE WHEN r.encoding='json' THEN r.content ELSE '{}' END,
                 '$.fields.dependencies') dependency
               WHERE o.contract_key='task-board/task' AND o.parent_id IS ? AND ivy_budget()").map_err(|e|store.query_error(e))?
                .query_map([parent.as_ref().map(text)], |r|Ok((r.get::<_,String>(0)?,value(r,"target")?))).map_err(|e|store.query_error(e))?
                .collect::<rusqlite::Result<Vec<_>>>().map_err(|e|store.query_error(e))?;
            for (source,target) in dependencies {
                let target = target.as_js_string().ok_or_else(|| IvyError::new("storage_invalid", "Task dependency identity must be a string."))?;
                require(!selected.contains(target) || selected.contains(&JsString::from(source)),
                    "target_conflict","Another ticket depends on this ticket. Remove that dependency before deleting.")?;
            }
        }
        for id in &selected_keys {
            let external = ids(store,"SELECT DISTINCT source_object_id FROM revision_references WHERE target_object_id=?",[text(id)])?
                .into_iter().any(|source|!selected.contains(&source));
            require(!external,"target_conflict","Another document references this content. Remove that reference before deleting.")?;
        }
        for row in &rows {
            let (revisions,bytes) = store.connection().query_row("SELECT COUNT(*) AS revisions,COALESCE(SUM(byte_length),0) AS bytes FROM revisions WHERE object_id=?",[text(&row.id)],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,i64>(1)?))).map_err(|e|store.query_error(e))?;
            store.adjust_retention_usage(&row.key,-1,-revisions,-bytes)?;
            store.connection().execute("DELETE FROM revision_references WHERE source_object_id=?",[text(&row.id)]).map_err(|e|store.query_error(e))?;
        }
        for row in &rows {
            store.connection().execute("DELETE FROM object_fts WHERE object_id=?",[text(&row.id)]).map_err(|e|store.query_error(e))?;
            store.connection().execute("DELETE FROM revisions WHERE object_id=?",[text(&row.id)]).map_err(|e|store.query_error(e))?;
        }
        rows.sort_by(|a,b| {
            let owned = |row:&Selected|row.owner.as_ref().is_some_and(|v|!v.0.is_empty());
            owned(b).cmp(&owned(a)).then_with(||b.depth.cmp(&a.depth))
        });
        let mut parents = Vec::new();
        let mut parent_set = HashSet::new();
        for row in rows {
            store.connection().execute("DELETE FROM objects WHERE id=?",[text(&row.id)]).map_err(|e|store.query_error(e))?;
            if parent_set.insert(row.parent.clone()) { parents.push(row.parent); }
        }
        for parent in parents {
            if parent.as_ref().is_none_or(|id|!selected.contains(id)) { normalize_positions(store,parent.as_ref())?; }
        }
        store.append_core_event(principal,id,root.revision as u64,&"delete".into(),parameters.get("mutationId").unwrap().as_js_string().unwrap(),now,Some(&root.key))?;
        Ok(LedgerResult::snapshot(IvyJson::object([("deleted",IvyJson::Bool(true))])))
    })
}
