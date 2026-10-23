//! Owning-service history pruning; the current revision and incoming pins survive.
use crate::{
    retention::{Runtime, integer},
    store::{AuthenticatedContext, LedgerResult, Store},
};
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString};
use rusqlite::{OptionalExtension, params};

fn field<'a>(value: &'a IvyJson, key: &str) -> IvyResult<&'a IvyJson> {
    value.get(key).ok_or_else(IvyError::internal)
}
fn require(condition: bool, code: &str, message: &str) -> IvyResult<()> {
    if condition {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}

/// Admitted parameters and the current bound service identity come from dispatch.
/// Keep the byte aggregate and bounded candidate selection inside the receipt.
pub fn prune_revisions(
    store: &mut Store,
    context: &AuthenticatedContext,
    service_node_id: Option<&JsString>,
    parameters: &IvyJson,
    now_ms: u64,
    runtime: &dyn Runtime,
) -> IvyResult<IvyJson> {
    store.mutate(context, &"objects.pruneRevisions".into(), parameters, now_ms, |store, principal| {
        let id = field(parameters, "objectId")?.as_str()?;
        let expected = integer(field(parameters, "expectedRevision")?).ok_or_else(IvyError::internal)?;
        let object = store.connection().query_row("SELECT current_revision,contract_key FROM objects WHERE id=?", [&id], |r| Ok((r.get::<_,i64>(0)?, r.get::<_,String>(1)?)))
            .optional().map_err(|e|store.query_error(e))?
            .ok_or_else(|| IvyError::new("not_found", "Result object does not exist."))?;
        require(object.0 == expected, "revision_conflict", "Result changed before retention.")?;
        let version = store.connection().query_row("SELECT contract_version FROM revisions WHERE object_id=? AND revision=?", params![id,expected], |r| r.get::<_,String>(0))
            .map_err(|e|store.query_error(e))?;
        let contract = store.contract(&object.1.as_str().into(), &version.into())?;
        let node = service_node_id.map(|s|String::from_utf16_lossy(&s.0));
        let owner = store.connection().query_row("SELECT service_name,principal_id FROM service_nodes WHERE id=?", [node], |r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?)))
            .optional().map_err(|e|store.query_error(e))?;
        let allowed=if let Some((service, _))=owner.filter(|(_,id)|JsString::from(id.as_str())==principal.principal_id){
            let contract_owner=field(&contract,"owner")?;
            if contract_owner.is_null(){return Err(IvyError::internal());}
            contract_owner.get("kind") == Some(&"service".into())
                && contract_owner.get("serviceName").and_then(IvyJson::as_js_string) == Some(&JsString::from(service))
        }else{false};
        require(allowed,
            "forbidden", "Only the current owning service can prune result history.")?;
        let total = store.connection().query_row("SELECT COALESCE(SUM(byte_length),0) AS bytes FROM revisions WHERE object_id=?", [&id], |r|r.get::<_,i64>(0)).map_err(|e|store.query_error(e))?;
        store.check_budget()?;
        let count = integer(field(parameters,"maximumCount")?).ok_or_else(IvyError::internal)?;
        let boundary = store.connection().query_row("SELECT revision FROM revisions WHERE object_id=? ORDER BY revision DESC LIMIT 1 OFFSET ?", params![id,count-1], |r|r.get::<_,i64>(0))
            .optional().map_err(|e|store.query_error(e))?.unwrap_or(0);
        let mut cutoff = 0_i64.max(boundary-1);
        let maximum_bytes = integer(field(parameters,"maximumBytes")?).ok_or_else(IvyError::internal)?;
        if total > maximum_bytes {
            let overflow = store.connection().query_row("SELECT revision FROM (
          SELECT revision,SUM(byte_length) OVER (ORDER BY revision DESC) AS bytes
          FROM revisions WHERE object_id=? AND revision>=? AND ivy_budget()
        ) WHERE bytes>? LIMIT 1",params![id,boundary,maximum_bytes],|r|r.get::<_,i64>(0))
                .optional().map_err(|e|store.query_error(e))?.unwrap_or(0);
            cutoff = cutoff.max(overflow);
        }
        let days = field(parameters,"maximumAgeDays")?.as_f64().ok_or_else(IvyError::internal)?;
        let before = String::from_utf16_lossy(&runtime.iso_date(now_ms as f64 - days*86_400_000.0)?.0);
        let batch = store.connection().prepare_cached("SELECT revision,byte_length FROM revisions r
       WHERE object_id=? AND revision<? AND (revision<=? OR created_at<=?)
         AND ivy_budget() AND NOT EXISTS (
           SELECT 1 FROM revision_references
           WHERE target_object_id=r.object_id AND target_revision=r.revision
         ) ORDER BY revision LIMIT 500").map_err(|e|store.query_error(e))?
            .query_map(params![id,expected,cutoff,before], |r|Ok((r.get::<_,i64>(0)?,r.get::<_,i64>(1)?)))
            .map_err(|e|store.query_error(e))?.collect::<rusqlite::Result<Vec<_>>>().map_err(|e|store.query_error(e))?;
        let protected = store.connection().query_row("SELECT COUNT(DISTINCT x.target_revision) AS count FROM revision_references x
       JOIN revisions r ON r.object_id=x.target_object_id AND r.revision=x.target_revision
       WHERE x.target_object_id=? AND r.revision<? AND (r.revision<=? OR r.created_at<=?)",params![id,expected,cutoff,before],|r|r.get::<_,i64>(0)).map_err(|e|store.query_error(e))?;
        store.check_budget()?;
        let mut removed_bytes = 0_i64;
        for (revision, bytes) in &batch {
            store.check_budget()?;
            store.connection().execute("DELETE FROM revisions WHERE object_id=? AND revision=?", params![id,revision]).map_err(|e|store.query_error(e))?;
            removed_bytes += bytes;
        }
        if !batch.is_empty() {
            store.adjust_retention_usage(&object.1.as_str().into(),0,-(batch.len() as i64),-removed_bytes)?;
            store.invalidate(format!("objects/{}",object.1).into());
            store.invalidate("system".into());
        }
        Ok(LedgerResult::snapshot(IvyJson::object([
            ("deleted",IvyJson::Number(batch.len() as f64)),
            ("protected",IvyJson::Number(protected as f64)),
            ("remainingBytes",IvyJson::Number((total-removed_bytes) as f64)),
        ])))
    })
}
