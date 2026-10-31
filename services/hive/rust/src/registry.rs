//! Durable registry state and the storage lifetime's contact/binding mirrors.
//! Live generation authority is supplied by dispatch, never the legacy column.
use crate::{
    registry_catalog::{Catalog, CatalogCache, ToolBinding},
    retention::Runtime,
    store::{AuthenticatedContext, Store},
};
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString,
    core_schema::validate_shared,
    hash::hash_json,
    json::{canonical, parse},
    limits::MANAGEMENT_FRAME_BYTES,
};
use rusqlite::{OptionalExtension, params};
use std::{collections::{HashMap, HashSet}, sync::Arc};
#[path = "registry_diagnostics.rs"]
mod diagnostics;
#[path = "registry_sync.rs"]
mod syncing;
#[path = "registry_namespaces.rs"]
mod namespaces;
#[path = "registry_pages.rs"]
mod pages;
#[cfg(test)]
#[path = "../../../../tests/rust/hive_registry_catalog_memory.rs"]
mod catalog_memory_tests;

pub const RESOLVED_DIAGNOSTIC_LIMIT: usize = 10_000;
pub const ACTIVE_DIAGNOSTIC_LIMIT: usize = 4_096;

pub struct ConnectionContext<'a> {
    pub principal: &'a AuthenticatedContext,
    pub session_digest: Option<&'a JsString>,
    pub generation: Option<f64>,
}
#[derive(Default)]
struct Contact {
    at: JsString,
    contracts_acknowledged: bool,
}
#[derive(Default)]
pub struct Registry {
    changed_nodes: Vec<JsString>,
    changed_node_set: HashSet<JsString>,
    changed_generations: Vec<(JsString, f64)>,
    generation_indices: HashMap<JsString, usize>,
    catalogs: CatalogCache,
    contacts: HashMap<JsString, Contact>,
}
fn sql<T>(store: &Store, value: rusqlite::Result<T>) -> IvyResult<T> {
    value.map_err(|e| store.query_error(e))
}
fn decode(value: &str) -> IvyResult<IvyJson> {
    let value = parse(value, usize::MAX).map_err(|_| IvyError::internal())?;
    require_object(&value)?;
    Ok(value)
}
fn encoded(value: &IvyJson) -> IvyResult<String> {
    canonical(value, MANAGEMENT_FRAME_BYTES)
}
fn text(value: &JsString) -> String {
    String::from_utf16_lossy(&value.0)
}
fn require_object(value: &IvyJson) -> IvyResult<()> {
    if value.as_object().is_none() {
        Err(IvyError::internal())
    } else {
        Ok(())
    }
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
fn optional_array<'a>(value: &'a IvyJson, key: &str) -> IvyResult<&'a [IvyJson]> {
    match value.get(key).filter(|v| !v.is_null()) {
        Some(v) => v.as_array().ok_or_else(IvyError::internal),
        None => Ok(&[]),
    }
}
fn join(parts: impl IntoIterator<Item = JsString>) -> JsString {
    JsString(parts.into_iter().flat_map(|v| v.0).collect())
}
fn require(value: bool, code: &str, message: &str) -> IvyResult<()> {
    if value {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn is_text(value: Option<&IvyJson>, expected: &str) -> bool {
    value.and_then(IvyJson::as_js_string) == Some(&expected.into())
}
fn iso(runtime: &dyn Runtime, now: f64) -> IvyResult<JsString> {
    runtime.iso_date(now)
}
fn generation_result(
    generation: Option<f64>,
    fields: impl IntoIterator<Item = (&'static str, IvyJson)>,
) -> IvyJson {
    let mut result = IvyJson::object(fields);
    if let Some(generation) = generation {
        result.insert("generation", IvyJson::Number(generation));
    }
    result
}

impl Registry {
    fn record_contact(&mut self, id: JsString, at: JsString) {
        self.contacts.entry(id).or_default().at = at;
    }
    pub fn new(store: &mut Store, runtime: &dyn Runtime, now: f64) -> IvyResult<Self> {
        let mut result = Self::default();
        let at = iso(runtime, now)?;
        let retained = {
            let mut statement = sql(
                store,
                store
                    .connection()
                    .prepare_cached("SELECT diagnostic_json FROM diagnostics
                        WHERE json_extract(diagnostic_json,'$.source')='hive'
                        AND json_extract(diagnostic_json,'$.status')='current'
                        AND json_extract(diagnostic_json,'$.code') IN ('invalid_frame','stale_generation')
                        ORDER BY rowid"),
            )?;
            sql(
                store,
                sql(store, statement.query_map([], |r| r.get::<_, String>(0)))?
                    .collect::<rusqlite::Result<Vec<_>>>(),
            )?
        };
        for value in retained {
            let mut value = decode(&value)?;
            require_object(&value)?;
            if is_text(value.get("source"), "hive")
                && is_text(value.get("status"), "current")
                && ["invalid_frame", "stale_generation"]
                    .iter()
                    .any(|code| is_text(value.get("code"), code))
            {
                value.insert("lastObservedAt", IvyJson::String(at.clone()));
                value.insert("status", "resolved".into());
                result.diagnostic(store, &value, runtime, now)?;
            }
        }
        Ok(result)
    }
    fn mark_node(&mut self, id: JsString) {
        if self.changed_node_set.insert(id.clone()) {
            self.changed_nodes.push(id);
        }
    }
    fn mark_generation(&mut self, id: JsString, generation: f64) {
        if let Some(index) = self.generation_indices.get(&id) {
            self.changed_generations[*index].1 = generation;
        } else {
            self.generation_indices
                .insert(id.clone(), self.changed_generations.len());
            self.changed_generations.push((id, generation));
        }
    }
    pub fn changed_nodes(&self) -> &[JsString] {
        &self.changed_nodes
    }
    pub fn changed_generations(&self) -> &[(JsString, f64)] {
        &self.changed_generations
    }
    pub fn take_changed_nodes(&mut self) -> Vec<JsString> {
        self.changed_node_set.clear();
        std::mem::take(&mut self.changed_nodes)
    }
    pub fn take_changed_generations(&mut self) -> Vec<(JsString, f64)> {
        self.generation_indices.clear();
        std::mem::take(&mut self.changed_generations)
    }

    fn current_node(
        &self,
        store: &Store,
        node: &IvyJson,
        diagnostics: Option<&[IvyJson]>,
        runtime: &dyn Runtime,
        now: f64,
    ) -> IvyResult<IvyJson> {
        require_object(node)?;
        let id = node.get("serviceNodeId").and_then(IvyJson::as_js_string);
        let last = id
            .and_then(|id| self.contacts.get(id).map(|contact| &contact.at))
            .or_else(|| node.get("lastContactAt").and_then(IvyJson::as_js_string))
            .cloned();
        let stale = node
            .get("connected")
            .and_then(IvyJson::as_bool)
            .unwrap_or(false)
            && last
                .as_ref()
                .is_none_or(|last| now - runtime.parse_date(last) > 10_000.0);
        let stored;
        let diagnostics = if let Some(diagnostics) = diagnostics {
            diagnostics
        } else {
            let id = id.ok_or_else(IvyError::internal)?;
            // Compare decoded JSON inside SQLite: lone UTF-16 units and NUL
            // retain exact identity without a scan/parse fallback in Rust.
            let source = encoded(&IvyJson::String(join(["service:".into(), id.clone()])))?;
            let mut statement = sql(store, store.connection().prepare_cached(
                "SELECT diagnostic_json FROM diagnostics
                 WHERE json_extract(diagnostic_json,'$.source')=json_extract(?1,'$')
                   AND json_extract(diagnostic_json,'$.status')='current' ORDER BY rowid",
            ))?;
            stored = sql(
                store,
                statement.query_map(
                    [source],
                    |r| r.get::<_, String>(0),
                ),
            )?
            .map(|r| decode(&sql(store, r)?))
            .collect::<IvyResult<Vec<_>>>()?;
            &stored
        };
        let mut diagnostic: Option<IvyJson> = None;
        let source = join(["service:".into(), string(node, "serviceNodeId")?.clone()]);
        for value in diagnostics {
            require_object(value)?;
            if value.get("source").and_then(IvyJson::as_js_string) != Some(&source)
                || !is_text(value.get("status"), "current")
            {
                continue;
            }
            if diagnostic.is_none()
                || Some(string(value, "lastObservedAt")?)
                    > diagnostic
                        .as_ref()
                        .and_then(|v| v.get("observedAt"))
                        .and_then(IvyJson::as_js_string)
            {
                let mut selected = IvyJson::object(std::iter::empty::<(&str, IvyJson)>());
                for (to, from) in [
                    ("code", "code"),
                    ("severity", "severity"),
                    ("message", "message"),
                    ("observedAt", "lastObservedAt"),
                ] {
                    if let Some(v) = value.get(from) {
                        selected.insert(to, v.clone());
                    }
                }
                diagnostic = Some(selected);
            }
        }
        let mut result = node.clone();
        result.insert(
            "instanceMode",
            node.get("instanceMode")
                .filter(|v| !v.is_null())
                .cloned()
                .unwrap_or_else(|| "multiple".into()),
        );
        if let Some(last) = last {
            result.insert("lastContactAt", IvyJson::String(last));
        } else {
            result.remove("lastContactAt");
        }
        result.insert("stale", stale.into());
        result.insert("diagnostic", diagnostic.unwrap_or(IvyJson::Null));
        Ok(result)
    }
    pub fn node(
        &self,
        store: &Store,
        id: &JsString,
        runtime: &dyn Runtime,
        now: f64,
    ) -> IvyResult<IvyJson> {
        let value = sql(
            store,
            store
                .connection()
                .query_row(
                    "SELECT status_json FROM service_nodes WHERE id=?",
                    [text(id)],
                    |r| r.get::<_, String>(0),
                )
                .optional(),
        )?
        .ok_or_else(|| IvyError::new("not_found", "Service Node not found."))?;
        self.current_node(store, &decode(&value)?, None, runtime, now)
    }
    pub fn nodes(&self, store: &Store, runtime: &dyn Runtime, now: f64) -> IvyResult<Vec<IvyJson>> {
        let mut statement=sql(store,store.connection().prepare_cached("SELECT diagnostic_json FROM diagnostics WHERE json_extract(diagnostic_json,'$.status')='current' ORDER BY rowid"))?;
        let diagnostics = sql(store, statement.query_map([], |r| r.get::<_, String>(0)))?
            .map(|r| decode(&sql(store, r)?))
            .collect::<IvyResult<Vec<_>>>()?;
        let mut statement = sql(
            store,
            store
                .connection()
                .prepare_cached("SELECT status_json FROM service_nodes ORDER BY id"),
        )?;
        sql(store, statement.query_map([], |r| r.get::<_, String>(0)))?
            .map(|r| {
                self.current_node(
                    store,
                    &decode(&sql(store, r)?)?,
                    Some(&diagnostics),
                    runtime,
                    now,
                )
            })
            .collect()
    }
    pub fn save(&mut self, store: &mut Store, node: &IvyJson, notify: bool) -> IvyResult<()> {
        let id = string(node, "serviceNodeId")?;
        sql(
            store,
            store.connection().execute(
                "UPDATE service_nodes SET status_json=? WHERE id=?",
                params![encoded(node)?, text(id)],
            ),
        )?;
        self.mark_node(id.clone());
        if notify {
            store.invalidate(join([
                "services/".into(),
                string(node, "serviceName")?.clone(),
                "/".into(),
                id.clone(),
            ]));
        }
        Ok(())
    }
    pub fn connect(
        &mut self,
        store: &mut Store,
        context: &ConnectionContext<'_>,
        parameters: &IvyJson,
        runtime: &dyn Runtime,
        now: f64,
    ) -> IvyResult<IvyJson> {
        require_object(parameters)?;
        require(
            !is_text(parameters.get("serviceName"), "hive"),
            "invalid_arguments",
            "The Hive service name is reserved for core operations.",
        )?;
        let at = iso(runtime, now)?;
        let id = string(parameters, "serviceNodeId")?.clone();
        let service = string(parameters, "serviceName")?;
        let host = string(parameters, "hostId")?;
        let result=store.transaction(|store|{
            let prior=sql(store,store.connection().query_row("SELECT status_json FROM service_nodes WHERE id=?",[text(&id)],|r|r.get::<_,String>(0)).optional())?.map(|v|decode(&v)).transpose()?;
            let valid=if let Some(prior)=prior.as_ref(){
                prior.get("principalId").and_then(IvyJson::as_js_string)==Some(&context.principal.principal_id)
                    &&prior.get("hostId").and_then(IvyJson::as_js_string)==Some(host)
                    &&prior.get("serviceName").and_then(IvyJson::as_js_string)==Some(service)
            }else{true};
            require(valid,"target_conflict","A Service Node identity cannot change principal, host or service.")?;
            let mode=parameters.get("instanceMode").filter(|v|!v.is_null()).cloned().unwrap_or_else(||"multiple".into());
            let same=sql(store,store.connection().query_row("SELECT id FROM service_nodes WHERE service_name=? AND id<>? AND json_extract(status_json,'$.connected')=1 AND (?='singleton' OR json_extract(status_json,'$.instanceMode')='singleton') LIMIT 1",params![text(service),text(&id),text(mode.as_js_string().ok_or_else(IvyError::internal)?)],|r|r.get::<_,String>(0)).optional())?;
            if same.is_some(){return Err(IvyError::new_js("target_conflict".into(),join(["Service ".into(),service.clone()," already has a connected singleton instance.".into()])));}
            let managed=sql(store,store.connection().query_row("SELECT host_id,desired_enabled FROM managed_nodes WHERE node_id=?",[text(&id)],|r|Ok((r.get::<_,String>(0)?,r.get::<_,i64>(1)?))).optional())?;
            require(managed.as_ref().is_none_or(|(owner,_)|JsString::from(owner.as_str())==*host),"target_conflict","Managed Service Node belongs to another host.")?;
            require(context.generation.is_some_and(|g|g.is_finite()&&g.fract()==0.0&&(1.0..=9_007_199_254_740_991.0).contains(&g)),"stale_generation","Service connection has no live routing generation.")?;
            let mut node=parameters.clone();
            for (key,value) in [
                ("instanceMode",mode),("principalId",IvyJson::String(context.principal.principal_id.clone())),
                ("nativeVersion",parameters.get("nativeVersion").filter(|v|!v.is_null()).cloned().unwrap_or(IvyJson::Null)),
                ("connected",true.into()),("ready",false.into()),("stale",false.into()),("synced",false.into()),("diagnostic",IvyJson::Null),
                ("desiredEnabled",managed.as_ref().map(|(_,n)|IvyJson::Bool(*n==1)).unwrap_or_else(||prior.as_ref().and_then(|v|v.get("desiredEnabled")).filter(|v|!v.is_null()).cloned().unwrap_or(true.into()))),
                ("lastContactAt",IvyJson::String(at.clone())),
            ]{node.insert(key,value);}
            for key in ["lastSuccessfulSyncAt","lastFailedSyncAt","lastObservationAt"]{node.insert(key,prior.as_ref().and_then(|v|v.get(key)).filter(|v|!v.is_null()).cloned().unwrap_or(IvyJson::Null));}
            if prior.is_some(){self.save(store,&node,true)?;}else{
                sql(store,store.connection().execute("INSERT INTO service_nodes(id,principal_id,host_id,service_name,generation,status_json) VALUES (?,?,?,?,1,?)",params![text(&id),text(&context.principal.principal_id),text(host),text(service),encoded(&node)?]))?;
            }
            self.mark_node(id.clone());store.invalidate(join(["services/".into(),service.clone(),"/".into(),id.clone()]));self.mark_generation(id.clone(),context.generation.unwrap());
            sql(store,store.connection().execute("INSERT INTO service_connections VALUES (?,?,?) ON CONFLICT(node_id) DO UPDATE SET credential_digest=excluded.credential_digest,session_digest=excluded.session_digest",params![text(&id),text(&context.principal.credential_digest),context.session_digest.map(text)]))?;
            Ok(generation_result(context.generation,[("serviceNodeId",IvyJson::String(id.clone()))]))
        })?;
        self.contacts.insert(
            id,
            Contact {
                at,
                contracts_acknowledged: false,
            },
        );
        Ok(result)
    }
    pub fn disconnect(
        &mut self,
        store: &mut Store,
        id: &JsString,
        runtime: &dyn Runtime,
        now: f64,
    ) -> IvyResult<()> {
        let mut node = self.node(store, id, runtime, now)?;
        for key in ["connected", "synced", "ready"] {
            node.insert(key, false.into());
        }
        self.save(store, &node, true)?;
        self.contacts.remove(id);
        Ok(())
    }
    pub fn eligible(node: &IvyJson) -> bool {
        ["connected", "synced", "ready", "desiredEnabled"]
            .iter()
            .all(|key| node.get(key).and_then(IvyJson::as_bool) == Some(true))
    }
    pub fn get_registry(&self, store: &Store, id: &JsString) -> IvyResult<Option<IvyJson>> {
        sql(
            store,
            store
                .connection()
                .query_row(
                    "SELECT registry_json FROM registries WHERE node_id=?",
                    [text(id)],
                    |r| r.get::<_, String>(0),
                )
                .optional(),
        )?
        .map(|v| {
            let value = parse(&v, usize::MAX).map_err(|_| IvyError::internal())?;
            if value.is_null() {
                Ok(None)
            } else {
                require_object(&value)?;
                Ok(Some(value))
            }
        })
        .transpose()
        .map(Option::flatten)
    }
    pub(crate) fn required_contracts(
        &self,
        store: &Store,
        id: &JsString,
    ) -> IvyResult<Option<IvyJson>> {
        let row = sql(store, store.connection().query_row(
            "SELECT json_type(registry_json),registry_json -> '$.requiredContracts' FROM registries WHERE node_id=?",
            [text(id)], |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
        ).optional())?;
        let Some((kind, value)) = row else {
            return Ok(None);
        };
        if kind == "null" {
            return Ok(None);
        }
        if kind != "object" {
            return Err(IvyError::internal());
        }
        Ok(Some(
            parse(value.as_deref().unwrap_or("null"), usize::MAX)
                .map_err(|_| IvyError::internal())?,
        ))
    }
    pub(crate) fn mcp_names(&self, store: &Store, id: &JsString) -> IvyResult<Vec<JsString>> {
        let mut statement = sql(store, store.connection().prepare_cached(
            "SELECT registry_json FROM registries WHERE node_id=?",
        ))?;
        let mut rows = sql(store, statement.query([text(id)]))?;
        let Some(row) = sql(store, rows.next())? else { return Ok(Vec::new()) };
        let source = sql(store, row.get_ref(0))?.as_str().map_err(|_| IvyError::internal())?;
        namespaces::mcp_names(source)
    }
    pub(crate) fn topic_namespaces(
        &self,
        store: &Store,
        id: &JsString,
        namespace: Option<&JsString>,
    ) -> IvyResult<Vec<namespaces::TopicNamespace>> {
        let mut statement = sql(store, store.connection().prepare_cached(
            "SELECT registry_json FROM registries WHERE node_id=?",
        ))?;
        let mut rows = sql(store, statement.query([text(id)]))?;
        let Some(row) = sql(store, rows.next())? else { return Ok(Vec::new()) };
        let source = sql(store, row.get_ref(0))?.as_str().map_err(|_| IvyError::internal())?;
        namespaces::topics(source, namespace)
    }
    pub fn namespace_names(&self, store: &Store, id: &JsString) -> IvyResult<Vec<IvyJson>> {
        let mut statement = sql(store, store.connection().prepare_cached(
            "SELECT registry_json FROM registries WHERE node_id=?",
        ))?;
        let mut rows = sql(store, statement.query([text(id)]))?;
        let Some(row) = sql(store, rows.next())? else {
            return Ok(Vec::new());
        };
        let source = sql(store, row.get_ref(0))?.as_str().map_err(|_| IvyError::internal())?;
        namespaces::names(source)
    }
    pub fn namespace(
        &self,
        store: &Store,
        id: &JsString,
        namespace: &JsString,
    ) -> IvyResult<IvyJson> {
        self.read_namespace(store, id, namespace, |selected| {
            namespaces::value(selected.ok_or_else(namespaces::not_found)?)
        })
    }
    fn read_namespace<T>(
        &self,
        store: &Store,
        id: &JsString,
        namespace: &JsString,
        read: impl FnOnce(Option<&serde_json::value::RawValue>) -> IvyResult<T>,
    ) -> IvyResult<T> {
        let mut statement = sql(store, store.connection().prepare_cached(
            "SELECT registry_json FROM registries WHERE node_id=?",
        ))?;
        let mut rows = sql(store, statement.query([text(id)]))?;
        let Some(row) = sql(store, rows.next())? else {
            return read(None);
        };
        let source = sql(store, row.get_ref(0))?.as_str().map_err(|_| IvyError::internal())?;
        read(namespaces::find(source, namespace)?)
    }
    pub fn check_requirements(
        &self,
        store: &Store,
        requirements: &[IvyJson],
        allow_unregistered: bool,
    ) -> IvyResult<()> {
        for required in requirements {
            let key = string(required, "key")?;
            let registered = sql(
                store,
                store
                    .connection()
                    .query_row(
                        "SELECT 1 FROM contracts WHERE key=? LIMIT 1",
                        [text(key)],
                        |r| r.get::<_, i64>(0),
                    )
                    .optional(),
            )?
            .is_some();
            if !registered && allow_unregistered {
                continue;
            }
            require(
                registered,
                "contract_not_found",
                "Required contract is not registered.",
            )?;
            for version in array(required, "writeVersions")? {
                store.contract(key, version.as_js_string().ok_or_else(IvyError::internal)?)?;
            }
            let stored = crate::storage_read::contract_versions(
                store.connection(),
                key,
                required.get("readScope"),
            )?;
            let readable = array(required, "readVersions")?;
            require(
                stored
                    .iter()
                    .all(|v| readable.contains(&IvyJson::from(v.as_str()))),
                "contract_version_conflict",
                "Required Object and evidence versions are not all readable by this component.",
            )?;
        }
        Ok(())
    }
    pub fn select(
        &self,
        store: &Store,
        namespace: &JsString,
        target: &IvyJson,
        runtime: &dyn Runtime,
        now: f64,
    ) -> IvyResult<IvyJson> {
        require_object(target)?;
        let resource = target
            .get("resourceRef")
            .filter(|v| v.as_object().is_some());
        let explicit = target
            .get("serviceNodeId")
            .filter(|v| !v.is_null())
            .or_else(|| resource.and_then(|v| v.get("serviceNodeId")));
        require(
            target
                .get("serviceNodeId")
                .and_then(IvyJson::as_js_string)
                .is_none_or(|v| v.0.is_empty())
                || resource.is_none()
                || target.get("serviceNodeId") == resource.and_then(|v| v.get("serviceNodeId")),
            "target_conflict",
            "Resource and node selectors disagree.",
        )?;
        require(
            resource.is_none_or(|v| {
                v.get("namespace").and_then(IvyJson::as_js_string) == Some(namespace)
            }),
            "target_conflict",
            "Resource and namespace selectors disagree.",
        )?;
        if let Some(id) = explicit
            .and_then(IvyJson::as_js_string)
            .filter(|v| !v.0.is_empty())
        {
            let node = self.node(store, id, runtime, now)?;
            require(
                ["hostId", "serviceName"].iter().all(|key| {
                    target
                        .get(key)
                        .and_then(IvyJson::as_js_string)
                        .is_none_or(|v| {
                            v.0.is_empty()
                                || node.get(key).and_then(IvyJson::as_js_string) == Some(v)
                        })
                }),
                "target_conflict",
                "The selected resource has a different owner.",
            )?;
            self.read_namespace(store, string(&node, "serviceNodeId")?, namespace, |selected| {
                selected.ok_or_else(namespaces::not_found).map(|_| ())
            })?;
            return Ok(node);
        }
        let mut eligible = vec![];
        for node in self.nodes(store, runtime, now)? {
            if ["hostId", "serviceName"].iter().any(|key| {
                target
                    .get(key)
                    .and_then(IvyJson::as_js_string)
                    .is_some_and(|v| {
                        !v.0.is_empty() && Some(v) != node.get(key).and_then(IvyJson::as_js_string)
                    })
            }) {
                continue;
            }
            let contains = self.read_namespace(store, string(&node, "serviceNodeId")?, namespace,
                |selected| Ok(selected.is_some()))?;
            if contains && Self::eligible(&node) {
                eligible.push(node);
            }
        }
        if eligible.len() > 1 {
            return Err(
                IvyError::new("ambiguous_service_node", "Choose the owning Service Node.")
                    .with_details(IvyJson::Array(
                        eligible
                            .iter()
                            .map(|n| {
                                IvyJson::object([
                                    ("serviceNodeId", n.get("serviceNodeId").unwrap().clone()),
                                    ("hostId", n.get("hostId").unwrap().clone()),
                                    ("serviceName", n.get("serviceName").unwrap().clone()),
                                ])
                            })
                            .collect(),
                    )),
            );
        }
        require(
            eligible.len() == 1,
            "service_unavailable",
            "No eligible provider matches the target.",
        )?;
        Ok(eligible.remove(0))
    }
    pub fn snapshot(
        &self,
        store: &Store,
        namespace: &JsString,
        target: &IvyJson,
        runtime: &dyn Runtime,
        now: f64,
    ) -> IvyResult<IvyJson> {
        let node = self.select(store, namespace, target, runtime, now)?;
        let ns = self.read_namespace(store, string(&node, "serviceNodeId")?, namespace, |selected| {
            namespaces::summary(selected.ok_or_else(namespaces::not_found)?)
        })?;
        Ok(IvyJson::object([
            ("namespace", IvyJson::String(namespace.clone())),
            ("description", ns.description),
            ("guideMarkdown", ns.guide_markdown),
            (
                "provider",
                IvyJson::object([("eligible", Self::eligible(&node).into()), ("node", node)]),
            ),
            (
                "toolCount",
                IvyJson::Number(ns.tool_count as f64),
            ),
            (
                "topicCount",
                IvyJson::Number(ns.topic_count as f64),
            ),
        ]))
    }
    pub fn bindings(
        &mut self,
        store: &Store,
        id: &JsString,
        namespace: &JsString,
    ) -> IvyResult<Vec<IvyJson>> {
        let catalog = self.live_catalog(store, id)?;
        Ok(catalog.and_then(|catalog| catalog.namespaces.iter()
            .find(|ns| &ns.name == namespace).map(|ns| catalog.bindings(ns).iter()
                .map(ToolBinding::to_wire).collect())).unwrap_or_default())
    }
    pub fn live_catalog(&mut self, store: &Store, id: &JsString) -> IvyResult<Option<Arc<Catalog>>> {
        if let Some(catalog) = self.catalogs.get(id) {
            return Ok(Some(catalog));
        }
        let mut statement = sql(store, store.connection().prepare_cached(
            "SELECT registry_json FROM registries WHERE node_id=?",
        ))?;
        let mut rows = sql(store, statement.query([text(id)]))?;
        let Some(row) = sql(store, rows.next())? else { return Ok(None) };
        let source = sql(store, row.get_ref(0))?.as_str().map_err(|_| IvyError::internal())?;
        Ok(namespaces::live_catalog(source)?.map(|catalog| self.catalogs.insert(id.clone(), catalog)))
    }
    pub fn report_host(
        &mut self,
        store: &mut Store,
        id: &JsString,
        snapshot: &IvyJson,
        runtime: &dyn Runtime,
        now: f64,
    ) -> IvyResult<IvyJson> {
        let reporter = self.node(store, id, runtime, now)?;
        let host = field(snapshot, "status")?;
        require(
            is_text(reporter.get("serviceName"), "service-manager")
                && reporter.get("hostId") == host.get("hostId"),
            "target_conflict",
            "Host observation requires its current local ServiceManager.",
        )?;
        crate::paging::utc(field(host, "observedAt")?, runtime)?;
        require(
            runtime.parse_date(string(host, "observedAt")?) <= now + 1000.0,
            "invalid_arguments",
            "Host observation is in the future.",
        )?;
        let instances = array(host, "instances")?;
        let unique = |key: &str| -> IvyResult<bool> {
            let mut selected = HashSet::new();
            for value in instances {
                if !selected.insert(string(value, key)?.clone()) {
                    return Ok(false);
                }
            }
            Ok(true)
        };
        require(
            unique("instanceId")? && unique("serviceNodeId")?,
            "invalid_arguments",
            "Host snapshot contains duplicate instance/node identities.",
        )?;
        let encoded = canonical(snapshot, 1024 * 1024)?;
        let host_id = string(host, "hostId")?;
        let sequence = snapshot
            .get("sequence")
            .and_then(IvyJson::as_f64)
            .filter(|v| {
                v.is_finite() && v.fract() == 0.0 && (0.0..=9_007_199_254_740_991.0).contains(v)
            })
            .ok_or_else(|| {
                IvyError::new(
                    "invalid_arguments",
                    "Host observation sequence must be a nonnegative integer.",
                )
            })?;
        store.transaction(|store|{
            let prior=sql(store,store.connection().query_row("SELECT sequence,reported_at,snapshot_json FROM host_reports WHERE host_id=?",[text(host_id)],|r|Ok((r.get::<_,f64>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?))).optional())?;
            if let Some((old,at,content))=prior{
                require(sequence>=old,"revision_conflict","Older host observation cannot replace a newer sequence.")?;
                if sequence==old{require(encoded==content,"mutation_conflict","Host observation sequence already identifies different content.")?;return Ok(IvyJson::object([("sequence",IvyJson::Number(sequence)),("reportedAt",at.into())]));}
            }
            for instance in instances{
                let node_id=string(instance,"serviceNodeId")?;
                let known=sql(store,store.connection().query_row("SELECT status_json FROM service_nodes WHERE id=?",[text(node_id)],|r|r.get::<_,String>(0)).optional())?.map(|v|decode(&v)).transpose()?;
                let managed=sql(store,store.connection().query_row("SELECT host_id FROM managed_nodes WHERE node_id=?",[text(node_id)],|r|r.get::<_,String>(0)).optional())?;
                require((known.as_ref().is_none_or(|v|v.get("hostId").and_then(IvyJson::as_js_string)==Some(host_id)))&&managed.is_none_or(|v|JsString::from(v)==*host_id),"target_conflict","Reported Service Node belongs to another host.")?;
                let enabled=field(instance,"desiredEnabled")?;
                sql(store,store.connection().execute("INSERT INTO managed_nodes VALUES (?,?,?) ON CONFLICT(node_id) DO UPDATE SET desired_enabled=excluded.desired_enabled",params![text(node_id),text(host_id),i64::from(enabled.as_bool().ok_or_else(IvyError::internal)?)]))?;
                if let Some(mut node)=known{let notify=node.get("desiredEnabled")!=Some(enabled);node.insert("desiredEnabled",enabled.clone());self.save(store,&node,notify)?;}
            }
            let at=iso(runtime,now)?;
            sql(store,store.connection().execute("INSERT INTO host_reports VALUES (?,?,?,?,?) ON CONFLICT(host_id) DO UPDATE SET node_id=excluded.node_id,sequence=excluded.sequence,reported_at=excluded.reported_at,snapshot_json=excluded.snapshot_json",params![text(host_id),text(string(&reporter,"serviceNodeId")?),sequence,text(&at),encoded]))?;
            store.invalidate("system/hosts".into());Ok(IvyJson::object([("sequence",IvyJson::Number(sequence)),("reportedAt",IvyJson::String(at))]))
        })
    }
    pub fn host_observations(
        &self,
        store: &Store,
        host: Option<&JsString>,
        runtime: &dyn Runtime,
        now: f64,
    ) -> IvyResult<Vec<IvyJson>> {
        let mut statement=sql(store,store.connection().prepare_cached("SELECT node_id,reported_at,snapshot_json FROM host_reports WHERE (? IS NULL OR host_id=?) ORDER BY host_id LIMIT 1001"))?;
        sql(
            store,
            statement.query_map(params![host.map(text), host.map(text)], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            }),
        )?
        .map(|row| {
            let (id, at, snapshot) = sql(store, row)?;
            let snapshot = decode(&snapshot)?;
            let reporter = self.node(store, &id.into(), runtime, now)?;
            Ok(IvyJson::object([
                ("snapshot", snapshot),
                ("serviceNodeId", field(&reporter, "serviceNodeId")?.clone()),
                ("reportedAt", at.into()),
                (
                    "stale",
                    (reporter.get("connected").and_then(IvyJson::as_bool) != Some(true)
                        || reporter.get("ready").and_then(IvyJson::as_bool) != Some(true)
                        || reporter.get("stale").and_then(IvyJson::as_bool) == Some(true))
                    .into(),
                ),
            ]))
        })
        .collect()
    }
}
