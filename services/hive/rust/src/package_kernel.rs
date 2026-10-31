//! Package catalog composition with the kernel's live Registry and options.
use crate::{package_catalog_object::PackageCatalogObject, store::Store};
use ivy_contracts::{IvyError, IvyJson, IvyResult, JsString, json::parse};
use std::collections::HashSet;

pub type KernelCatalog = PackageCatalogObject<Box<dyn Fn(&Store) -> IvyResult<HashSet<JsString>>>>;

fn sql<T>(store: &Store, result: rusqlite::Result<T>) -> IvyResult<T> {
    result.map_err(|error| store.query_error(error))
}
fn build(value: &str) -> IvyResult<JsString> {
    parse(value, usize::MAX).map_err(|_| IvyError::internal())?
        .into_js_string().ok_or_else(IvyError::internal)
}
/// Retained host observations protect offline installations and unfinished work.
/// Registry readiness/staleness does not remove a build from this protection set.
pub fn protected_builds(
    store: &Store, build_id: &JsString,
) -> IvyResult<HashSet<JsString>> {
    let mut builds = HashSet::from([build_id.clone()]);
    let mut nodes = sql(store,store.connection().prepare_cached(
        "SELECT json_type(status_json) IS 'object' AND json_type(status_json,'$.serviceNodeId') IS 'text',
             CASE WHEN json_type(status_json,'$.buildId') IS 'text' THEN status_json -> '$.buildId' END
         FROM service_nodes ORDER BY id",
    ))?;
    for row in sql(store,nodes.query_map([],|row| Ok((row.get::<_,bool>(0)?,row.get::<_,Option<String>>(1)?))))? {
        let (valid,id) = sql(store,row)?;
        if !valid {return Err(IvyError::internal());}
        if let Some(id) = id {builds.insert(build(&id)?);}
    }
    let mut hosts = sql(store,store.connection().prepare_cached(
        "SELECT json_type(h.snapshot_json) IS 'object',n.id IS NOT NULL
         FROM host_reports h LEFT JOIN service_nodes n ON n.id=h.node_id ORDER BY h.host_id",
    ))?;
    // A missing reporter must still fail before inspecting reference arrays or
    // writing the catalog, as it did when observations were fully collected.
    for row in sql(store,hosts.query_map([],|row|Ok((row.get::<_,bool>(0)?,row.get::<_,bool>(1)?))))? {
        let (valid,reporter) = sql(store,row)?;
        if !valid {return Err(IvyError::internal());}
        if !reporter {return Err(IvyError::new("not_found","Service Node not found."));}
    }
    let mut hosts = sql(store,store.connection().prepare_cached(
        "SELECT h.host_id,(json_type(h.snapshot_json,'$.status.instances') IS 'array'
                 OR json_type(h.snapshot_json,'$.status.instances') IS 'text')
             AND (json_type(h.snapshot_json,'$.status.unfinished') IS 'array'
                 OR json_type(h.snapshot_json,'$.status.unfinished') IS 'text')
             AND NOT EXISTS (SELECT 1 FROM json_each(h.snapshot_json,'$.status.instances') WHERE type='null')
             AND NOT EXISTS (SELECT 1 FROM json_each(h.snapshot_json,'$.status.unfinished') WHERE type='null')
         FROM host_reports h ORDER BY h.host_id",
    ))?;
    let mut references = sql(store,store.connection().prepare_cached(
        // SQLite scans one host's retained JSON at a time. Only string build
        // fields enter this compact projection; opaque status/instance bodies
        // and live diagnostics never enter Rust or a global sort.
        "SELECT metadata -> b.fullkey FROM (
             SELECT json_object('executor',CASE WHEN json_type(snapshot_json,'$.status.executor.buildId') IS 'text'
                 THEN snapshot_json -> '$.status.executor.buildId' END) AS metadata
             FROM host_reports WHERE host_id=?1
             UNION ALL
             SELECT CASE WHEN e.type='object' THEN json_object(
                 'installed',CASE WHEN json_type(e.value,'$.installedBuild') IS 'text' THEN e.value -> '$.installedBuild' END,
                 'observed',CASE WHEN json_type(e.value,'$.observedBuild') IS 'text' THEN e.value -> '$.observedBuild' END)
                 ELSE '{}' END
             FROM host_reports h,json_each(h.snapshot_json,'$.status.instances') e WHERE h.host_id=?1
             UNION ALL
             SELECT CASE WHEN e.type='object' THEN json_object(
                 'previous',CASE WHEN json_type(e.value,'$.previousBuild') IS 'text' THEN e.value -> '$.previousBuild' END,
                 'target',CASE WHEN json_type(e.value,'$.targetBuild') IS 'text' THEN e.value -> '$.targetBuild' END,
                 'observed',CASE WHEN json_type(e.value,'$.observedBuild') IS 'text' THEN e.value -> '$.observedBuild' END)
                 ELSE '{}' END
             FROM host_reports h,json_each(h.snapshot_json,'$.status.unfinished') e WHERE h.host_id=?1
         ) AS selected,json_each(selected.metadata) b WHERE b.type='text'",
    ))?;
    // Retention must cover every host, including reports beyond a list page's
    // operating bound. No listing limit may discard an installed build here.
    for row in sql(store,hosts.query_map([],|row|Ok((row.get::<_,String>(0)?,row.get::<_,bool>(1)?))))? {
        let (host,arrays) = sql(store,row)?;
        if !arrays {return Err(IvyError::internal());}
        for row in sql(store,references.query_map([host],|row|row.get::<_,String>(0)))? {
            let id = build(&sql(store,row)?)?;
            if !id.0.is_empty() {builds.insert(id);}
        }
    }
    Ok(builds)
}
/// Read the current single-owner Store without retaining live status snapshots.
pub fn catalog(store: &mut Store, build_id: JsString) -> IvyResult<KernelCatalog> {
    PackageCatalogObject::new(
        store,
        Box::new(move |store: &Store| protected_builds(store,&build_id))
            as Box<dyn Fn(&Store) -> IvyResult<HashSet<JsString>>>,
    )
}

/// Typed internal requests retain the existing package.* worker wire shapes.
pub enum CatalogRequest<'a> {
    Catalog {
        after: &'a IvyJson,
        include_uis: bool,
    },
    Seed {
        catalog: &'a IvyJson,
    },
    Prune,
    Publish {
        input: &'a IvyJson,
        publisher_principal_id: &'a JsString,
    },
}
pub(crate) fn configured_principal(principals: &[JsString]) -> IvyResult<&JsString> {
    principals
        .first()
        .filter(|id| !id.0.is_empty())
        .ok_or_else(|| {
            IvyError::new(
                "service_unavailable",
                "Hive package catalog requires one configured principal.",
            )
        })
}
/// The caller supplies the current configured credential order for every request.
/// Only seed/prune require its first principal; an empty first entry is not skipped.
pub fn dispatch(
    catalog: &KernelCatalog,
    store: &mut Store,
    request: CatalogRequest<'_>,
    configured_principals: &[JsString],
    now_ms: u64,
    now: &JsString,
) -> IvyResult<IvyJson> {
    let principal = || configured_principal(configured_principals);
    match request {
        CatalogRequest::Catalog { after, include_uis } => catalog
            .list(store, after, include_uis)
            .map(|v| v.into_json()),
        CatalogRequest::Seed { catalog: value } => catalog
            .seed(store, value, principal()?, now_ms, now)
            .map(|v| v.into_json()),
        CatalogRequest::Prune => catalog
            .prune(store, principal()?, now_ms, now)
            .map(IvyJson::Array),
        CatalogRequest::Publish {
            input,
            publisher_principal_id,
        } => catalog.publish(store, input, publisher_principal_id, now_ms, now),
    }
}
