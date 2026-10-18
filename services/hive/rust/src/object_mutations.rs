use super::*;
use crate::store::{AuthenticatedContext, LedgerResult};
use rusqlite::params;

fn text(value: &JsString) -> String {
    String::from_utf16_lossy(&value.0)
}
fn argument<T>(value: Option<T>) -> IvyResult<T> {
    value.ok_or_else(|| IvyError::new("invalid_arguments", "Invalid object mutation arguments."))
}
fn require(condition: bool, code: &str, message: &str) -> IvyResult<()> {
    if condition {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}

/// Context and clock observations come from admitted dispatch, never parameters.
pub fn reorder(
    store: &mut Store,
    context: &AuthenticatedContext,
    parameters: &IvyJson,
    now_ms: u64,
    now: &JsString,
) -> IvyResult<IvyJson> {
    store.mutate(
        context,
        &"objects.reorder".into(),
        parameters,
        now_ms,
        |store, principal| {
            let id = argument(parameters.get("objectId").and_then(IvyJson::as_js_string))?;
            let source = raw_object(store, Location::Id(id))?;
            let parent = source.get("parentId").unwrap();
            let before = argument(parameters.get("beforeObjectId"))?;
            let before = if before.is_null() {
                None
            } else {
                Some(argument(before.as_js_string())?)
            };
            if let Some(before) = before {
                require(
                    before != id,
                    "invalid_arguments",
                    "An Object cannot be ordered before itself.",
                )?;
                let target = raw_object(store, Location::Id(before))?;
                require(
                    target.get("parentId") == Some(parent),
                    "invalid_arguments",
                    "Objects can only be reordered within the same parent.",
                )?;
            }
            let mut ids = store
                .connection()
                .prepare_cached("SELECT id FROM objects WHERE parent_id IS ? ORDER BY position,id")
                .map_err(|e| store.query_error(e))?
                .query_map([parent.as_js_string().map(text)], |row| {
                    row.get::<_, String>(0)
                })
                .map_err(|e| store.query_error(e))?
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(|e| store.query_error(e))?
                .into_iter()
                .map(JsString::from)
                .filter(|v| v != id)
                .collect::<Vec<_>>();
            let target = match before {
                None => ids.len(),
                Some(before) => ids.iter().position(|v| v == before).ok_or_else(|| {
                    IvyError::new("not_found", "The target sibling is unavailable.")
                })?,
            };
            ids.insert(target, id.clone());
            {
                let mut update = store
                    .connection()
                    .prepare_cached("UPDATE objects SET position=? WHERE id=?")
                    .map_err(|e| store.query_error(e))?;
                for (position, id) in ids.iter().enumerate() {
                    update
                        .execute(params![position as i64, text(id)])
                        .map_err(|e| store.query_error(e))?;
                }
            }
            store
                .connection()
                .execute(
                    "UPDATE objects SET updated_at=? WHERE id=?",
                    [text(now), text(id)],
                )
                .map_err(|e| store.query_error(e))?;
            let object = stat(store, Location::Id(id))?;
            store.append_core_event(
                principal,
                object.get("id").unwrap().as_js_string().unwrap(),
                object.get("currentRevision").unwrap().as_f64().unwrap() as u64,
                &"move".into(),
                parameters
                    .get("mutationId")
                    .unwrap()
                    .as_js_string()
                    .unwrap(),
                now,
                None,
            )?;
            Ok(LedgerResult::snapshot(object))
        },
    )
}

pub(super) fn refresh_navigation(store: &Store, root: &JsString) -> IvyResult<()> {
    store.connection().execute("WITH RECURSIVE navigation(id,path,depth,effective_archive) AS (
      SELECT root.id,
        CASE WHEN parent.id IS NULL THEN '/'||root.name ELSE parent.path||'/'||root.name END,
        COALESCE(parent.depth,0)+1,
        CASE WHEN root.archived_at IS NOT NULL OR COALESCE(parent.effective_archive,0)=1 THEN 1 ELSE 0 END
      FROM objects root LEFT JOIN objects parent ON parent.id=root.parent_id WHERE root.id=?
      UNION ALL
      SELECT child.id,navigation.path||'/'||child.name,navigation.depth+1,
        CASE WHEN child.archived_at IS NOT NULL OR navigation.effective_archive=1 THEN 1 ELSE 0 END
      FROM objects child JOIN navigation ON child.parent_id=navigation.id WHERE ivy_budget()
    ) UPDATE objects SET
      path=(SELECT path FROM navigation WHERE navigation.id=objects.id),
      depth=(SELECT depth FROM navigation WHERE navigation.id=objects.id),
      effective_archive=(SELECT effective_archive FROM navigation WHERE navigation.id=objects.id)
    WHERE id IN (SELECT id FROM navigation)",[text(root)]).map_err(|e|store.query_error(e))?;
    Ok(())
}

/// service_node_id is the authenticated connection binding, not a request field.
pub fn archive(
    store: &mut Store,
    context: &AuthenticatedContext,
    service_node_id: Option<&JsString>,
    parameters: &IvyJson,
    now_ms: u64,
    now: &JsString,
) -> IvyResult<IvyJson> {
    store.mutate(context,&"objects.archive".into(),parameters,now_ms,|store,principal| {
        let id=argument(parameters.get("objectId").and_then(IvyJson::as_js_string))?;
        let object=raw_object(store,Location::Id(id))?;
        if object.get("contractKey")==Some(&IvyJson::from("task-board/task")) {
            let owner=match service_node_id.filter(|s|!s.0.is_empty()) {
                Some(id)=>store.connection().query_row("SELECT service_name,principal_id FROM service_nodes WHERE id=?",[text(id)],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?))).optional().map_err(|e|store.query_error(e))?,
                None=>None,
            };
            require(owner.is_some_and(|(name,owner)|name=="task-board"&&JsString::from(owner)==principal.principal_id),"task_board_workflow_required","Use task-board.archive to archive or restore a ticket together with its Codex tasks.")?;
        }
        if let Some(expected)=parameters.get("expectedRevision") {
            require(object.get("currentRevision").unwrap()==expected,"revision_conflict","The document changed. Refresh it before archiving.")?;
        }
        let archived=argument(parameters.get("archived").and_then(IvyJson::as_bool))?;
        let previous=object.get("archivedAt").unwrap();
        let timestamp=if archived {Some(if previous.is_null(){text(now)}else{previous.as_str()?})}else{None};
        store.connection().execute("UPDATE objects SET archived_at=?,updated_at=? WHERE id=?",params![timestamp,text(now),text(id)]).map_err(|e|store.query_error(e))?;
        refresh_navigation(store,id)?;
        let object=stat(store,Location::Id(id))?;
        store.append_core_event(principal,id,object.get("currentRevision").unwrap().as_f64().unwrap() as u64,&"archive".into(),parameters.get("mutationId").unwrap().as_js_string().unwrap(),now,None)?;
        Ok(LedgerResult::snapshot(object))
    })
}
