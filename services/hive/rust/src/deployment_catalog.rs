//! Select deployment identities before materializing report bodies and live status.
use super::*;

struct DeploymentId {
    host: String,
    deployment: String,
    key: String,
}

fn read(call: &Call<'_>, host: &str, deployment: &str) -> IvyResult<Option<IvyJson>> {
    let row = sql(
        call.store,
        call.store
            .connection()
            .prepare_cached(
                "SELECT record_json,node_id,reported_at FROM deployment_reports
             WHERE host_id=? AND deployment_id=?",
            )
            .and_then(|mut statement| {
                statement
                    .query_row(params![host, deployment], |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                        ))
                    })
                    .optional()
            }),
    )?;
    let Some((record, node_id, at)) = row else {
        return Ok(None);
    };
    let record = decode(&record)?;
    let node = call.registry.node(
        call.store,
        &node_id.as_str().into(),
        call.runtime,
        (call.clock)(),
    )?;
    Ok(Some(IvyJson::object([
        ("record", record),
        ("serviceNodeId", node_id.into()),
        ("reportedAt", at.into()),
        (
            "stale",
            (node.get("connected").and_then(IvyJson::as_bool) != Some(true)).into(),
        ),
    ])))
}

pub(super) fn get(call: &Call<'_>, params: &IvyJson) -> IvyResult<IvyJson> {
    read(
        call,
        &text(string(params, "hostId")?),
        &text(string(params, "deploymentId")?),
    )?
    // SQLite identity columns use lossy UTF-8. Preserve exact external units.
    .filter(|value| {
        value.get("record").is_some_and(|record| {
            record.get("hostId") == params.get("hostId")
                && record.get("deploymentId") == params.get("deploymentId")
        })
    })
    .ok_or_else(|| IvyError::new("not_found", "Deployment snapshot not found."))
}

pub(super) fn list(call: &Call<'_>, params: &IvyJson) -> IvyResult<IvyJson> {
    let identity = objects::page_identity("deployments.list", params)?;
    let after = match optional_string(params, "cursor").filter(|cursor| !cursor.0.is_empty()) {
        Some(cursor) => text(&crate::paging::catalog_boundary(
            call.store, &identity, cursor,
        )?),
        None => String::new(),
    };
    let selector = |name| {
        optional_string(params, name)
            .filter(|value| !value.0.is_empty())
            .map(|value| canonical(&IvyJson::String(value.clone()), MANAGEMENT_FRAME_BYTES))
            .transpose()
    };
    let limit = params
        .get("limit")
        .and_then(IvyJson::as_f64)
        .unwrap_or(50.0) as usize;
    // Reports are stored canonically by report(). JSON fragments preserve lone
    // UTF-16 units and distinguish them from SQLite's lossy identity columns.
    // UTC admission limits timestamps to whole milliseconds in years 0000–9999:
    // their reverse-time keys are 16 decimal digits, matching the existing pager.
    // ivy_recency receives only the projected date, never a complete report.
    // The inner LIMIT prevents flattening: stream each computed key once to the
    // outer cursor filter and bounded sort instead of evaluating its date twice.
    let mut statement = sql(
        call.store,
        call.store.connection().prepare_cached(
            "SELECT host_id,deployment_id,page_key FROM (
                 SELECT host_id,deployment_id,
                     json_array(printf('%016.0f',9007199254740991-ivy_recency(
                                    json_object('updatedAt',record_json -> '$.updatedAt'),'/updatedAt')),
                                record_json -> '$.hostId',record_json -> '$.deploymentId') AS page_key
                 FROM deployment_reports
                 WHERE (?1 IS NULL OR record_json -> '$.hostId'=?1)
                   AND (?2 IS NULL OR record_json -> '$.instanceId'=?2)
                 LIMIT -1
             ) WHERE page_key>?3 ORDER BY page_key LIMIT ?4",
        ),
    )?;
    let rows = sql(
        call.store,
        statement.query_map(
            params![
                selector("hostId")?,
                selector("instanceId")?,
                after,
                (limit + 1) as i64
            ],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        ),
    )?;
    let ids = rows
        .map(|row| {
            let (host, deployment, key) = sql(call.store, row)?;
            Ok(DeploymentId {
                host,
                deployment,
                key,
            })
        })
        .collect::<IvyResult<Vec<_>>>()?;
    drop(statement);
    let mut items = Vec::new();
    let mut used = 128;
    let mut boundary = None;
    let mut more = false;
    for id in ids {
        if items.len() == limit {
            more = true;
            break;
        }
        let record = read(call, &id.host, &id.deployment)?.ok_or_else(IvyError::internal)?;
        let size = canonical(&record, MANAGEMENT_FRAME_BYTES)?.len();
        if used + size > crate::paging::PAGE_BYTES {
            if items.is_empty() {
                return Err(IvyError::new(
                    "result_too_large",
                    "A catalog entry exceeds the response limit.",
                ));
            }
            more = true;
            break;
        }
        used += size;
        items.push(record);
        boundary = Some(id.key);
    }
    let next = if more && let Some(boundary) = boundary {
        Some(call.store.cursor(&identity, &IvyJson::from(boundary))?)
    } else {
        None
    };
    Ok(crate::paging::page(items, next))
}

#[cfg(test)]
#[path = "../../../../tests/rust/hive_deployment_catalog.rs"]
mod tests;
