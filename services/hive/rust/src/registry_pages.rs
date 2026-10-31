//! Select compact page identities before materializing live node status.
use super::*;
use crate::{objects, paging};

// The non-flattened relation carries only names and their original positions;
// schema/catalog bodies never enter the namespace grouping or page sort.
const NAMESPACE_NAMES: &str = "SELECT n.id,e.key AS position,
        json_object('namespace',e.value -> '$.namespace') AS metadata
    FROM service_nodes AS n JOIN registries AS r ON r.node_id=n.id,
        json_each(r.registry_json,'$.namespaces') AS e
    WHERE CASE WHEN e.type='object' THEN json_type(e.value,'$.namespace')='text' END
    LIMIT -1";

struct NamespaceProviders {
    name: JsString,
    nodes: Vec<String>,
}

fn selector(params: &IvyJson, name: &str) -> IvyResult<Option<String>> {
    params.get(name).and_then(IvyJson::as_js_string).filter(|value| !value.0.is_empty())
        .map(|value| encoded(&IvyJson::String(value.clone()))).transpose()
}
fn boundary(store: &Store, identity: &IvyJson, params: &IvyJson) -> IvyResult<String> {
    match params.get("cursor").and_then(IvyJson::as_js_string).filter(|value| !value.0.is_empty()) {
        Some(cursor) => Ok(text(&paging::catalog_boundary(store, identity, cursor)?)),
        None => Ok(String::new()),
    }
}
fn page_key(source: &str) -> IvyResult<JsString> {
    parse(source, usize::MAX).map_err(|_| IvyError::internal())?
        .into_js_string().ok_or_else(IvyError::internal)
}

impl Registry {
    pub(crate) fn namespace_page(
        &self, store: &Store, params: &IvyJson, runtime: &dyn Runtime, now: f64,
    ) -> IvyResult<IvyJson> {
        let identity = objects::page_identity("namespaces.list",params)?;
        let after = boundary(store,&identity,params)?;
        let limit = params.get("limit").and_then(IvyJson::as_f64).unwrap_or(50.0) as usize;
        let mut statement = sql(store,store.connection().prepare_cached(&format!(
            // Rank first appearances by database node order and array position.
            // This preserves stable ties between distinct UTF-16 names whose
            // lossy UTF-8 page keys compare equal.
            "SELECT metadata -> '$.namespace' FROM (
                 SELECT id,position,metadata,row_number() OVER (
                     PARTITION BY CAST(json_extract(metadata,'$.namespace') AS BLOB)
                     ORDER BY id,position) AS occurrence
                 FROM ({NAMESPACE_NAMES})
             ) WHERE occurrence=1 AND ivy_value(metadata,'/namespace')>?1
             ORDER BY ivy_value(metadata,'/namespace'),id,position LIMIT ?2",
        )))?;
        let mut names = sql(store,statement.query_map(params![after,(limit+1) as i64],
            |row| row.get::<_,String>(0)))?.map(|row| page_key(&sql(store,row)?))
            .map(|name| Ok(NamespaceProviders { name:name?,nodes:Vec::new() }))
            .collect::<IvyResult<Vec<_>>>()?;
        if !names.is_empty() {
            // Select all page memberships in one scan, retaining only node IDs.
            // Re-reading every registry for each namespace would multiply work.
            let selected = encoded(&IvyJson::Array(names.iter().map(|entry| entry.name.clone().into()).collect()))?;
            let mut statement = sql(store,store.connection().prepare_cached(&format!(
                "SELECT id,metadata -> '$.namespace' FROM ({NAMESPACE_NAMES})
                 WHERE CAST(json_extract(metadata,'$.namespace') AS BLOB)
                     IN (SELECT CAST(value AS BLOB) FROM json_each(?1))
                 ORDER BY id,position",
            )))?;
            for row in sql(store,statement.query_map([selected],
                |row| Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?))))? {
                let (id,name) = sql(store,row)?;
                let name = page_key(&name)?;
                names.iter_mut().find(|entry| entry.name==name).ok_or_else(IvyError::internal)?.nodes.push(id);
            }
        }
        paging::catalog_page_with(store,names,&identity,|entry| Ok(entry.name.clone()),|entry| {
            let providers = entry.nodes.into_iter().map(|id| {
                    let node = self.node(store,&id.into(),runtime,now)?;
                    let eligible = Self::eligible(&node);
                    Ok(IvyJson::object([("node",node),("eligible",eligible.into())]))
                }).collect::<IvyResult<Vec<_>>>()?;
            Ok(IvyJson::object([("namespace",entry.name.into()),("providers",IvyJson::Array(providers))]))
        },Some(limit),None)
    }

    pub(crate) fn node_page(
        &self, store: &Store, params: &IvyJson, runtime: &dyn Runtime, now: f64,
    ) -> IvyResult<IvyJson> {
        let identity = objects::page_identity("serviceNodes.list", params)?;
        let after = boundary(store, &identity, params)?;
        let limit = params.get("limit").and_then(IvyJson::as_f64).unwrap_or(50.0) as usize;
        let mut statement = sql(store, store.connection().prepare_cached(
            // ivy_value sees only the projected ID and gives the existing
            // pager's lossy UTF-8 ordering; raw JSON retains the cursor's UTF-16.
            "SELECT id,status_json -> '$.serviceNodeId' FROM service_nodes
             WHERE (?1 IS NULL OR json_extract(status_json,'$.hostId')=json_extract(?1,'$'))
               AND (?2 IS NULL OR json_extract(status_json,'$.serviceName')=json_extract(?2,'$'))
               AND ivy_value(json_object('id',status_json -> '$.serviceNodeId'),'/id')>?3
             ORDER BY ivy_value(json_object('id',status_json -> '$.serviceNodeId'),'/id'),id LIMIT ?4",
        ))?;
        let keys = sql(store, statement.query_map(
            params![selector(params,"hostId")?,selector(params,"serviceName")?,after,(limit+1) as i64],
            |row| Ok((row.get::<_, String>(0)?,row.get::<_, String>(1)?)),
        ))?.map(|row| {
            let (id, key) = sql(store,row)?;
            Ok((id,page_key(&key)?))
        }).collect::<IvyResult<Vec<_>>>()?;
        paging::catalog_page_with(store,keys,&identity,|(_,key)| Ok(key.clone()),
            |(id,_)| self.node(store,&id.into(),runtime,now),Some(limit),None)
    }

    pub(crate) fn host_page(&self, store: &Store, params: &IvyJson) -> IvyResult<IvyJson> {
        let identity = objects::page_identity("hosts.list",params)?;
        let after = boundary(store,&identity,params)?;
        let limit = params.get("limit").and_then(IvyJson::as_f64).unwrap_or(50.0) as usize;
        let mut statement = sql(store,store.connection().prepare_cached(
            // Group on decoded bytes so escaped spellings share a host while
            // distinct lone surrogates and U+FFFD remain separate. min(id) is
            // unique and selects the first node, preserving ties in page order.
            // LIMIT -1 keeps this projection ahead of grouping, so SQLite's
            // sort carries small metadata rather than complete status bodies.
            "SELECT metadata -> '$.hostId',min(id) FROM (
                 SELECT id,json_object('hostId',status_json -> '$.hostId') AS metadata
                 FROM service_nodes LIMIT -1
             ) GROUP BY CAST(json_extract(metadata,'$.hostId') AS BLOB)
             HAVING ivy_value(metadata,'/hostId')>?1
             ORDER BY ivy_value(metadata,'/hostId'),min(id) LIMIT ?2",
        ))?;
        let keys = sql(store,statement.query_map(params![after,(limit+1) as i64],
            |row| row.get::<_,String>(0)))?.map(|row| page_key(&sql(store,row)?))
            .collect::<IvyResult<Vec<_>>>()?;
        paging::catalog_page_with(store,keys,&identity,|key| Ok(key.clone()),|host| {
            let mut statement = sql(store,store.connection().prepare_cached(
                "SELECT status_json -> '$.serviceNodeId',json_type(status_json,'$.connected') IS 'true'
                 FROM service_nodes WHERE CAST(json_extract(status_json,'$.hostId') AS BLOB)
                     =CAST(json_extract(?1,'$') AS BLOB) ORDER BY id",
            ))?;
            let mut connected = false;
            let ids = sql(store,statement.query_map([encoded(&IvyJson::String(host.clone()))?],
                |row| Ok((row.get::<_,String>(0)?,row.get::<_,bool>(1)?))))?
                .map(|row| {
                    let (id,online) = sql(store,row)?;
                    connected |= online;
                    parse(&id,usize::MAX).map_err(|_| IvyError::internal())
                }).collect::<IvyResult<Vec<_>>>()?;
            Ok(IvyJson::object([("hostId",host.into()),("serviceNodeIds",IvyJson::Array(ids)),
                ("connected",connected.into())]))
        },Some(limit),None)
    }
}
