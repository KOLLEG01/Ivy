//! Durable registry state and the storage lifetime's contact/binding mirrors.
//! Live generation authority is supplied by dispatch, never the legacy column.
use crate::{
    retention::Runtime,
    store::{AuthenticatedContext, Store},
};
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString,
    core_schema::validate_shared,
    hash::{hash_json, tool_definition_hash},
    json::{canonical, number_from_string, parse, try_js_string},
    limits::MANAGEMENT_FRAME_BYTES,
};
use rusqlite::{OptionalExtension, params};
use std::{
    cmp::Ordering,
    collections::{HashMap, HashSet},
};
#[path = "registry_diagnostics.rs"]
mod diagnostics;
#[path = "registry_sync.rs"]
mod syncing;

pub const RESOLVED_DIAGNOSTIC_LIMIT: usize = 10_000;
pub const ACTIVE_DIAGNOSTIC_LIMIT: usize = 4_096;

pub struct ConnectionContext<'a> {
    pub principal: &'a AuthenticatedContext,
    pub session_digest: Option<&'a JsString>,
    pub generation: Option<f64>,
}
#[derive(Default)]
pub struct Registry {
    changed_nodes: Vec<JsString>,
    changed_node_set: HashSet<JsString>,
    changed_generations: Vec<(JsString, f64)>,
    generation_indices: HashMap<JsString, usize>,
    binding_catalog: HashMap<JsString, HashMap<JsString, Vec<IvyJson>>>,
    contact_times: HashMap<JsString, JsString>,
}
fn sql<T>(store: &Store, value: rusqlite::Result<T>) -> IvyResult<T> {
    value.map_err(|e| store.query_error(e))
}
fn decode(value: &str) -> IvyResult<IvyJson> {
    parse(value, usize::MAX).map_err(|_| IvyError::internal())
}
fn encoded(value: &IvyJson) -> IvyResult<String> {
    canonical(value, MANAGEMENT_FRAME_BYTES)
}
fn text(value: &JsString) -> String {
    String::from_utf16_lossy(&value.0)
}
fn non_null(value: &IvyJson) -> IvyResult<()> {
    if value.is_null() {
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
fn truthy(value: Option<&IvyJson>) -> bool {
    match value {
        None | Some(IvyJson::Null) => false,
        Some(IvyJson::Bool(v)) => *v,
        Some(IvyJson::Number(v)) => *v != 0.0 && !v.is_nan(),
        Some(IvyJson::String(v)) => !v.0.is_empty(),
        _ => true,
    }
}
fn number(value: Option<&IvyJson>) -> IvyResult<f64> {
    Ok(match value {
        None => f64::NAN,
        Some(IvyJson::Null) => 0.0,
        Some(IvyJson::Bool(v)) => u8::from(*v) as f64,
        Some(IvyJson::Number(v)) => *v,
        Some(v) => number_from_string(&try_js_string(v)?),
    })
}
fn greater(a: Option<&IvyJson>, b: Option<&IvyJson>) -> IvyResult<bool> {
    Ok(match (a, b) {
        (Some(IvyJson::String(a)), Some(IvyJson::String(b))) => a > b,
        _ => number(a)? > number(b)?,
    })
}
fn spread(value: &IvyJson) -> IvyJson {
    match value {
        IvyJson::Object(_) => value.clone(),
        IvyJson::Array(v) => IvyJson::object(
            v.iter()
                .enumerate()
                .map(|(i, v)| (i.to_string(), v.clone())),
        ),
        IvyJson::String(v) => IvyJson::object(
            v.0.iter()
                .enumerate()
                .map(|(i, u)| (i.to_string(), IvyJson::String(JsString(vec![*u])))),
        ),
        _ => IvyJson::object(std::iter::empty::<(&str, IvyJson)>()),
    }
}
fn join(parts: impl IntoIterator<Item = JsString>) -> JsString {
    JsString(parts.into_iter().flat_map(|v| v.0).collect())
}
fn js_field(value: &IvyJson, key: &str) -> IvyResult<JsString> {
    non_null(value)?;
    value
        .get(key)
        .map(try_js_string)
        .transpose()
        .map(|v| v.unwrap_or_else(|| "undefined".into()))
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
    pub fn new(store: &mut Store, runtime: &dyn Runtime, now: f64) -> IvyResult<Self> {
        let mut result = Self::default();
        let at = iso(runtime, now)?;
        let retained = {
            let mut statement = sql(
                store,
                store
                    .connection()
                    .prepare_cached("SELECT diagnostic_json FROM diagnostics"),
            )?;
            sql(
                store,
                sql(store, statement.query_map([], |r| r.get::<_, String>(0)))?
                    .collect::<rusqlite::Result<Vec<_>>>(),
            )?
        };
        for value in retained {
            let mut value = decode(&value)?;
            non_null(&value)?;
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
        non_null(node)?;
        let id = node.get("serviceNodeId").and_then(IvyJson::as_js_string);
        let last = id
            .and_then(|id| self.contact_times.get(id))
            .map(|v| IvyJson::String(v.clone()))
            .or_else(|| node.get("lastContactAt").cloned());
        let stale = if truthy(node.get("connected")) {
            Some(IvyJson::Bool(
                !truthy(last.as_ref())
                    || now - runtime.parse_date(&try_js_string(last.as_ref().unwrap())?) > 10_000.0,
            ))
        } else {
            node.get("connected").cloned()
        };
        let stored;
        let diagnostics = if let Some(diagnostics) = diagnostics {
            diagnostics
        } else {
            let id = id.ok_or_else(IvyError::internal)?;
            let mut statement=sql(store,store.connection().prepare_cached("SELECT diagnostic_json FROM diagnostics WHERE json_extract(diagnostic_json,'$.resource.serviceNodeId')=?"))?;
            stored = sql(
                store,
                statement.query_map([text(id)], |r| r.get::<_, String>(0)),
            )?
            .map(|r| decode(&sql(store, r)?))
            .collect::<IvyResult<Vec<_>>>()?;
            &stored
        };
        let mut diagnostic: Option<IvyJson> = None;
        let source = join(["service:".into(), js_field(node, "serviceNodeId")?]);
        for value in diagnostics {
            non_null(value)?;
            if value.get("source").and_then(IvyJson::as_js_string) != Some(&source)
                || !is_text(value.get("status"), "current")
            {
                continue;
            }
            if diagnostic.is_none()
                || greater(
                    value.get("lastObservedAt"),
                    diagnostic.as_ref().and_then(|v| v.get("observedAt")),
                )?
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
        let mut result = spread(node);
        result.insert(
            "instanceMode",
            node.get("instanceMode")
                .filter(|v| !v.is_null())
                .cloned()
                .unwrap_or_else(|| "multiple".into()),
        );
        if let Some(last) = last {
            result.insert("lastContactAt", last);
        } else {
            result.remove("lastContactAt");
        }
        if let Some(stale) = stale {
            result.insert("stale", stale);
        } else {
            result.remove("stale");
        }
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
        let mut statement=sql(store,store.connection().prepare_cached("SELECT diagnostic_json FROM diagnostics WHERE json_extract(diagnostic_json,'$.status')='current'"))?;
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
                js_field(node, "serviceName")?,
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
        non_null(parameters)?;
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
            let valid=if let Some(prior)=prior.as_ref().filter(|v|truthy(Some(v))){
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
            let mut node=spread(parameters);
            for (key,value) in [
                ("instanceMode",mode),("principalId",IvyJson::String(context.principal.principal_id.clone())),
                ("nativeVersion",parameters.get("nativeVersion").filter(|v|!v.is_null()).cloned().unwrap_or(IvyJson::Null)),
                ("connected",true.into()),("ready",false.into()),("stale",false.into()),("synced",false.into()),("diagnostic",IvyJson::Null),
                ("desiredEnabled",managed.as_ref().map(|(_,n)|IvyJson::Bool(*n==1)).unwrap_or_else(||prior.as_ref().and_then(|v|v.get("desiredEnabled")).filter(|v|!v.is_null()).cloned().unwrap_or(true.into()))),
                ("lastContactAt",IvyJson::String(at.clone())),
            ]{node.insert(key,value);}
            for key in ["lastSuccessfulSyncAt","lastFailedSyncAt","lastObservationAt"]{node.insert(key,prior.as_ref().and_then(|v|v.get(key)).filter(|v|!v.is_null()).cloned().unwrap_or(IvyJson::Null));}
            if prior.as_ref().is_some_and(|v|truthy(Some(v))){self.save(store,&node,true)?;}else{
                sql(store,store.connection().execute("INSERT INTO service_nodes(id,principal_id,host_id,service_name,generation,status_json) VALUES (?,?,?,?,1,?)",params![text(&id),text(&context.principal.principal_id),text(host),text(service),encoded(&node)?]))?;
            }
            self.mark_node(id.clone());store.invalidate(join(["services/".into(),service.clone(),"/".into(),id.clone()]));self.mark_generation(id.clone(),context.generation.unwrap());
            sql(store,store.connection().execute("INSERT INTO service_connections VALUES (?,?,?) ON CONFLICT(node_id) DO UPDATE SET credential_digest=excluded.credential_digest,session_digest=excluded.session_digest",params![text(&id),text(&context.principal.credential_digest),context.session_digest.map(text)]))?;
            Ok(generation_result(context.generation,[("serviceNodeId",IvyJson::String(id.clone()))]))
        })?;
        self.contact_times.insert(id, at);
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
        self.contact_times.remove(id);
        Ok(())
    }
    pub fn eligible(node: &IvyJson) -> bool {
        ["connected", "synced", "ready", "desiredEnabled"]
            .iter()
            .all(|key| truthy(node.get(key)))
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
        .map(|v| decode(&v))
        .transpose()
    }
    pub fn namespace(
        &self,
        store: &Store,
        id: &JsString,
        namespace: &JsString,
    ) -> IvyResult<IvyJson> {
        let registry = self.get_registry(store, id)?;
        let mut selected = None;
        if let Some(registry) = registry.as_ref().filter(|v| !v.is_null()) {
            for value in array(registry, "namespaces")? {
                non_null(value)?;
                if value.get("namespace").and_then(IvyJson::as_js_string) == Some(namespace) {
                    selected = Some(value.clone());
                    break;
                }
            }
        }
        selected.ok_or_else(|| {
            IvyError::new(
                "not_found",
                "Namespace is not in the selected provider catalog.",
            )
        })
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
        non_null(target)?;
        let resource = target.get("resourceRef").filter(|v| truthy(Some(v)));
        let explicit = target
            .get("serviceNodeId")
            .filter(|v| !v.is_null())
            .or_else(|| resource.and_then(|v| v.get("serviceNodeId")));
        require(
            !truthy(target.get("serviceNodeId"))
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
        if let Some(id) = explicit.filter(|v| truthy(Some(v))) {
            let node = self.node(
                store,
                id.as_js_string().ok_or_else(IvyError::internal)?,
                runtime,
                now,
            )?;
            require(
                ["hostId", "serviceName"]
                    .iter()
                    .all(|key| !truthy(target.get(key)) || node.get(key) == target.get(key)),
                "target_conflict",
                "The selected resource has a different owner.",
            )?;
            self.namespace(store, string(&node, "serviceNodeId")?, namespace)?;
            return Ok(node);
        }
        let mut eligible = vec![];
        for node in self.nodes(store, runtime, now)? {
            if ["hostId", "serviceName"]
                .iter()
                .any(|key| truthy(target.get(key)) && target.get(key) != node.get(key))
            {
                continue;
            }
            let registry = self.get_registry(store, string(&node, "serviceNodeId")?)?;
            let mut contains = false;
            if let Some(registry) = registry.as_ref().filter(|v| !v.is_null()) {
                for value in array(registry, "namespaces")? {
                    non_null(value)?;
                    if value.get("namespace").and_then(IvyJson::as_js_string) == Some(namespace) {
                        contains = true;
                        break;
                    }
                }
            }
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
        let ns = self.namespace(store, string(&node, "serviceNodeId")?, namespace)?;
        Ok(IvyJson::object([
            ("namespace", IvyJson::String(namespace.clone())),
            ("description", field(&ns, "description")?.clone()),
            ("guideMarkdown", field(&ns, "guideMarkdown")?.clone()),
            (
                "provider",
                IvyJson::object([("eligible", Self::eligible(&node).into()), ("node", node)]),
            ),
            (
                "toolCount",
                IvyJson::Number(array(&ns, "tools")?.len() as f64),
            ),
            (
                "topicCount",
                IvyJson::Number(array(&ns, "topics")?.len() as f64),
            ),
        ]))
    }
    pub fn bindings(
        &mut self,
        store: &Store,
        id: &JsString,
        namespace: &IvyJson,
    ) -> IvyResult<&[IvyJson]> {
        if !self.binding_catalog.contains_key(id) {
            let mut cache = HashMap::new();
            if let Some(registry) = self.get_registry(store, id)?.filter(|v| !v.is_null()) {
                for ns in array(&registry, "namespaces")? {
                    cache.insert(
                        string(ns, "namespace")?.clone(),
                        tool_bindings(array(ns, "tools")?)?,
                    );
                }
            }
            self.binding_catalog.insert(id.clone(), cache);
        }
        Ok(self.binding_catalog[id]
            .get(string(namespace, "namespace")?)
            .map(Vec::as_slice)
            .unwrap_or(&[]))
    }
    pub fn live_catalog(&mut self, store: &Store, id: &JsString) -> IvyResult<Option<IvyJson>> {
        let Some(registry) = self.get_registry(store, id)?.filter(|v| !v.is_null()) else {
            return Ok(None);
        };
        let mut namespaces = vec![];
        let mut tools = vec![];
        let mut notifications = vec![];
        for ns in array(&registry, "namespaces")? {
            namespaces.push(field(ns, "namespace")?.clone());
            tools.extend_from_slice(self.bindings(store, id, ns)?);
            for notification in optional_array(ns, "notifications")? {
                let mut value = IvyJson::object([("namespace", field(ns, "namespace")?.clone())]);
                for (key, child) in spread(notification)
                    .as_object()
                    .ok_or_else(IvyError::internal)?
                {
                    value.insert(key.clone(), child.clone());
                }
                notifications.push(value);
            }
        }
        Ok(Some(IvyJson::object([
            ("namespaces", IvyJson::Array(namespaces)),
            ("tools", IvyJson::Array(tools)),
            ("notifications", IvyJson::Array(notifications)),
        ])))
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
        let sequence = number(snapshot.get("sequence"))?;
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
                require((known.as_ref().is_none_or(|v|!truthy(Some(v))||v.get("hostId").and_then(IvyJson::as_js_string)==Some(host_id)))&&managed.is_none_or(|v|JsString::from(v)==*host_id),"target_conflict","Reported Service Node belongs to another host.")?;
                let enabled=field(instance,"desiredEnabled")?;
                sql(store,store.connection().execute("INSERT INTO managed_nodes VALUES (?,?,?) ON CONFLICT(node_id) DO UPDATE SET desired_enabled=excluded.desired_enabled",params![text(node_id),text(host_id),number(Some(enabled))?]))?;
                if let Some(mut node)=known.filter(|v|truthy(Some(v))){let notify=node.get("desiredEnabled")!=Some(enabled);node.insert("desiredEnabled",enabled.clone());self.save(store,&node,notify)?;}
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
                    (!truthy(reporter.get("connected"))
                        || !truthy(reporter.get("ready"))
                        || truthy(reporter.get("stale")))
                    .into(),
                ),
            ]))
        })
        .collect()
    }
}
fn tool_bindings(tools: &[IvyJson]) -> IvyResult<Vec<IvyJson>> {
    tools
        .iter()
        .map(|tool| {
            Ok(IvyJson::object([
                (
                    "qualifiedName",
                    IvyJson::String(join([
                        js_field(tool, "namespace")?,
                        ".".into(),
                        js_field(tool, "name")?,
                    ])),
                ),
                ("definition", tool.clone()),
                ("definitionHash", tool_definition_hash(tool)?.into()),
            ]))
        })
        .collect()
}
