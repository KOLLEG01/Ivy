//! Schema-bound complete/delta observations and SQL keyset inventory reads.
use crate::{
    paging::{PAGE_BYTES, page, utc},
    registry::Registry,
    retention::Runtime,
    store::Store,
};
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString,
    json::{canonical, parse},
    limits::MANAGEMENT_FRAME_BYTES,
};
use rusqlite::{
    OptionalExtension, params, params_from_iter,
    types::{Value, ValueRef},
};
use std::collections::{HashMap, HashSet};

fn sql<T>(store: &Store, result: rusqlite::Result<T>) -> IvyResult<T> {
    result.map_err(|e| store.query_error(e))
}
fn text(value: &JsString) -> String {
    String::from_utf16_lossy(&value.0)
}
fn field<'a>(value: &'a IvyJson, key: &str) -> IvyResult<&'a IvyJson> {
    value.get(key).ok_or_else(IvyError::internal)
}
fn string<'a>(value: &'a IvyJson, key: &str) -> IvyResult<&'a JsString> {
    field(value, key)?
        .as_js_string()
        .ok_or_else(IvyError::internal)
}
fn array<'a>(value: &'a IvyJson, key: &str) -> IvyResult<&'a [IvyJson]> {
    field(value, key)?.as_array().ok_or_else(IvyError::internal)
}
fn is_text(value: Option<&IvyJson>, text: &str) -> bool {
    value.and_then(IvyJson::as_js_string) == Some(&text.into())
}
fn require(value: bool, code: &str, message: &str) -> IvyResult<()> {
    if value {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn decoded(value: &str) -> IvyResult<IvyJson> {
    parse(value, usize::MAX).map_err(|_| IvyError::internal())
}
fn encoded(value: &IvyJson) -> IvyResult<String> {
    canonical(value, MANAGEMENT_FRAME_BYTES)
}
fn binding(value: Option<&IvyJson>) -> IvyResult<Value> {
    Ok(match value {
        None | Some(IvyJson::Null) => Value::Null,
        Some(IvyJson::String(v)) => Value::Text(text(v)),
        Some(IvyJson::Number(v)) => Value::Real(*v),
        Some(IvyJson::Bool(v)) => Value::Integer(i64::from(*v)),
        _ => return Err(IvyError::internal()),
    })
}
fn row_value(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<IvyJson> {
    Ok(match row.get_ref(index)? {
        ValueRef::Null => IvyJson::Null,
        ValueRef::Integer(v) => IvyJson::Number(v as f64),
        ValueRef::Real(v) => IvyJson::Number(v),
        ValueRef::Text(v) => String::from_utf8_lossy(v).into_owned().into(),
        ValueRef::Blob(_) => {
            return Err(rusqlite::Error::InvalidColumnType(
                index,
                index.to_string(),
                rusqlite::types::Type::Blob,
            ));
        }
    })
}
fn rows(store: &Store, query: &str, bindings: &[Value]) -> IvyResult<Vec<IvyJson>> {
    let mut statement = sql(store, store.connection().prepare_cached(query))?;
    let columns: Vec<_> = statement
        .column_names()
        .iter()
        .map(|name| name.to_string())
        .collect();
    sql(
        store,
        statement.query_map(params_from_iter(bindings.iter()), |row| {
            Ok(IvyJson::object(
                columns
                    .iter()
                    .enumerate()
                    .map(|(index, name)| Ok((name.clone(), row_value(row, index)?)))
                    .collect::<rusqlite::Result<Vec<_>>>()?,
            ))
        }),
    )?
    .map(|row| sql(store, row))
    .collect()
}

pub fn sync(
    store: &mut Store,
    registry: &mut Registry,
    id: &JsString,
    parameters: &IvyJson,
    runtime: &dyn Runtime,
    now: f64,
) -> IvyResult<IvyJson> {
    store.transaction(|store|{
        let node=registry.node(store,id,runtime,now)?;
        require(node.get("synced").and_then(IvyJson::as_bool) == Some(true),"service_not_ready","Registry synchronization is required before inventory.")?;
        let node_id=string(&node,"serviceNodeId")?;let namespace=string(parameters,"namespace")?;let kind=string(parameters,"kind")?;let version=string(parameters,"schemaVersion")?;
        let ns=registry.namespace(store,node_id,namespace)?;
        let mut schema=None;
        for candidate in array(&ns,"inventoryKinds")?{
            if candidate.is_null(){return Err(IvyError::internal());}
            if candidate.get("kind").and_then(IvyJson::as_js_string)==Some(kind)&&candidate.get("version").and_then(IvyJson::as_js_string)==Some(version){schema=Some(field(candidate,"summarySchema")?);break;}
        }
        let schema=schema.ok_or_else(||IvyError::new("registry_invalid","Inventory kind/version is not in the current registry."))?;
        let entries=array(parameters,"entries")?;let mut ids=HashSet::new();
        for entry in entries{if !ids.insert(string(entry,"nativeId")?.clone()){return Err(IvyError::new("registry_invalid","Inventory contains duplicate native IDs."));}}
        for entry in entries{utc(field(entry,"observedAt")?,runtime)?;store.validators().validate(schema,field(entry,"summary")?,1024*1024)?;}
        let delta=is_text(parameters.get("mode"),"delta");
        let removed=if delta{array(parameters,"removedNativeIds")?}else{&[]};
        if delta{
            let mut removed_set=HashSet::new();
            let mut unique=true;for id in removed{if !removed_set.insert(id.as_js_string().ok_or_else(IvyError::internal)?.clone()){unique=false;}}
            require(unique&&!entries.iter().any(|entry|removed.contains(field(entry,"nativeId").unwrap())),"registry_invalid","Delta contains conflicting or duplicate resource IDs.")?;
        }
        let previous=sql(store,store.connection().query_row("SELECT revision,schema_version,observed_at FROM inventory_sets WHERE node_id=? AND namespace=? AND kind=?",params![text(node_id),text(namespace),text(kind)],|r|Ok((r.get::<_,f64>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?))).optional())?;
        let revision=field(parameters,"snapshotRevision")?.as_f64().ok_or_else(IvyError::internal)?;
        let conflict=|message:&str|IvyError::new("revision_conflict",message).with_details(IvyJson::object([("serviceNodeId",IvyJson::String(node_id.clone())),("namespace",IvyJson::String(namespace.clone())),("kind",IvyJson::String(kind.clone())),("currentRevision",IvyJson::Number(previous.as_ref().map_or(0.0,|v|v.0)))]));
        if let Some((_,prior_version,prior_at))=previous.as_ref().filter(|(r,_,_)|is_text(parameters.get("mode"),"snapshot")&&*r==revision){
            let current=rows(store,"SELECT native_id,summary,observed_at FROM inventory WHERE node_id=? AND namespace=? AND kind=? ORDER BY native_id",&[Value::Text(text(node_id)),Value::Text(text(namespace)),Value::Text(text(kind))])?.iter().map(|row|Ok(IvyJson::object([("nativeId",field(row,"native_id")?.clone()),("summary",decoded(&text(string(row,"summary")?))?),("observedAt",field(row,"observed_at")?.clone())]))).collect::<IvyResult<Vec<_>>>()?;
            let mut sorted=entries.to_vec();sorted.sort_by_key(|entry|text(string(entry,"nativeId").unwrap()).into_bytes());
            if JsString::from(prior_version.as_str())!=*version||encoded(&IvyJson::Array(current))?!=encoded(&IvyJson::Array(sorted))?{return Err(conflict("Snapshot revision already identifies different inventory."));}
            return Ok(IvyJson::object([("snapshotRevision",IvyJson::Number(revision)),("observedAt",prior_at.clone().into())]));
        }
        if previous.as_ref().is_some_and(|v|revision<=v.0){return Err(conflict("Inventory revision must advance."));}
        if delta&&(previous.as_ref().is_none_or(|(r,v,_)|Some(*r)!=parameters.get("expectedRevision").and_then(IvyJson::as_f64)||JsString::from(v.as_str())!=*version)){return Err(conflict("Inventory delta has a gap; send a fresh complete snapshot."));}
        let observed=if let Some(first)=entries.first(){
            let mut latest=string(first,"observedAt")?.clone();for entry in entries{let observed=string(entry,"observedAt")?;if observed>&latest{latest=observed.clone();}}latest
        }else{runtime.iso_date(now)?};
        sql(store,store.connection().execute("INSERT INTO inventory_sets VALUES (?,?,?,?,?,?) ON CONFLICT(node_id,namespace,kind) DO UPDATE SET revision=excluded.revision,schema_version=excluded.schema_version,observed_at=excluded.observed_at",params![text(node_id),text(namespace),text(kind),revision,text(version),text(&observed)]))?;
        if is_text(parameters.get("mode"),"snapshot"){
            sql(store,store.connection().execute("DELETE FROM inventory WHERE node_id=? AND namespace=? AND kind=?",params![text(node_id),text(namespace),text(kind)]))?;
        }else{for removed in removed{sql(store,store.connection().execute("DELETE FROM inventory WHERE node_id=? AND namespace=? AND kind=? AND native_id=?",params![text(node_id),text(namespace),text(kind),text(removed.as_js_string().ok_or_else(IvyError::internal)?)]))?;}}
        for entry in entries{sql(store,store.connection().execute("INSERT INTO inventory VALUES (?,?,?,?,?,?,?) ON CONFLICT(node_id,namespace,kind,native_id) DO UPDATE SET schema_version=excluded.schema_version,summary=excluded.summary,observed_at=excluded.observed_at",params![text(node_id),text(namespace),text(kind),text(string(entry,"nativeId")?),text(version),encoded(field(entry,"summary")?)?,text(string(entry,"observedAt")?)]))?;}
        let mut updated=node.clone();updated.insert("lastObservationAt",IvyJson::String(observed.clone()));registry.save(store,&updated,false)?;
        let scope=JsString(["inventory/".encode_utf16().collect::<Vec<_>>(),namespace.0.clone(),vec![47],kind.0.clone(),vec![47],node_id.0.clone()].concat());store.invalidate(scope);
        Ok(IvyJson::object([("snapshotRevision",IvyJson::Number(revision)),("observedAt",IvyJson::String(observed))]))
    })
}

fn item(
    row: &IvyJson,
    provider_stale: bool,
    revision: IvyJson,
    runtime: &dyn Runtime,
    now: f64,
) -> IvyResult<IvyJson> {
    let stale = provider_stale || now - runtime.parse_date(string(row, "observed_at")?) > 60_000.0;
    Ok(IvyJson::object([
        (
            "resourceRef",
            IvyJson::object([
                ("serviceNodeId", field(row, "node_id")?.clone()),
                ("namespace", field(row, "namespace")?.clone()),
                ("kind", field(row, "kind")?.clone()),
                ("nativeId", field(row, "native_id")?.clone()),
            ]),
        ),
        ("schemaVersion", field(row, "schema_version")?.clone()),
        ("summary", decoded(&text(string(row, "summary")?))?),
        ("observedAt", field(row, "observed_at")?.clone()),
        ("stale", stale.into()),
        ("snapshotRevision", revision),
    ]))
}

fn provider_stale(node: &IvyJson) -> bool {
    node.get("stale").and_then(IvyJson::as_bool) == Some(true) || !Registry::eligible(node)
}

pub fn get(
    store: &Store,
    registry: &Registry,
    resource: &IvyJson,
    runtime: &dyn Runtime,
    now: f64,
) -> IvyResult<IvyJson> {
    let found = rows(
        store,
        "SELECT * FROM inventory WHERE node_id=? AND namespace=? AND kind=? AND native_id=?",
        &[
            binding(resource.get("serviceNodeId"))?,
            binding(resource.get("namespace"))?,
            binding(resource.get("kind"))?,
            binding(resource.get("nativeId"))?,
        ],
    )?;
    let row = found
        .first()
        .ok_or_else(|| IvyError::new("not_found", "Native inventory resource not found."))?;
    let node = registry.inventory_provider(store, string(row, "node_id")?, runtime, now)?;
    let revision = sql(
        store,
        store.connection().query_row(
            "SELECT revision FROM inventory_sets WHERE node_id=? AND namespace=? AND kind=?",
            params![
                text(string(row, "node_id")?),
                text(string(row, "namespace")?),
                text(string(row, "kind")?)
            ],
            |r| r.get::<_, f64>(0),
        ),
    )?;
    item(row, provider_stale(&node), IvyJson::Number(revision), runtime, now)
}

pub fn list(
    store: &Store,
    registry: &Registry,
    parameters: &IvyJson,
    runtime: &dyn Runtime,
    now: f64,
) -> IvyResult<IvyJson> {
    let resource = parameters
        .get("resourceRef")
        .filter(|v| v.as_object().is_some());
    if let Some(resource) = resource {
        require(
            [
                ("serviceNodeId", "serviceNodeId"),
                ("namespace", "namespace"),
                ("kind", "kind"),
            ]
            .iter()
            .all(|(left, right)| {
                parameters
                    .get(left)
                    .and_then(IvyJson::as_js_string)
                    .is_none_or(|v| v.0.is_empty())
                    || parameters.get(left) == resource.get(right)
            }),
            "target_conflict",
            "Inventory selectors disagree.",
        )?;
    }
    let select = |key: &str| {
        parameters
            .get(key)
            .filter(|v| !v.is_null())
            .or_else(|| resource.and_then(|v| v.get(key)).filter(|v| !v.is_null()))
    };
    let node_id = select("serviceNodeId");
    let namespace = select("namespace");
    let kind = select("kind");
    let native_id = resource.and_then(|v| v.get("nativeId"));
    let mut providers = HashMap::new();
    if let Some(id) = node_id.filter(|v| v.as_js_string().is_some_and(|v| !v.0.is_empty())) {
        let node = registry.inventory_provider(
            store,
            id.as_js_string().ok_or_else(IvyError::internal)?,
            runtime,
            now,
        )?;
        require(
            ["hostId", "serviceName"].iter().all(|key| {
                parameters
                    .get(key)
                    .and_then(IvyJson::as_js_string)
                    .is_none_or(|v| v.0.is_empty())
                    || parameters.get(key) == node.get(key)
            }),
            "target_conflict",
            "Inventory owner selectors disagree.",
        )?;
        // The old provider map is keyed by the original JSON identity, which
        // may differ from the SQL row's lossy UTF-8 key.
        providers.insert(string(&node, "serviceNodeId")?.clone(), provider_stale(&node));
    }
    let base = [
        node_id,
        parameters.get("hostId"),
        parameters.get("serviceName"),
        namespace,
        kind,
    ];
    let mut selectors = vec![];
    for selected in base.into_iter().chain([native_id]) {
        let value = binding(selected)?;
        selectors.push(value.clone());
        selectors.push(value);
    }
    const JOINS: &str = "FROM inventory i JOIN service_nodes n ON i.node_id=n.id JOIN inventory_sets s ON s.node_id=i.node_id AND s.namespace=i.namespace AND s.kind=i.kind JOIN inventory_schemas k ON k.namespace=i.namespace AND k.kind=i.kind AND k.version=i.schema_version";
    const SELECTED: &str = "(? IS NULL OR n.id=?) AND (? IS NULL OR n.host_id=?) AND (? IS NULL OR n.service_name=?) AND (? IS NULL OR i.namespace=?) AND (? IS NULL OR i.kind=?) AND (? IS NULL OR i.native_id=?)";
    let recency_sort = is_text(parameters.get("sort"), "recency-desc");
    for (active, condition, message) in [
        (
            parameters.get("archived").is_some(),
            "json_type(k.definition,'$.archivedPointer') IS NULL",
            "Selected inventory kind does not declare archive semantics.",
        ),
        (
            parameters
                .get("searchTerm")
                .and_then(IvyJson::as_js_string)
                .is_some_and(|v| !v.0.is_empty()),
            "COALESCE(json_array_length(k.definition,'$.searchPointers'),0)=0",
            "Selected inventory kind does not declare searchable fields.",
        ),
        (
            recency_sort,
            "json_type(k.definition,'$.recencyPointer') IS NULL",
            "Selected inventory kind does not declare recency semantics.",
        ),
    ] {
        if active {
            require(
                rows(
                    store,
                    &format!("SELECT 1 {JOINS} WHERE {SELECTED} AND {condition} LIMIT 1"),
                    &selectors,
                )?
                .is_empty(),
                "invalid_arguments",
                message,
            )?;
        }
    }
    let sets = rows(
        store,
        "SELECT s.node_id,s.namespace,s.kind,s.revision,s.schema_version FROM inventory_sets s JOIN service_nodes n ON s.node_id=n.id WHERE (? IS NULL OR n.id=?) AND (? IS NULL OR n.host_id=?) AND (? IS NULL OR n.service_name=?) AND (? IS NULL OR s.namespace=?) AND (? IS NULL OR s.kind=?) ORDER BY s.node_id,s.namespace,s.kind",
        &selectors[..10],
    )?;
    let mut identity_parameters = parameters.clone();
    identity_parameters.remove("cursor");
    identity_parameters.remove("limit");
    let identity = IvyJson::object([
        ("method", "inventory.list".into()),
        ("selectors", identity_parameters),
        ("sets", encoded(&IvyJson::Array(sets))?.into()),
    ]);
    let boundary = if let Some(cursor) = parameters
        .get("cursor")
        .filter(|v| v.as_js_string().is_some_and(|v| !v.0.is_empty()))
    {
        store.read_cursor(
            &identity,
            cursor.as_js_string().ok_or_else(IvyError::internal)?,
        )?
    } else {
        Some(IvyJson::Null)
    };
    require(
        boundary.as_ref().is_some_and(|boundary| {
            boundary.is_null()
                || boundary.as_array().is_some_and(|parts| {
                    parts.len() == (if recency_sort { 5 } else { 4 })
                        && parts.iter().enumerate().all(|(index, value)| {
                            if recency_sort && index == 0 {
                                value.as_f64().is_some()
                            } else {
                                value.as_js_string().is_some()
                            }
                        })
                })
        }),
        "invalid_cursor",
        "Invalid inventory boundary.",
    )?;
    let parts = boundary.as_ref().and_then(IvyJson::as_array);
    let part = |index: usize| parts.and_then(|v| v.get(index));
    const RECENCY: &str = "ivy_recency(i.summary,json_extract(k.definition,'$.recencyPointer'))";
    const TIE_AFTER: &str = "(i.kind COLLATE BINARY>? OR (i.kind=? AND (i.native_id COLLATE BINARY>? OR (i.native_id=? AND (i.namespace COLLATE BINARY>? OR (i.namespace=? AND i.node_id COLLATE BINARY>?))))))";
    let after = if recency_sort {
        format!("(? IS NULL OR {RECENCY}<? OR ({RECENCY}=? AND {TIE_AFTER}))")
    } else {
        format!("(? IS NULL OR {TIE_AFTER})")
    };
    let mut bindings = selectors;
    bindings.push(binding(parameters.get("archived"))?);
    bindings.push(Value::Integer(i64::from(
        parameters.get("archived").and_then(IvyJson::as_bool) == Some(true),
    )));
    let search = parameters
        .get("searchTerm")
        .filter(|v| !v.is_null())
        .map(|v| store.locale_lowercase(v.as_js_string().ok_or_else(IvyError::internal)?))
        .transpose()?;
    bindings.push(
        if parameters
            .get("searchTerm")
            .and_then(IvyJson::as_js_string)
            .is_some_and(|v| !v.0.is_empty())
        {
            Value::Text(text(search.as_ref().ok_or_else(IvyError::internal)?))
        } else {
            Value::Null
        },
    );
    bindings.push(Value::Text(search.as_ref().map(text).unwrap_or_default()));
    bindings.push(binding(part(0))?);
    if recency_sort {
        for _ in 0..2 {
            bindings.push(binding(part(0)).map(|v| {
                if v == Value::Null {
                    Value::Integer(0)
                } else {
                    v
                }
            })?);
        }
    }
    let first = usize::from(recency_sort);
    for index in first..first + 4 {
        for _ in 0..if index == first + 3 { 1 } else { 2 } {
            bindings.push(
                part(index)
                    .map(|v| binding(Some(v)))
                    .transpose()?
                    .unwrap_or_else(|| Value::Text(String::new())),
            );
        }
    }
    let limit = parameters
        .get("limit")
        .and_then(IvyJson::as_f64)
        .unwrap_or(50.0) as usize;
    bindings.push(Value::Integer((limit + 1) as i64));
    let query = format!(
        "SELECT i.*,s.revision AS snapshot_revision,{} AS sort_value {JOINS} WHERE {SELECTED} AND (? IS NULL OR (ivy_type(i.summary,json_extract(k.definition,'$.archivedPointer'))=1 AND ivy_value(i.summary,json_extract(k.definition,'$.archivedPointer'))=?)) AND (? IS NULL OR EXISTS (SELECT 1 FROM json_each(k.definition,'$.searchPointers') p WHERE ivy_contains(i.summary,p.value,?)=1)) AND {after} AND ivy_budget() ORDER BY {} i.kind COLLATE BINARY,i.native_id COLLATE BINARY,i.namespace COLLATE BINARY,i.node_id COLLATE BINARY LIMIT ?",
        if recency_sort { RECENCY } else { "0" },
        if recency_sort {
            format!("{RECENCY} DESC,")
        } else {
            String::new()
        }
    );
    let found = rows(store, &query, &bindings)?;
    let mut items = vec![];
    let mut bytes = 128;
    let mut trimmed = false;
    for row in found.iter().take(limit) {
        let id = string(row, "node_id")?;
        let stale = if let Some(stale) = providers.get(id) {
            *stale
        } else {
            let node = registry.inventory_provider(store, id, runtime, now)?;
            require(string(&node, "serviceNodeId")? == id,
                "storage_invalid",
                "Inventory item references an unknown Service Node.",
            )?;
            let stale = provider_stale(&node);
            providers.insert(id.clone(), stale);
            stale
        };
        let next = item(
            row,
            stale,
            field(row, "snapshot_revision")?.clone(),
            runtime,
            now,
        )?;
        let size = encoded(&next)?.len();
        if bytes + size > PAGE_BYTES {
            require(
                !items.is_empty(),
                "result_too_large",
                "An inventory entry exceeds the response limit.",
            )?;
            trimmed = true;
            break;
        }
        bytes += size;
        items.push(next);
    }
    let more = trimmed || found.len() > items.len();
    let cursor = if more && !items.is_empty() {
        let last = &found[items.len() - 1];
        let mut parts = vec![];
        if recency_sort {
            parts.push(field(last, "sort_value")?.clone());
        }
        for key in ["kind", "native_id", "namespace", "node_id"] {
            parts.push(field(last, key)?.clone());
        }
        Some(store.cursor(&identity, &IvyJson::Array(parts))?)
    } else {
        None
    };
    Ok(page(items, cursor))
}
