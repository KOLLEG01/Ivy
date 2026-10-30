use super::*;
use ivy_contracts::{
    core_schema::validate_shared,
    hash::hash_json,
    json::{canonical, parse},
    limits::MANAGEMENT_FRAME_BYTES,
};
use rusqlite::{OptionalExtension, params};
use std::collections::HashSet;
#[path = "deployment_catalog.rs"]
mod deployments;

pub(super) const METHODS: &[&str] = &[
    "system.diagnostics",
    "contracts.list",
    "contracts.summaries",
    "hosts.observations",
    "hosts.list",
    "serviceNodes.list",
    "serviceNodes.get",
    "serviceNodes.contracts",
    "namespaces.list",
    "namespaces.get",
    "tools.list",
    "topics.list",
    "deployments.report",
    "deployments.list",
    "deployments.get",
];
fn sql<T>(store: &Store, result: rusqlite::Result<T>) -> IvyResult<T> {
    result.map_err(|e| store.query_error(e))
}
fn decode(value: &str) -> IvyResult<IvyJson> {
    parse(value, usize::MAX).map_err(|_| IvyError::internal())
}
fn text(value: &JsString) -> String {
    String::from_utf16_lossy(&value.0)
}
fn optional_string<'a>(value: &'a IvyJson, key: &str) -> Option<&'a JsString> {
    value.get(key).and_then(IvyJson::as_js_string)
}
fn array<'a>(value: &'a IvyJson, key: &str) -> IvyResult<&'a [IvyJson]> {
    match value.get(key).filter(|v| !v.is_null()) {
        None => Ok(&[]),
        Some(v) => v.as_array().ok_or_else(IvyError::internal),
    }
}
fn provider(node: &IvyJson) -> IvyJson {
    IvyJson::object([
        ("node", node.clone()),
        ("eligible", Registry::eligible(node).into()),
    ])
}
fn page(
    call: &Call<'_>,
    method: &str,
    params: &IvyJson,
    records: Vec<IvyJson>,
    key: impl Fn(&IvyJson) -> JsString,
    snapshot: Option<IvyJson>,
) -> IvyResult<IvyJson> {
    try_page(
        call,
        method,
        params,
        records,
        |record| Ok(key(record)),
        snapshot,
    )
}
pub(super) fn try_page(
    call: &Call<'_>,
    method: &str,
    params: &IvyJson,
    records: Vec<IvyJson>,
    key: impl Fn(&IvyJson) -> IvyResult<JsString>,
    snapshot: Option<IvyJson>,
) -> IvyResult<IvyJson> {
    let mut identity = objects::page_identity(method, params)?;
    if let Some(snapshot) = snapshot {
        identity.insert("snapshot", snapshot);
    }
    crate::paging::try_catalog_page(
        call.store,
        records,
        &identity,
        key,
        params
            .get("limit")
            .and_then(IvyJson::as_f64)
            .map(|v| v as usize),
        optional_string(params, "cursor"),
    )
}
fn key(value: &IvyJson, name: &str) -> JsString {
    optional_string(value, name).cloned().unwrap_or_default()
}
fn selected(params: &IvyJson, name: &str, value: Option<&IvyJson>) -> bool {
    !params
        .get(name)
        .and_then(IvyJson::as_js_string)
        .is_some_and(|v| !v.0.is_empty())
        || params.get(name) == value
}
fn prefixed(value: &IvyJson, name: &str, prefix: Option<&IvyJson>) -> IvyResult<bool> {
    if !prefix
        .and_then(IvyJson::as_js_string)
        .is_some_and(|v| !v.0.is_empty())
    {
        return Ok(true);
    }
    let prefix = prefix
        .and_then(IvyJson::as_js_string)
        .ok_or_else(IvyError::internal)?;
    Ok(string(value, name)?.0.starts_with(&prefix.0))
}
fn tool_page_records(
    id: &JsString,
    guide: &IvyJson,
    tools: Vec<IvyJson>,
    params: &IvyJson,
) -> IvyResult<(String, Vec<IvyJson>)> {
    let mut identity = IvyJson::object([
        ("nodeId", IvyJson::String(id.clone())),
        ("tools", IvyJson::Array(tools)),
        ("guide", guide.clone()),
    ]);
    // Check every prefix before hashing, preserving the original error order.
    // The identity owns the only materialized tools; move them into the page input.
    let included = array(&identity, "tools")?.iter().map(|tool| prefixed(
        tool.get("definition").unwrap_or(&IvyJson::Null), "name", params.get("namePrefix"),
    )).collect::<IvyResult<Vec<_>>>()?;
    let hash = hash_json(&identity)?;
    let tools = identity.remove("tools").and_then(IvyJson::into_array).ok_or_else(IvyError::internal)?;
    Ok((hash, tools.into_iter().zip(included).filter_map(|(tool, include)| include.then_some(tool)).collect()))
}
pub(super) fn handle(
    call: &mut Call<'_>,
    context: &IvyJson,
    params: &IvyJson,
    method: &str,
) -> IvyResult<IvyJson> {
    match method {
        "system.diagnostics" => {
            let records = call
                .registry
                .diagnostics(call.store, optional_string(params, "status"))?;
            try_page(
                call,
                method,
                params,
                records,
                |value| {
                    Ok(hash_json(&IvyJson::object([
                        (
                            "resource",
                            value.get("resource").cloned().unwrap_or(IvyJson::Null),
                        ),
                        ("code", value.get("code").cloned().unwrap_or(IvyJson::Null)),
                        (
                            "source",
                            value.get("source").cloned().unwrap_or(IvyJson::Null),
                        ),
                    ]))?
                    .into())
                },
                None,
            )
        }
        "contracts.list" | "contracts.summaries" => {
            crate::contract_catalog::list(call.store, method, params)
        }
        "hosts.observations" => {
            let records = call.registry.host_observations(
                call.store,
                optional_string(params, "hostId"),
                call.runtime,
                (call.clock)(),
            )?;
            require(
                records.len() <= 1000,
                "limit_exceeded",
                "Host observation catalog exceeds its operating bound.",
            )?;
            page(
                call,
                method,
                params,
                records,
                |v| {
                    key(
                        v.get("snapshot")
                            .and_then(|v| v.get("status"))
                            .unwrap_or(&IvyJson::Null),
                        "hostId",
                    )
                },
                None,
            )
        }
        "hosts.list" => {
            let nodes = call
                .registry
                .nodes(call.store, call.runtime, (call.clock)())?;
            let mut seen = HashSet::new();
            let mut records = vec![];
            for node in &nodes {
                let host = field(node, "hostId")?;
                if seen.insert(string(node, "hostId")?.clone()) {
                    let owned = nodes
                        .iter()
                        .filter(|v| v.get("hostId") == Some(host))
                        .collect::<Vec<_>>();
                    records.push(IvyJson::object([
                        ("hostId", host.clone()),
                        (
                            "serviceNodeIds",
                            IvyJson::Array(
                                owned
                                    .iter()
                                    .map(|v| {
                                        v.get("serviceNodeId").cloned().unwrap_or(IvyJson::Null)
                                    })
                                    .collect(),
                            ),
                        ),
                        (
                            "connected",
                            owned
                                .iter()
                                .any(|v| {
                                    v.get("connected").and_then(IvyJson::as_bool) == Some(true)
                                })
                                .into(),
                        ),
                    ]));
                }
            }
            page(call, method, params, records, |v| key(v, "hostId"), None)
        }
        "serviceNodes.list" => {
            let records = call
                .registry
                .nodes(call.store, call.runtime, (call.clock)())?
                .into_iter()
                .filter(|v| {
                    selected(params, "hostId", v.get("hostId"))
                        && selected(params, "serviceName", v.get("serviceName"))
                })
                .collect();
            page(
                call,
                method,
                params,
                records,
                |v| key(v, "serviceNodeId"),
                None,
            )
        }
        "serviceNodes.get" => call.registry.node(
            call.store,
            string(params, "serviceNodeId")?,
            call.runtime,
            (call.clock)(),
        ),
        "serviceNodes.contracts" => {
            let id = string(params, "serviceNodeId")?;
            let node = call
                .registry
                .node(call.store, id, call.runtime, (call.clock)())?;
            let catalog = call.registry.required_contracts(call.store, id)?;
            let mut records = vec![];
            for (index, value) in catalog
                .as_ref()
                .filter(|value| !value.is_null())
                .map(|value| value.as_array().ok_or_else(IvyError::internal))
                .transpose()?
                .unwrap_or(&[])
                .iter()
                .enumerate()
            {
                if selected(params, "key", value.get("key")) {
                    records.push(IvyJson::object([
                        ("value", value.clone()),
                        ("index", IvyJson::Number(index as f64)),
                    ]));
                }
            }
            let mut identity = IvyJson::object([("records", IvyJson::Array(records.clone()))]);
            if let Some(captured) = node.get("lastSuccessfulSyncAt") {
                identity.insert("capturedAt", captured.clone());
            }
            let paged = page(
                call,
                method,
                params,
                records,
                |v| {
                    format!(
                        "{:08}",
                        v.get("index").and_then(IvyJson::as_f64).unwrap() as usize
                    )
                    .into()
                },
                Some(hash_json(&identity)?.into()),
            )?;
            let mut result = IvyJson::object([
                ("provider", provider(&node)),
                ("hasCatalog", catalog.is_some().into()),
                (
                    "items",
                    IvyJson::Array(
                        field(&paged, "items")?
                            .as_array()
                            .ok_or_else(IvyError::internal)?
                            .iter()
                            .map(|v| v.get("value").cloned().unwrap_or(IvyJson::Null))
                            .collect(),
                    ),
                ),
                ("nextCursor", field(&paged, "nextCursor")?.clone()),
            ]);
            if let Some(captured) = node.get("lastSuccessfulSyncAt") {
                result.insert("capturedAt", captured.clone());
            }
            Ok(result)
        }
        "namespaces.list" => {
            let mut records: Vec<IvyJson> = vec![];
            for node in call
                .registry
                .nodes(call.store, call.runtime, (call.clock)())?
            {
                let names = call
                    .registry
                    .namespace_names(call.store, string(&node, "serviceNodeId")?)?;
                for name in names {
                    if let Some(record) = records
                        .iter_mut()
                        .find(|v| v.get("namespace") == Some(&name))
                    {
                        let mut providers = record.remove("providers")
                            .and_then(IvyJson::into_array).ok_or_else(IvyError::internal)?;
                        providers.push(provider(&node));
                        record.insert("providers", IvyJson::Array(providers));
                    } else {
                        records.push(IvyJson::object([
                            ("namespace", name),
                            ("providers", IvyJson::Array(vec![provider(&node)])),
                        ]));
                    }
                }
            }
            page(call, method, params, records, |v| key(v, "namespace"), None)
        }
        "namespaces.get" => call.registry.snapshot(
            call.store,
            string(params, "namespace")?,
            params,
            call.runtime,
            (call.clock)(),
        ),
        "tools.list" => {
            let snapshot = call.registry.snapshot(
                call.store,
                string(params, "namespace")?,
                params,
                call.runtime,
                (call.clock)(),
            )?;
            let node = field(field(&snapshot, "provider")?, "node")?;
            let id = string(node, "serviceNodeId")?;
            let tools = call.registry.bindings(call.store, id, string(params, "namespace")?)?;
            let (identity, filtered) = tool_page_records(id, field(&snapshot, "guideMarkdown")?, tools, params)?;
            let mut result = page(
                call,
                method,
                params,
                filtered,
                |v| key(v, "qualifiedName"),
                Some(identity.into()),
            )?;
            result.insert("provider", field(&snapshot, "provider")?.clone());
            result.insert("guideMarkdown", field(&snapshot, "guideMarkdown")?.clone());
            Ok(result)
        }
        "topics.list" => topics(call, params),
        "deployments.report" => report(call, context, params),
        "deployments.list" => deployments::list(call, params),
        "deployments.get" => deployments::get(call, params),
        _ => Err(IvyError::internal()),
    }
}
fn topics(call: &Call<'_>, params: &IvyJson) -> IvyResult<IvyJson> {
    let resource = params.get("resourceRef");
    let id = params
        .get("serviceNodeId")
        .filter(|v| !v.is_null())
        .or_else(|| resource.and_then(|v| v.get("serviceNodeId")));
    let namespace = params
        .get("namespace")
        .filter(|v| !v.is_null())
        .or_else(|| resource.and_then(|v| v.get("namespace")));
    require(
        !params
            .get("serviceNodeId")
            .and_then(IvyJson::as_js_string)
            .is_some_and(|v| !v.0.is_empty())
            || !resource.is_some_and(|v| v.as_object().is_some())
            || params.get("serviceNodeId") == resource.and_then(|v| v.get("serviceNodeId")),
        "target_conflict",
        "Resource and node selectors disagree.",
    )?;
    require(
        !params
            .get("namespace")
            .and_then(IvyJson::as_js_string)
            .is_some_and(|v| !v.0.is_empty())
            || !resource.is_some_and(|v| v.as_object().is_some())
            || params.get("namespace") == resource.and_then(|v| v.get("namespace")),
        "target_conflict",
        "Resource and namespace selectors disagree.",
    )?;
    let nodes = if id
        .and_then(IvyJson::as_js_string)
        .is_some_and(|v| !v.0.is_empty())
    {
        vec![
            call.registry.node(
                call.store,
                id.and_then(IvyJson::as_js_string)
                    .ok_or_else(IvyError::internal)?,
                call.runtime,
                (call.clock)(),
            )?,
        ]
    } else {
        call.registry
            .nodes(call.store, call.runtime, (call.clock)())?
    };
    if id
        .and_then(IvyJson::as_js_string)
        .is_some_and(|v| !v.0.is_empty())
    {
        let node = &nodes[0];
        require(
            selected(params, "hostId", node.get("hostId"))
                && selected(params, "serviceName", node.get("serviceName")),
            "target_conflict",
            "The selected resource has a different owner.",
        )?;
        if namespace
            .and_then(IvyJson::as_js_string)
            .is_some_and(|v| !v.0.is_empty())
        {
            call.registry.namespace(
                call.store,
                string(node, "serviceNodeId")?,
                namespace
                    .and_then(IvyJson::as_js_string)
                    .ok_or_else(IvyError::internal)?,
            )?;
        }
    }
    let mut records = vec![];
    for node in nodes.into_iter().filter(|n| {
        selected(params, "hostId", n.get("hostId"))
            && selected(params, "serviceName", n.get("serviceName"))
    }) {
        let catalog = call
            .registry
            .get_registry(call.store, string(&node, "serviceNodeId")?)?;
        for ns in array(catalog.as_ref().unwrap_or(&IvyJson::Null), "namespaces")? {
            if namespace
                .and_then(IvyJson::as_js_string)
                .is_some_and(|v| !v.0.is_empty())
                && ns.get("namespace") != namespace
            {
                continue;
            }
            for topic in array(ns, "topics")? {
                if prefixed(topic, "topic", params.get("topicPrefix"))? {
                    records.push(IvyJson::object([
                        ("provider", provider(&node)),
                        ("namespace", field(ns, "namespace")?.clone()),
                        ("definition", topic.clone()),
                    ]));
                }
            }
        }
    }
    if !id
        .and_then(IvyJson::as_js_string)
        .is_some_and(|v| !v.0.is_empty())
        && !params
            .get("hostId")
            .and_then(IvyJson::as_js_string)
            .is_some_and(|v| !v.0.is_empty())
        && (!namespace
            .and_then(IvyJson::as_js_string)
            .is_some_and(|v| !v.0.is_empty())
            || is_text(namespace, "hive"))
        && (!params
            .get("serviceName")
            .and_then(IvyJson::as_js_string)
            .is_some_and(|v| !v.0.is_empty())
            || is_text(params.get("serviceName"), "hive"))
    {
        let mut statement = sql(
            call.store,
            call.store
                .connection()
                .prepare_cached("SELECT definition FROM topics WHERE owner='hive'"),
        )?;
        for row in sql(
            call.store,
            statement.query_map([], |r| r.get::<_, String>(0)),
        )? {
            let mut topic = decode(&sql(call.store, row)?)?;
            if !prefixed(&topic, "topic", params.get("topicPrefix"))? {
                continue;
            }
            topic.insert("title", "Object changes".into());
            topic.insert("description","A Hive object is written, moved, archived or deleted according to the selected topic version.".into());
            topic.insert("eventKinds", IvyJson::Array(vec![]));
            records.push(IvyJson::object([
                ("provider", IvyJson::Null),
                ("namespace", "hive".into()),
                ("definition", topic),
            ]));
        }
    }
    let identity = IvyJson::Array(
        records
            .iter()
            .map(|v| {
                IvyJson::object([
                    (
                        "serviceNodeId",
                        v.get("provider")
                            .and_then(|v| v.get("node"))
                            .and_then(|v| v.get("serviceNodeId"))
                            .cloned()
                            .unwrap_or_else(|| "hive".into()),
                    ),
                    (
                        "namespace",
                        v.get("namespace").cloned().unwrap_or(IvyJson::Null),
                    ),
                    (
                        "definition",
                        v.get("definition").cloned().unwrap_or(IvyJson::Null),
                    ),
                ])
            })
            .collect(),
    );
    page(
        call,
        "topics.list",
        params,
        records,
        |v| {
            let mut out = v
                .get("provider")
                .and_then(|v| v.get("node"))
                .map(|v| key(v, "serviceNodeId"))
                .unwrap_or_else(|| "hive".into());
            for part in [
                key(v, "namespace"),
                key(v.get("definition").unwrap_or(&IvyJson::Null), "topic"),
                key(v.get("definition").unwrap_or(&IvyJson::Null), "version"),
            ] {
                out.0.push(31);
                out.0.extend(part.0);
            }
            out
        },
        Some(hash_json(&identity)?.into()),
    )
}
fn report(call: &mut Call<'_>, context: &IvyJson, params: &IvyJson) -> IvyResult<IvyJson> {
    service(context)?;
    let node = call.registry.node(
        call.store,
        string(context, "serviceNodeId")?,
        call.runtime,
        (call.clock)(),
    )?;
    let now = call.runtime.iso_date((call.clock)())?;
    let runtime = call.runtime;
    call.store.transaction(|store|{
        for record in field(params,"records")?.as_array().ok_or_else(IvyError::internal)?{
            validate_shared("DeploymentRecord",record)?;
            crate::paging::utc(&IvyJson::String(string(record, "createdAt")?.clone()),runtime)?;
            crate::paging::utc(&IvyJson::String(string(record, "updatedAt")?.clone()),runtime)?;
            require(record.get("hostId")==node.get("hostId"),"target_conflict","Deployment snapshot belongs to another host.")?;
            let host=text(string(&node,"hostId")?);
            let id=text(&string(record, "deploymentId")?.clone());
            let prior=sql(store,store.connection().query_row("SELECT record_json FROM deployment_reports WHERE host_id=? AND deployment_id=?",params![host,id],|r|r.get::<_,String>(0)).optional())?;
            if let Some(prior)=prior{
                let previous=decode(&prior)?;
                require(["deploymentId","hostId","instanceId","componentId","requestHash","previousBuild","createdAt"].iter().all(|key|previous.get(key)==record.get(key))&&(previous.get("targetBuild")==Some(&IvyJson::Null)||previous.get("targetBuild")==record.get("targetBuild")),"target_conflict","Deployment observation cannot reinterpret its original local identity.")?;
                let phases=["preparing","prepared","checking","draining","activating","verifying","rolling_back"];
                let before=phases.iter().position(|v|is_text(previous.get("phase"),v));let after=phases.iter().position(|v|is_text(record.get("phase"),v));
                let valid=string(&previous, "updatedAt")?.clone()<=string(record, "updatedAt")?.clone()&&match before{None=>canonical(&previous,MANAGEMENT_FRAME_BYTES)?==canonical(record,MANAGEMENT_FRAME_BYTES)?,Some(before)=>after.is_none_or(|after|after>=before)};
                require(valid,"revision_conflict","Deployment observation cannot regress a phase or replace a terminal result.")?;
            }
            sql(store,store.connection().execute("INSERT INTO deployment_reports VALUES (?,?,?,?,?) ON CONFLICT(host_id,deployment_id) DO UPDATE SET node_id=excluded.node_id,reported_at=excluded.reported_at,record_json=excluded.record_json",params![host,id,text(string(&node,"serviceNodeId")?),text(&now),canonical(record,MANAGEMENT_FRAME_BYTES)?]))?;
        }Ok(())
    })?;
    Ok(IvyJson::object([("reportedAt", IvyJson::String(now))]))
}
#[cfg(test)]
#[path = "../../../../tests/rust/hive_catalog_reads.rs"]
mod read_tests;
