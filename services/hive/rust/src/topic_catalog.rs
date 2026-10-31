//! Stream topic snapshot bodies; sort and page only compact record identities.
use super::*;
use ivy_contracts::hash::hash_array;

const NODE_FILTER: &str = "(?1 IS NULL OR n.id=?1)
    AND (?2 IS NULL OR CAST(json_extract(n.status_json,'$.hostId') AS BLOB)=CAST(json_extract(?2,'$') AS BLOB))
    AND (?3 IS NULL OR CAST(json_extract(n.status_json,'$.serviceName') AS BLOB)=CAST(json_extract(?3,'$') AS BLOB))";
const NAMESPACE_FILTER: &str = "CASE WHEN ns.type='object' THEN (?4 IS NULL OR CAST(json_extract(ns.value,'$.namespace') AS BLOB)=CAST(json_extract(?4,'$') AS BLOB)) ELSE 0 END";
const PAGE_KEY: &str = "coalesce(ivy_value(metadata,'/serviceNodeId'),'')||char(31)||
    coalesce(ivy_value(metadata,'/namespace'),'')||char(31)||
    coalesce(ivy_value(metadata,'/topic'),'')||char(31)||coalesce(ivy_value(metadata,'/version'),'')";
const PREFIX: &str = "(?5 IS NULL OR ivy_starts_with(metadata,'/topic',?5))";

struct TopicId {
    node: Option<String>,
    namespace: i64,
    topic: i64,
    metadata: IvyJson,
}
fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<(Option<String>,i64,i64,String)> {
    Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?))
}
fn entry(store: &Store, row: rusqlite::Result<(Option<String>,i64,i64,String)>) -> IvyResult<TopicId> {
    let (node,namespace,topic,metadata)=sql(store,row)?;
    Ok(TopicId {node,namespace,topic,metadata:decode(&metadata)?})
}
fn selector(value: Option<&IvyJson>) -> IvyResult<Option<String>> {
    value.and_then(IvyJson::as_js_string).filter(|value| !value.0.is_empty())
        .map(|value| canonical(&IvyJson::String(value.clone()),MANAGEMENT_FRAME_BYTES)).transpose()
}
fn definition(store: &Store, id: &TopicId) -> IvyResult<IvyJson> {
    let source = if let Some(node)=&id.node {
        sql(store,store.connection().query_row(
            "SELECT registry_json -> ('$.namespaces['||?2||'].topics['||?3||']') FROM registries WHERE node_id=?1",
            params![node,id.namespace,id.topic],|row|row.get::<_,String>(0)))?
    } else {
        sql(store,store.connection().query_row("SELECT definition FROM topics WHERE rowid=?",
            [id.topic],|row|row.get::<_,String>(0)))?
    };
    wire_definition(&source,id.node.is_none())
}
fn wire_definition(source: &str, builtin: bool) -> IvyResult<IvyJson> {
    let mut topic=decode(source)?;
    if builtin {
        topic.insert("title","Object changes".into());
        topic.insert("description","A Hive object is written, moved, archived or deleted according to the selected topic version.".into());
        topic.insert("eventKinds",IvyJson::Array(vec![]));
    }
    Ok(topic)
}
fn page_key(id: &TopicId) -> IvyResult<JsString> {
    let mut out=string(&id.metadata,"serviceNodeId")?.clone();
    for name in ["namespace","topic","version"] {
        out.0.push(31);out.0.extend(key(&id.metadata,name).0);
    }
    Ok(out)
}
pub(super) fn list(
    store: &Store, registry: &Registry, params: &IvyJson, runtime: &dyn Runtime, now: f64,
) -> IvyResult<IvyJson> {
    let resource=params.get("resourceRef");
    let id=params.get("serviceNodeId").filter(|value|!value.is_null())
        .or_else(||resource.and_then(|value|value.get("serviceNodeId")));
    let namespace=params.get("namespace").filter(|value|!value.is_null())
        .or_else(||resource.and_then(|value|value.get("namespace")));
    for (name,message) in [("serviceNodeId","Resource and node selectors disagree."),
        ("namespace","Resource and namespace selectors disagree.")] {
        require(!optional_string(params,name).is_some_and(|value|!value.0.is_empty())
            || !resource.is_some_and(|value|value.as_object().is_some())
            || params.get(name)==resource.and_then(|value|value.get(name)),"target_conflict",message)?;
    }
    let node_id=id.and_then(IvyJson::as_js_string).filter(|id|!id.0.is_empty());
    let namespace_name=namespace.and_then(IvyJson::as_js_string).filter(|name|!name.0.is_empty());
    if let Some(id)=node_id {
        let node=registry.node(store,id,runtime,now)?;
        require(selected(params,"hostId",node.get("hostId"))&&selected(params,"serviceName",node.get("serviceName")),
            "target_conflict","The selected resource has a different owner.")?;
        if let Some(namespace)=namespace_name {registry.check_namespace(store,string(&node,"serviceNodeId")?,namespace)?;}
    }
    let include_hive=node_id.is_none()&&optional_string(params,"hostId").is_none_or(|value|value.0.is_empty())
        && namespace_name.is_none_or(|value|value.eq_str("hive"))
        && optional_string(params,"serviceName").is_none_or(|value|value.0.is_empty()||value.eq_str("hive"));
    let node_id=node_id.map(text);
    let host=selector(params.get("hostId"))?;let service=selector(params.get("serviceName"))?;
    let namespace=selector(namespace)?;let prefix=selector(params.get("topicPrefix"))?;
    let bindings=params![node_id,host,service,namespace,prefix,include_hive];
    // Retained registry shape errors precede hashing and cursor admission.
    let mut statement=sql(store,store.connection().prepare_cached(&format!(
        "SELECT json_type(r.registry_json),json_type(r.registry_json,'$.namespaces'),
            CASE WHEN ns.type='object' THEN json_type(ns.value,'$.topics') END
         FROM service_nodes n JOIN registries r ON r.node_id=n.id
         LEFT JOIN json_each(r.registry_json,'$.namespaces') ns
             ON ns.type='object' AND {NAMESPACE_FILTER}
         WHERE {NODE_FILTER} ORDER BY n.id,ns.key")))?;
    for row in sql(store,statement.query_map(params![node_id,host,service,namespace],|row|
        Ok((row.get::<_,String>(0)?,row.get::<_,Option<String>>(1)?,row.get::<_,Option<String>>(2)?))))? {
        let (kind,names,topics)=sql(store,row)?;
        if kind!="null"&&(kind!="object"||names.as_deref().is_some_and(|kind|!matches!(kind,"null"|"array"))
            ||topics.as_deref().is_some_and(|kind|!matches!(kind,"null"|"array"))) {return Err(IvyError::internal());}
    }
    // LIMIT -1 prevents flattening: sort/hash traversal carries no schema body.
    let records=format!("SELECT n.id AS node_id,ns.key AS nspos,t.key AS topicpos,0 AS builtin,
        json_object('serviceNodeId',n.status_json -> '$.serviceNodeId',
            'namespace',ns.value -> '$.namespace','hasNamespace',json_type(ns.value,'$.namespace') IS NOT NULL,
            'topic',CASE WHEN t.type='object' THEN t.value -> '$.topic' END,
            'version',CASE WHEN t.type='object' THEN t.value -> '$.version' END) AS metadata
        FROM service_nodes n JOIN registries r ON r.node_id=n.id,
            json_each(r.registry_json,'$.namespaces') ns,
            json_each(CASE WHEN ns.type='object' THEN ns.value ELSE '{{}}' END,'$.topics') t
        WHERE {NODE_FILTER} AND ns.type='object' AND {NAMESPACE_FILTER} AND t.key IS NOT NULL
        UNION ALL SELECT NULL,0,rowid,1,
            json_object('serviceNodeId','hive','namespace','hive','hasNamespace',1,
                'topic',definition -> '$.topic','version',definition -> '$.version')
        FROM topics WHERE owner='hive' AND ?6 LIMIT -1");
    // Validate prefix/name projections before snapshot encoding, preserving
    // their priority even if a later record fails canonical hash admission.
    let mut statement=sql(store,store.connection().prepare_cached(&format!(
        "SELECT node_id,nspos,topicpos,metadata FROM ({records})
         ORDER BY builtin,node_id,nspos,topicpos")))?;
    for row in sql(store,statement.query_map(bindings,from_row))? {
        let record=entry(store,row)?;
        if prefixed(&record.metadata,"topic",params.get("topicPrefix"))?
            && record.metadata.get("hasNamespace").and_then(IvyJson::as_f64)!=Some(1.0) {
            return Err(IvyError::internal());
        }
    }
    let snapshot={
        // Order compact node identities first. CROSS JOIN keeps that traversal
        // outermost; json_each emits namespace/topic positions in array order.
        // Bodies are yielded directly, never sorted or re-opened for each topic.
        let stream=format!("SELECT json_object('serviceNodeId',json(n.node_name),
                'namespace',ns.value -> '$.namespace',
                'topic',CASE WHEN t.type='object' THEN t.value -> '$.topic' END) AS metadata,
                CASE WHEN t.type IN ('object','array') THEN t.value ELSE
                    r.registry_json -> ('$.namespaces['||ns.key||'].topics['||t.key||']') END,0
            FROM (SELECT n.id,n.status_json -> '$.serviceNodeId' AS node_name FROM service_nodes n
                    WHERE {NODE_FILTER} ORDER BY n.id LIMIT -1) n
            CROSS JOIN registries r ON r.node_id=n.id
            CROSS JOIN json_each(r.registry_json,'$.namespaces') ns
            CROSS JOIN json_each(CASE WHEN ns.type='object' THEN ns.value ELSE '{{}}' END,'$.topics') t
            WHERE ns.type='object' AND {NAMESPACE_FILTER} AND t.key IS NOT NULL AND {PREFIX}
            UNION ALL SELECT json_object('serviceNodeId','hive','namespace','hive','topic',definition -> '$.topic'),
                definition,1 FROM topics WHERE owner='hive' AND ?6 AND
                    (?5 IS NULL OR ivy_starts_with(json_object('topic',definition -> '$.topic'),'/topic',?5))");
        let mut statement=sql(store,store.connection().prepare_cached(&stream))?;
        let rows=sql(store,statement.query_map(bindings,|row|Ok((row.get::<_,String>(0)?,
            row.get::<_,String>(1)?,row.get::<_,bool>(2)?))))?;
        hash_array(rows.map(|row|{
            let (metadata,source,builtin)=sql(store,row)?;
            let mut record=decode(&metadata)?;
            record.remove("topic");record.insert("definition",wire_definition(&source,builtin)?);
            Ok(record)
        }))?
    };
    let mut identity=objects::page_identity("topics.list",params)?;
    identity.insert("snapshot",snapshot.into());
    let after=match optional_string(params,"cursor").filter(|cursor|!cursor.0.is_empty()) {
        Some(cursor)=>text(&crate::paging::catalog_boundary(store,&identity,cursor)?),None=>String::new(),
    };
    let limit=params.get("limit").and_then(IvyJson::as_f64).unwrap_or(50.0) as usize;
    let mut statement=sql(store,store.connection().prepare_cached(&format!(
        "SELECT node_id,nspos,topicpos,metadata FROM ({records}) WHERE {PREFIX} AND ({PAGE_KEY})>?7
         ORDER BY ({PAGE_KEY}),builtin,node_id,nspos,topicpos LIMIT ?8")))?;
    let ids=sql(store,statement.query_map(params![node_id,host,service,namespace,prefix,include_hive,after,(limit+1)as i64],from_row))?
        .map(|row|entry(store,row)).collect::<IvyResult<Vec<_>>>()?;
    crate::paging::catalog_page_with(store,ids,&identity,page_key,|id|{
        let topic=definition(store,&id)?;
        let provider=if let Some(node)=id.node {
            let node=registry.node(store,&node.into(),runtime,now)?;
            let eligible=Registry::eligible(&node);
            IvyJson::object([("node",node),("eligible",eligible.into())])
        }else{IvyJson::Null};
        Ok(IvyJson::object([("provider",provider),
            ("namespace",field(&id.metadata,"namespace")?.clone()),("definition",topic)]))
    },Some(limit),None)
}
