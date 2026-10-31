use super::*;
use ivy_contracts::{
    core_schema::validate_shared,
    hash::{hash_json, hash_object_with_array},
    json::{canonical, parse},
    limits::MANAGEMENT_FRAME_BYTES,
};
use rusqlite::{OptionalExtension, params};
#[path = "deployment_catalog.rs"]
mod deployments;
#[path = "service_contract_catalog.rs"]
mod service_contracts;
#[path = "topic_catalog.rs"]
mod topic_catalog;

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
fn tool_page_records<'a>(
    id: &JsString,
    guide: &IvyJson,
    tools: &'a [crate::registry_catalog::ToolBinding],
    params: &IvyJson,
) -> IvyResult<(String, Vec<&'a crate::registry_catalog::ToolBinding>)> {
    // Check every prefix before hashing, preserving the original error order.
    let included = tools.iter().map(|tool| prefixed(
        &tool.metadata, "name", params.get("namePrefix"),
    )).collect::<IvyResult<Vec<_>>>()?;
    let node_key = JsString::from("nodeId");
    let guide_key = JsString::from("guide");
    let tools_key = JsString::from("tools");
    let node = IvyJson::String(id.clone());
    let hash = hash_object_with_array([(&node_key, &node), (&guide_key, guide)], &tools_key,
        tools.iter().map(|tool| Ok(tool.to_wire())))?;
    Ok((hash, tools.iter().zip(included).filter_map(|(tool, include)| include.then_some(tool)).collect()))
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
        "hosts.list" => call.registry.host_page(call.store, params),
        "serviceNodes.list" => call.registry.node_page(
            call.store, params, call.runtime, (call.clock)(),
        ),
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
            service_contracts::list(call.store, params, &node)
        }
        "namespaces.list" => call.registry.namespace_page(
            call.store, params, call.runtime, (call.clock)(),
        ),
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
            let catalog = call.registry.live_catalog(call.store, id)?;
            let namespace = string(params, "namespace")?;
            let tools = catalog.as_ref().and_then(|catalog| catalog.namespaces.iter()
                .find(|ns| &ns.name == namespace).map(|ns| catalog.bindings(ns))).unwrap_or(&[]);
            let (identity, filtered) = tool_page_records(id, field(&snapshot, "guideMarkdown")?, tools, params)?;
            let mut page_identity = objects::page_identity(method, params)?;
            page_identity.insert("snapshot", identity.into());
            let mut result = crate::paging::catalog_page_with(
                call.store,
                filtered,
                &page_identity,
                |binding| Ok(binding.qualified_name.clone()),
                |binding| Ok(binding.to_wire()),
                params.get("limit").and_then(IvyJson::as_f64).map(|v| v as usize),
                optional_string(params, "cursor"),
            )?;
            result.insert("provider", field(&snapshot, "provider")?.clone());
            result.insert("guideMarkdown", field(&snapshot, "guideMarkdown")?.clone());
            Ok(result)
        }
        "topics.list" => topic_catalog::list(call.store, call.registry, params, call.runtime, (call.clock)()),
        "deployments.report" => report(call, context, params),
        "deployments.list" => deployments::list(call, params),
        "deployments.get" => deployments::get(call, params),
        _ => Err(IvyError::internal()),
    }
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
