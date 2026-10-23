use super::{
    AuthContext, AuthenticatedContext, IvyError, IvyJson, JsString, Root, Store,
    native_with_runtime, oracle,
};
use ivy_contracts::IvyResult;
use ivy_hive::registry::{ConnectionContext, Registry};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
#[path = "hive_configurations.rs"]
mod configurations;
#[path = "hive_discovery.rs"]
mod discovery;
#[path = "hive_inventory.rs"]
mod inventory;
#[path = "hive_mcp.rs"]
mod mcp;
#[path = "hive_mcp_request.rs"]
mod mcp_request;
#[path = "hive_uis.rs"]
mod uis;

#[path = "../fixtures/hive-observed-runtime.rs"]
mod observed_runtime;
pub(super) use observed_runtime::ObservedRuntime;
impl ObservedRuntime {
    pub(super) fn new(profile: &Value) -> Self {
        Self::from_json(&IvyJson::from_serde(profile).unwrap())
    }
}
fn units(value: &Value) -> JsString {
    JsString(
        value
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_u64().unwrap() as u16)
            .collect(),
    )
}

#[derive(Default)]
pub(super) struct Trace {
    registry: Option<Registry>,
    runtime: Option<Arc<ObservedRuntime>>,
    discovery: Option<ivy_hive::discovery::Discovery>,
    configurations: Option<ivy_hive::configurations::Configurations>,
}
impl Trace {
    pub(super) fn reset_owner(&mut self) {
        self.registry = None;
        self.discovery = None;
        self.configurations = None;
    }
    pub(super) fn state(&self) -> Value {
        match self.registry.as_ref() {
            Some(registry) => {
                json!({"changedNodes":registry.changed_nodes().iter().map(|v|&v.0).collect::<Vec<_>>(),"changedGenerations":registry.changed_generations().iter().map(|(id,g)|json!([id.0,super::wire(IvyJson::Number(*g))])).collect::<Vec<_>>()})
            }
            None => Value::Null,
        }
    }
    pub(super) fn execute(
        &mut self,
        store: &mut Store,
        step: &Value,
        profile: &Value,
        database: &std::path::Path,
    ) -> IvyResult<IvyJson> {
        if self.runtime.is_none() {
            self.runtime = Some(Arc::new(ObservedRuntime::new(profile)));
        }
        let runtime = self.runtime.as_ref().unwrap().as_ref();
        let now = step["now"].as_f64().unwrap();
        let method = step["method"].as_str().unwrap();
        let mut parameters = step
            .get("params")
            .map(IvyJson::from_serde)
            .transpose()
            .unwrap()
            .unwrap_or_else(|| IvyJson::object(std::iter::empty::<(&str, IvyJson)>()));
        super::object_write::apply_units(&mut parameters, step);
        if step["op"] == "configurations" {
            if self.configurations.is_none() || method == "constructor" {
                self.configurations = Some(ivy_hive::configurations::Configurations::new(store)?);
            }
            return configurations::execute(
                self.configurations.as_ref().unwrap(),
                store,
                runtime,
                &parameters,
                step,
            );
        }
        if (method == "constructor" && step["op"] != "discovery") || self.registry.is_none() {
            self.registry = Some(Registry::new(store, runtime, now)?);
        }
        let registry = self.registry.as_mut().unwrap();
        if step["op"] == "uis" {
            return uis::execute(registry, store, runtime, &parameters, step, database);
        }
        if step["op"] == "discovery" {
            if self.discovery.is_none() || method == "constructor" {
                self.discovery = Some(ivy_hive::discovery::Discovery::new(runtime)?);
            }
            let discovery = self.discovery.as_mut().unwrap();
            return match method {
                "constructor" => Ok(IvyJson::Null),
                "list" => discovery.list(
                    store,
                    registry,
                    &parameters,
                    step["mcpOnly"] == true,
                    runtime,
                    now,
                ),
                "describe" => discovery.describe(
                    store,
                    registry,
                    &parameters,
                    step["mcpOnly"] == true,
                    runtime,
                    now,
                ),
                "resolveCall" => discovery.resolve_call(&parameters),
                "mcpCatalog" => discovery
                    .mcp_catalog(
                        store,
                        registry,
                        parameters.get("name").and_then(IvyJson::as_js_string),
                        runtime,
                        now,
                    )
                    .map(IvyJson::Array),
                "instructions" => discovery.instructions(store, registry, runtime, now),
                "instructionsPage" => {
                    discovery.instructions_page(store, registry, &parameters, runtime, now)
                }
                "toolSchema" => discovery.tool_schema(store, registry, &parameters, runtime, now),
                "mcpSurface" => {
                    let bindings = match parameters.get("bindings") {
                        Some(v) => v.as_array().ok_or_else(IvyError::internal)?.to_vec(),
                        None => discovery.mcp_catalog(
                            store,
                            registry,
                            parameters.get("name").and_then(IvyJson::as_js_string),
                            runtime,
                            now,
                        )?,
                    };
                    ivy_hive::mcp::surface_bindings(
                        &bindings,
                        parameters
                            .get("surface")
                            .and_then(IvyJson::as_js_string)
                            .ok_or_else(IvyError::internal)?,
                    )
                    .map(IvyJson::Array)
                }
                "mcpSuccess" => ivy_hive::mcp::success(
                    parameters.get("result").ok_or_else(IvyError::internal)?,
                    parameters.get("structured") == Some(&IvyJson::Bool(true)),
                ),
                "mcpRequest" => {
                    mcp_request::execute(store, registry, discovery, runtime, &parameters, now)
                }
                "directDescriptors" | "directCall" | "hasStructuredOutput" => {
                    let mut bindings = match parameters.get("bindings") {
                        Some(v) => v.as_array().ok_or_else(IvyError::internal)?.to_vec(),
                        None => discovery.mcp_catalog(
                            store,
                            registry,
                            parameters.get("name").and_then(IvyJson::as_js_string),
                            runtime,
                            now,
                        )?,
                    };
                    if let Some(surface) = parameters.get("surface") {
                        bindings.retain(|b| {
                            b.get("definition")
                                .and_then(|v| v.get("discovery"))
                                .and_then(|v| v.get("mcp"))
                                .and_then(|v| v.get("surface"))
                                == Some(surface)
                        });
                    }
                    match method {
                        "directDescriptors" => {
                            ivy_hive::mcp::direct_descriptors(&bindings, runtime)
                                .map(IvyJson::Array)
                        }
                        "directCall" => ivy_hive::mcp::direct_call(
                            &bindings,
                            parameters.get("args").ok_or_else(IvyError::internal)?,
                            runtime,
                        ),
                        _ => ivy_hive::mcp::has_structured_output(&bindings).map(IvyJson::Bool),
                    }
                }
                "mcpBundle" => {
                    let mut bundle = ivy_hive::mcp_schema::McpSchemaBundle::new();
                    let mut embeds = vec![];
                    for entry in parameters
                        .get("entries")
                        .and_then(IvyJson::as_array)
                        .ok_or_else(IvyError::internal)?
                    {
                        embeds.push(
                            bundle.embed(
                                entry.get("schema").ok_or_else(IvyError::internal)?,
                                entry
                                    .get("path")
                                    .and_then(IvyJson::as_js_string)
                                    .ok_or_else(IvyError::internal)?,
                            )?,
                        );
                    }
                    Ok(IvyJson::object([
                        ("embeds", IvyJson::Array(embeds)),
                        (
                            "finished",
                            bundle.finish(parameters.get("root").cloned().unwrap_or_else(|| {
                                IvyJson::object([
                                    ("type", "object".into()),
                                    (
                                        "properties",
                                        IvyJson::object(std::iter::empty::<(&str, IvyJson)>()),
                                    ),
                                ])
                            })),
                        ),
                    ]))
                }
                "cursor" => store
                    .cursor(
                        parameters.get("identity").ok_or_else(IvyError::internal)?,
                        parameters.get("boundary").ok_or_else(IvyError::internal)?,
                    )
                    .map(IvyJson::from),
                _ => Err(IvyError::internal()),
            };
        }
        if step["op"] == "inventory" {
            store.bind_query_runtime(self.runtime.as_ref().unwrap().clone());
            return store.with_budget_mut(
                if step["overdue"] == true {
                    Duration::ZERO
                } else {
                    Duration::from_secs(10)
                },
                |store| match method {
                    "sync" => ivy_hive::inventory::sync(
                        store,
                        registry,
                        &step
                            .get("idUnits")
                            .map(units)
                            .or_else(|| step["id"].as_str().map(JsString::from))
                            .ok_or_else(IvyError::internal)?,
                        &parameters,
                        runtime,
                        now,
                    ),
                    "get" => ivy_hive::inventory::get(
                        store,
                        registry,
                        parameters
                            .get("resourceRef")
                            .ok_or_else(IvyError::internal)?,
                        runtime,
                        now,
                    ),
                    "list" => ivy_hive::inventory::list(store, registry, &parameters, runtime, now),
                    "cursor" => store
                        .cursor(
                            parameters.get("identity").ok_or_else(IvyError::internal)?,
                            parameters.get("boundary").ok_or_else(IvyError::internal)?,
                        )
                        .map(IvyJson::from),
                    "sql" => inventory::query_sql(store, step["sql"].as_str().unwrap()),
                    _ => Err(IvyError::internal()),
                },
            );
        }
        let principal = if step["context"].is_object() {
            Some(
                store.authenticate(&AuthContext {
                    credential_digest: step["context"]["credentialDigest"].as_str().unwrap().into(),
                    session_digest: step["context"]["sessionDigest"]
                        .as_str()
                        .map(JsString::from),
                })?,
            )
        } else {
            None
        };
        let id = || {
            step.get("idUnits")
                .map(units)
                .or_else(|| step["id"].as_str().map(JsString::from))
                .ok_or_else(IvyError::internal)
        };
        let generation = step["generation"].as_f64();
        let field = |key: &str| parameters.get(key).ok_or_else(IvyError::internal);
        match method {
            "constructor" => Ok(IvyJson::Null),
            "connect" => {
                let principal = principal.ok_or_else(IvyError::internal)?;
                let principal = AuthenticatedContext {
                    principal_id: principal.principal_id,
                    credential_digest: principal.credential_digest,
                };
                let session = step["context"]["sessionDigest"]
                    .as_str()
                    .map(JsString::from);
                registry.connect(
                    store,
                    &ConnectionContext {
                        principal: &principal,
                        session_digest: session.as_ref(),
                        generation,
                    },
                    &parameters,
                    runtime,
                    now,
                )
            }
            "disconnect" => registry
                .disconnect(store, &id()?, runtime, now)
                .map(|_| IvyJson::Null),
            "node" => registry.node(store, &id()?, runtime, now),
            "nodes" => registry.nodes(store, runtime, now).map(IvyJson::Array),
            "save" => registry
                .save(store, &parameters, step["notify"] != false)
                .map(|_| IvyJson::Null),
            "sync" => registry.sync(store, &id()?, generation, &parameters, runtime, now),
            "heartbeat" => registry.heartbeat(store, &id()?, generation, &parameters, runtime, now),
            "diagnostic" => registry
                .diagnostic(store, &parameters, runtime, now)
                .map(|_| IvyJson::Null),
            "diagnostics" => registry
                .diagnostics(
                    store,
                    parameters.get("status").and_then(IvyJson::as_js_string),
                )
                .map(IvyJson::Array),
            "collectDiagnostics" => registry
                .collect_diagnostics(store, runtime, now)
                .map(|_| IvyJson::Null),
            "checkRequirements" => registry
                .check_requirements(
                    store,
                    field("requirements")?
                        .as_array()
                        .ok_or_else(IvyError::internal)?,
                    parameters.get("allowUnregistered") == Some(&true.into()),
                )
                .map(|_| IvyJson::Null),
            "getRegistry" => registry
                .get_registry(store, &id()?)
                .map(|v| v.unwrap_or(IvyJson::Null)),
            "namespace" => registry.namespace(
                store,
                &id()?,
                field("namespace")?
                    .as_js_string()
                    .ok_or_else(IvyError::internal)?,
            ),
            "select" => registry.select(
                store,
                field("namespace")?
                    .as_js_string()
                    .ok_or_else(IvyError::internal)?,
                field("target")?,
                runtime,
                now,
            ),
            "snapshot" => registry.snapshot(
                store,
                field("namespace")?
                    .as_js_string()
                    .ok_or_else(IvyError::internal)?,
                field("target")?,
                runtime,
                now,
            ),
            "bindings" => registry
                .bindings(store, &id()?, field("namespace")?)
                .map(|v| IvyJson::Array(v.to_vec())),
            "liveCatalog" => registry
                .live_catalog(store, &id()?)
                .map(|v| v.unwrap_or(IvyJson::Null)),
            "reportHost" => registry.report_host(store, &id()?, &parameters, runtime, now),
            "hostObservations" => registry
                .host_observations(
                    store,
                    parameters.get("hostId").and_then(IvyJson::as_js_string),
                    runtime,
                    now,
                )
                .map(IvyJson::Array),
            "clearHints" => {
                registry.take_changed_nodes();
                registry.take_changed_generations();
                store.take_changes();
                Ok(IvyJson::Null)
            }
            "collectRetention" => {
                let mut collect =
                    |store: &mut Store, now| registry.collect_diagnostics(store, runtime, now);
                ivy_hive::retention::collect(store, runtime, now, Some(&mut collect))
            }
            _ => Err(IvyError::internal()),
        }
    }
}

fn sql(text: impl Into<String>) -> Value {
    json!({"op":"sql","sql":text.into()})
}
fn digest(principal: bool) -> String {
    format!("sha256:{}", if principal { "a" } else { "b" }.repeat(64))
}
fn setup() -> Vec<Value> {
    vec![
        sql("UPDATE metadata SET value='epoch' WHERE key='runtime_epoch';"),
        json!({"op":"configure","credentials":[{"principalId":"alpha","digest":digest(true)},{"principalId":"beta","digest":digest(false)}]}),
    ]
}
fn action(method: &str, id: Option<&str>, params: Value) -> Value {
    let mut result =
        json!({"op":"registry","method":method,"params":params,"now":100000,"generation":1});
    if let Some(id) = id {
        result["id"] = json!(id);
    }
    result
}
fn at(mut value: Value, now: u64) -> Value {
    value["now"] = json!(now);
    value
}
fn connect(id: &str, service: &str, host: &str, generation: u64) -> Value {
    let mut result = action(
        "connect",
        None,
        json!({"serviceNodeId":id,"hostId":host,"serviceName":service,"version":"1.0.0","buildId":format!("sha256:{}","c".repeat(64)),"hiveProtocol":1}),
    );
    result["context"] = json!({"credentialDigest":digest(true)});
    result["generation"] = json!(generation);
    result
}
fn catalog(namespace: &str) -> Value {
    json!({"namespaces":[{"namespace":namespace,"description":"Synthetic registry provider","guideMarkdown":"Synthetic guide.","tools":[{"namespace":namespace,"name":"turn/ReadCase","nativeMethod":"turn/ReadCase","nativeSchemaIdentity":"synthetic","interfaceVersion":"1.0.0","description":"Read synthetic data.","inputSchema":{"type":"object","properties":{"value":{"type":"string"}},"required":["value"],"additionalProperties":false},"outputSchema":{"type":"null"}}],"topics":[],"inventoryKinds":[]}],"contracts":[],"requiredContracts":[]})
}
fn heartbeat(id: &str, ready: bool, diagnostics: Vec<Value>) -> Value {
    action(
        "heartbeat",
        Some(id),
        json!({"ready":ready,"diagnostics":diagnostics}),
    )
}
fn diagnostic(code: &str, node: &str, status: &str, first: &str, last: &str) -> Value {
    json!({"code":code,"resource":{"serviceNodeId":node},"severity":"error","source":format!("service:{node}"),"firstObservedAt":first,"lastObservedAt":last,"status":status,"message":"Synthetic diagnostic."})
}
fn compare(steps: &[Value]) -> Value {
    let root = Root::new();
    let expected = oracle(&root.0.join("reference.sqlite"), steps);
    let actual = native_with_runtime(
        &root.0.join("native.sqlite"),
        steps,
        Some(&expected["registryRuntime"]),
    );
    if let Some(path) = std::env::var_os("IVY_HIVE_REGISTRY_CORPUS") {
        use std::io::Write;
        writeln!(
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .unwrap(),
            "{}",
            json!({"steps": steps, "original": expected, "native": actual})
        )
        .unwrap();
    }
    assert_eq!(actual["rows"].as_array().unwrap().len(), steps.len());
    for (index, (left, right)) in actual["rows"]
        .as_array()
        .unwrap()
        .iter()
        .zip(expected["rows"].as_array().unwrap())
        .enumerate()
    {
        assert_eq!(
            left, right,
            "Registry trace step {index} ({}/{})",
            steps[index]["op"], steps[index]["method"]
        );
        if matches!(
            steps[index]["op"].as_str(),
            Some("sql" | "configure" | "register")
        ) || steps[index]["requireSuccess"] == true
        {
            assert!(
                right["error"].is_null(),
                "Fixture setup failed at {index}: {}",
                right["error"]
            );
        }
    }
    for key in ["definitions", "metadata", "topics"] {
        assert_eq!(actual[key], expected[key]);
    }
    expected
}
fn value(rows: &Value, index: usize) -> &Value {
    &rows["rows"][index]["value"]
}
fn state(rows: &Value, index: usize) -> &Value {
    &rows["rows"][index]["state"]
}
fn code(rows: &Value, index: usize) -> &Value {
    &rows["rows"][index]["error"]["data"]["code"]
}
fn success(rows: &Value, index: usize) {
    assert!(
        rows["rows"][index]["error"].is_null(),
        "Registry step {index} failed: {}",
        rows["rows"][index]["error"]
    );
}
fn node<'a>(rows: &'a Value, index: usize, id: &str) -> Value {
    serde_json::from_str(
        state(rows, index)["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|v| v["id"] == id)
            .unwrap()["status_json"]
            .as_str()
            .unwrap(),
    )
    .unwrap()
}

fn push(steps: &mut Vec<Value>, step: Value) -> usize {
    let index = steps.len();
    steps.push(step);
    index
}
fn full_catalog() -> Value {
    let mut result = catalog("fixture");
    result["namespaces"][0]["discoveryGroups"] =
        json!([{"group":"fixture/read","description":"Read synthetic data."}]);
    result["namespaces"][0]["tools"][0]["discovery"] =
        json!({"group":"fixture/read","mcp":{"name":"sample_read"}});
    result["namespaces"][0]["notifications"] = json!([{"name":"changed","version":"1.0.0","description":"Synthetic transient change.","payloadSchema":{"type":"object"}}]);
    result["namespaces"][0]["topics"] = json!([{"topic":"fixture.changed","version":"1.0.0","title":"Changes","description":"Synthetic changes.","payloadSchema":{"type":"object"},"eventKinds":[{"kind":"changed","title":"Changed","description":"Synthetic change."}]}]);
    result["namespaces"][0]["inventoryKinds"] = json!([{"kind":"item","version":"1.0.0","searchPointers":["/name"],"archivedPointer":"/archived","recencyPointer":"/recency","summarySchema":{"type":"object","properties":{"name":{"type":["string","null"]},"archived":{"type":"boolean"},"recency":{"type":"integer"}},"additionalProperties":false}}]);
    result
}
fn host_report(sequence: u64, enabled: bool) -> Value {
    json!({"sequence":sequence,"status":{"schemaVersion":1,"hostId":"own-host","observedAt":"1970-01-01T00:01:40.000Z","executor":null,"unfinished":[],"instances":[{"instanceId":"worker","serviceNodeId":"worker","componentId":"sample","engine":"process","desiredEnabled":enabled,"installedBuild":null,"installedCandidateId":null,"observedBuild":null,"observedState":"stopped","observedAt":"1970-01-01T00:01:40.000Z","code":null,"message":""}]}})
}
fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}
fn seed_diagnostics(values: impl IntoIterator<Item = (String, Value)>) -> Value {
    let mut text = String::from("BEGIN IMMEDIATE;");
    for (id, value) in values {
        text.push_str(&format!(
            "INSERT INTO diagnostics VALUES({},{});",
            quote(&id),
            quote(&value.to_string())
        ));
    }
    text.push_str("COMMIT;");
    let mut result = sql(text);
    result["compactDiagnostics"] = json!(true);
    result
}

#[test]
fn registry_lifecycle_identity_generations_and_contact_mirrors_match_original() {
    let mut steps = setup();
    let first = push(&mut steps, connect("worker", "sample", "own-host", 7));
    push(&mut steps, action("clearHints", None, json!({})));
    let premature = push(&mut steps, at(heartbeat("worker", true, vec![]), 105000));
    let after = push(
        &mut steps,
        at(action("node", Some("worker"), json!({})), 115001),
    );
    let missing = push(&mut steps, action("node", Some("absent"), json!({})));
    let mut no_generation = connect("absent", "sample", "own-host", 1);
    no_generation.as_object_mut().unwrap().remove("generation");
    let invalid_generation = push(&mut steps, no_generation);
    let mut fractional = connect("absent", "sample", "own-host", 1);
    fractional["generation"] = json!(1.5);
    let fraction = push(&mut steps, fractional);
    let host_conflict = push(&mut steps, connect("worker", "sample", "other-host", 8));
    let service_conflict = push(&mut steps, connect("worker", "other", "own-host", 8));
    let mut principal = connect("worker", "sample", "own-host", 8);
    principal["context"]["credentialDigest"] = json!(digest(false));
    let principal_conflict = push(&mut steps, principal);
    let reserved = push(&mut steps, connect("hive", "hive", "own-host", 1));
    push(
        &mut steps,
        sql(
            "UPDATE service_nodes SET generation=937,status_json=json_set(status_json,'$.lastSuccessfulSyncAt','prior-success','$.lastFailedSyncAt','prior-failure','$.lastObservationAt','prior-observed','$.desiredEnabled',json('false')) WHERE id='worker';",
        ),
    );
    let reconnect = push(
        &mut steps,
        at(connect("worker", "sample", "own-host", 19), 120000),
    );
    let mut singleton = connect("singleton", "sample", "own-host", 2);
    singleton["params"]["instanceMode"] = json!("singleton");
    let singleton_conflict = push(&mut steps, singleton.clone());
    push(
        &mut steps,
        at(action("disconnect", Some("worker"), json!({})), 121000),
    );
    let singleton_ok = push(&mut steps, singleton);
    let multiple_conflict = push(&mut steps, connect("third", "sample", "own-host", 20));
    push(
        &mut steps,
        action("disconnect", Some("singleton"), json!({})),
    );
    let reopened = push(&mut steps, json!({"op":"reopen"}));
    let disconnected = push(&mut steps, action("node", Some("worker"), json!({})));
    let rows = compare(&steps);
    success(&rows, first);
    assert_eq!(value(&rows, first)["generation"], 7);
    for index in [premature] {
        assert_eq!(code(&rows, index), "service_not_ready");
    }
    for index in [invalid_generation, fraction] {
        assert_eq!(code(&rows, index), "stale_generation");
    }
    for index in [
        host_conflict,
        service_conflict,
        principal_conflict,
        singleton_conflict,
        multiple_conflict,
    ] {
        assert_eq!(code(&rows, index), "target_conflict");
    }
    assert_eq!(code(&rows, missing), "not_found");
    assert_eq!(code(&rows, reserved), "invalid_arguments");
    assert_eq!(
        value(&rows, after)["lastContactAt"],
        "1970-01-01T00:01:45.000Z"
    );
    assert_eq!(value(&rows, after)["stale"], true);
    assert_eq!(node(&rows, reconnect, "worker")["desiredEnabled"], false);
    assert_eq!(
        node(&rows, reconnect, "worker")["lastSuccessfulSyncAt"],
        "prior-success"
    );
    assert_eq!(state(&rows, reconnect)["nodes"][0]["generation"], 937);
    assert_eq!(
        rows["rows"][reconnect]["registry"]["changedGenerations"],
        json!([["worker".encode_utf16().collect::<Vec<_>>(), 19]])
    );
    success(&rows, singleton_ok);
    assert_eq!(rows["rows"][reopened]["registry"], Value::Null);
    assert_eq!(value(&rows, disconnected)["connected"], false);
    assert_eq!(value(&rows, disconnected)["stale"], false);
}

#[test]
fn registry_catalog_binding_hashes_notifications_and_rejection_cache_match_original() {
    let mut steps = setup();
    push(&mut steps, connect("worker", "sample", "own-host", 4));
    let catalog = full_catalog();
    let synced = push(&mut steps, action("sync", Some("worker"), catalog.clone()));
    push(&mut steps, heartbeat("worker", true, vec![]));
    let bindings = push(
        &mut steps,
        action(
            "bindings",
            Some("worker"),
            json!({"namespace":catalog["namespaces"][0]}),
        ),
    );
    let live = push(&mut steps, action("liveCatalog", Some("worker"), json!({})));
    let mut changed = catalog.clone();
    changed["namespaces"][0]["tools"][0]["discovery"]["mcp"]["name"] = json!("sample_new");
    let changed_sync = push(
        &mut steps,
        at(action("sync", Some("worker"), changed.clone()), 101000),
    );
    let changed_binding = push(
        &mut steps,
        action(
            "bindings",
            Some("worker"),
            json!({"namespace":changed["namespaces"][0]}),
        ),
    );
    let mut rejected = changed.clone();
    rejected["namespaces"][0]["tools"][0]["nativeMethod"] = json!("turn/readCase");
    let failure = push(
        &mut steps,
        at(action("sync", Some("worker"), rejected), 102000),
    );
    let cached = push(
        &mut steps,
        action(
            "bindings",
            Some("worker"),
            json!({"namespace":changed["namespaces"][0]}),
        ),
    );
    let retained = push(&mut steps, action("getRegistry", Some("worker"), json!({})));
    let before_restart = push(&mut steps, action("liveCatalog", Some("worker"), json!({})));
    push(&mut steps, json!({"op":"reopen"}));
    let after_restart = push(&mut steps, action("liveCatalog", Some("worker"), json!({})));
    let rows = compare(&steps);
    success(&rows, synced);
    success(&rows, changed_sync);
    assert_eq!(code(&rows, failure), "registry_invalid");
    assert_eq!(
        value(&rows, bindings)[0]["definitionHash"],
        value(&rows, changed_binding)[0]["definitionHash"]
    );
    assert_ne!(
        value(&rows, bindings)[0]["definition"],
        value(&rows, changed_binding)[0]["definition"]
    );
    assert_eq!(value(&rows, changed_binding), value(&rows, cached));
    assert_eq!(value(&rows, retained), &changed);
    assert_eq!(node(&rows, failure, "worker")["synced"], false);
    assert_eq!(node(&rows, failure, "worker")["ready"], false);
    assert_eq!(
        value(&rows, live)["notifications"][0]["namespace"],
        "fixture"
    );
    assert_eq!(value(&rows, before_restart), value(&rows, after_restart));
    assert_eq!(
        state(&rows, synced)["inventorySchemas"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(
        rows["topics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["topic"] == "fixture.changed")
    );
}

#[test]
fn registry_semantic_admission_and_schema_compilation_order_match_original() {
    let mut cases: Vec<(Value, &str)> = vec![];
    let base = full_catalog();
    let mut add = |pointer: &str, next: Value, code: &'static str| {
        let mut value = base.clone();
        *value.pointer_mut(pointer).unwrap() = next;
        cases.push((value, code));
    };
    add(
        "/namespaces",
        json!([base["namespaces"][0], base["namespaces"][0]]),
        "registry_invalid",
    );
    add("/namespaces/0/namespace", json!("hive"), "registry_invalid");
    add(
        "/namespaces/0/discoveryGroups",
        json!([{"group":"other","description":"Other."}]),
        "registry_invalid",
    );
    add(
        "/namespaces/0/tools",
        json!([
            base["namespaces"][0]["tools"][0],
            base["namespaces"][0]["tools"][0]
        ]),
        "registry_invalid",
    );
    let mut second = base["namespaces"][0]["tools"][0].clone();
    second["name"] = json!("turn/Other");
    second["nativeMethod"] = json!("turn/Other");
    second["interfaceVersion"] = json!("2.0.0");
    add(
        "/namespaces/0/tools",
        json!([base["namespaces"][0]["tools"][0], second]),
        "registry_invalid",
    );
    add(
        "/namespaces/0/tools/0/namespace",
        json!("other"),
        "registry_invalid",
    );
    add(
        "/namespaces/0/tools/0/nativeMethod",
        json!("turn/readCase"),
        "registry_invalid",
    );
    add(
        "/namespaces/0/tools/0/discovery/group",
        json!("fixture/missing"),
        "registry_invalid",
    );
    add(
        "/namespaces/0/tools/0/discovery/mcp/name",
        json!("hive_reserved"),
        "registry_invalid",
    );
    add(
        "/namespaces/0/notifications",
        json!([
            base["namespaces"][0]["notifications"][0],
            base["namespaces"][0]["notifications"][0]
        ]),
        "registry_invalid",
    );
    add(
        "/namespaces/0/topics",
        json!([
            base["namespaces"][0]["topics"][0],
            base["namespaces"][0]["topics"][0]
        ]),
        "registry_invalid",
    );
    add(
        "/namespaces/0/topics/0/topic",
        json!("other.changed"),
        "registry_invalid",
    );
    add(
        "/namespaces/0/topics/0/eventKinds",
        json!([
            base["namespaces"][0]["topics"][0]["eventKinds"][0],
            base["namespaces"][0]["topics"][0]["eventKinds"][0]
        ]),
        "registry_invalid",
    );
    add(
        "/namespaces/0/inventoryKinds",
        json!([
            base["namespaces"][0]["inventoryKinds"][0],
            base["namespaces"][0]["inventoryKinds"][0]
        ]),
        "registry_invalid",
    );
    add(
        "/namespaces/0/inventoryKinds/0/searchPointers",
        json!(["/archived"]),
        "registry_invalid",
    );
    add(
        "/namespaces/0/inventoryKinds/0/archivedPointer",
        json!("/name"),
        "registry_invalid",
    );
    add(
        "/namespaces/0/inventoryKinds/0/recencyPointer",
        json!("/archived"),
        "registry_invalid",
    );
    // These graph-admissible schemas are deliberately not provider validators.
    let mut admitted = base.clone();
    admitted["namespaces"][0]["tools"][0]["inputSchema"] = json!({"type":"not-a-json-schema-type"});
    admitted["namespaces"][0]["notifications"][0]["payloadSchema"] =
        json!({"type":"not-a-json-schema-type"});
    let mut steps = setup();
    push(&mut steps, connect("worker", "sample", "own-host", 1));
    let mut failures = vec![];
    for (value, code) in cases {
        let index = push(&mut steps, action("sync", Some("worker"), value));
        failures.push((index, code));
    }
    let admitted_at = push(&mut steps, action("sync", Some("worker"), admitted));
    let mut immutable = base.clone();
    immutable["namespaces"][0]["inventoryKinds"][0]["summarySchema"]["description"] =
        json!("Changed immutable content.");
    let immutable_at = push(&mut steps, action("sync", Some("worker"), immutable));
    let mut bad_inventory = base.clone();
    bad_inventory["namespaces"][0]["inventoryKinds"][0]["summarySchema"] =
        json!({"type":"not-a-json-schema-type"});
    let inventory_at = push(&mut steps, action("sync", Some("worker"), bad_inventory));
    let mut bad_topic = base.clone();
    bad_topic["namespaces"][0]["topics"][0]["payloadSchema"] =
        json!({"type":"not-a-json-schema-type"});
    let topic_at = push(&mut steps, action("sync", Some("worker"), bad_topic));
    let mut prefix = base.clone();
    prefix["mcpPrefix"] = json!("wiki");
    let prefix_at = push(&mut steps, action("sync", Some("worker"), prefix));
    let schema_at = push(
        &mut steps,
        action(
            "sync",
            Some("worker"),
            json!({"namespaces":[],"contracts":[]}),
        ),
    );
    let rows = compare(&steps);
    for (index, expected) in failures {
        assert_eq!(code(&rows, index), expected);
        assert!(
            state(&rows, index)["registries"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }
    success(&rows, admitted_at);
    assert_eq!(code(&rows, immutable_at), "registry_invalid");
    assert_eq!(code(&rows, prefix_at), "registry_invalid");
    for index in [inventory_at, topic_at, schema_at] {
        assert!(!code(&rows, index).is_null());
    }
}

#[test]
fn registry_mcp_name_reservations_and_provider_selection_match_original() {
    let mut steps = setup();
    push(&mut steps, connect("a", "sample", "host-a", 1));
    let mut first = catalog("fixture");
    first["namespaces"][0]["tools"][0]["discovery"] = json!({"mcp":{"name":"sample_read"}});
    let mut duplicate = first["namespaces"][0]["tools"][0].clone();
    duplicate["name"] = json!("turn/Other");
    duplicate["nativeMethod"] = json!("turn/Other");
    first["namespaces"][0]["tools"]
        .as_array_mut()
        .unwrap()
        .push(duplicate);
    let same_namespace = push(&mut steps, action("sync", Some("a"), first.clone()));
    push(&mut steps, heartbeat("a", true, vec![]));
    let stale = push(
        &mut steps,
        at(
            action("select", None, json!({"namespace":"fixture","target":{}})),
            200000,
        ),
    );
    push(&mut steps, connect("b", "other", "host-b", 2));
    let mut collision = catalog("fixture");
    collision["mcpPrefix"] = json!("sample");
    collision["namespaces"][0]["tools"][0]["discovery"] = json!({"mcp":{"name":"sample_read"}});
    push(&mut steps, action("disconnect", Some("a"), json!({})));
    let collision_at = push(&mut steps, action("sync", Some("b"), collision));
    let explicit = push(
        &mut steps,
        action(
            "select",
            None,
            json!({"namespace":"fixture","target":{"serviceNodeId":"a"}}),
        ),
    );
    let unavailable = push(
        &mut steps,
        action("select", None, json!({"namespace":"fixture","target":{}})),
    );
    push(&mut steps, connect("a", "sample", "host-a", 3));
    push(&mut steps, action("sync", Some("a"), first));
    push(&mut steps, heartbeat("a", true, vec![]));
    push(&mut steps, action("sync", Some("b"), catalog("fixture")));
    push(&mut steps, heartbeat("b", true, vec![]));
    let ambiguous = push(
        &mut steps,
        action("select", None, json!({"namespace":"fixture","target":{}})),
    );
    let selected = push(
        &mut steps,
        action(
            "snapshot",
            None,
            json!({"namespace":"fixture","target":{"hostId":"host-b"}}),
        ),
    );
    let owner = push(
        &mut steps,
        action(
            "select",
            None,
            json!({"namespace":"fixture","target":{"serviceNodeId":"a","hostId":"host-b"}}),
        ),
    );
    let resource = push(
        &mut steps,
        action(
            "select",
            None,
            json!({"namespace":"fixture","target":{"serviceNodeId":"a","resourceRef":{"serviceNodeId":"b","namespace":"other"}}}),
        ),
    );
    let namespace = push(
        &mut steps,
        action(
            "select",
            None,
            json!({"namespace":"fixture","target":{"resourceRef":{"serviceNodeId":"a","namespace":"other"}}}),
        ),
    );
    let absent = push(
        &mut steps,
        action("namespace", Some("a"), json!({"namespace":"missing"})),
    );
    let rows = compare(&steps);
    success(&rows, same_namespace);
    success(&rows, stale);
    assert_eq!(value(&rows, stale)["stale"], true);
    assert_eq!(code(&rows, collision_at), "registry_invalid");
    success(&rows, explicit);
    assert_eq!(value(&rows, explicit)["connected"], false);
    assert_eq!(code(&rows, unavailable), "service_unavailable");
    assert_eq!(code(&rows, ambiguous), "ambiguous_service_node");
    assert_eq!(
        rows["rows"][ambiguous]["error"]["data"]["details"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    success(&rows, selected);
    for index in [owner, resource, namespace] {
        assert_eq!(code(&rows, index), "target_conflict");
    }
    assert_eq!(code(&rows, absent), "not_found");
}

#[test]
fn registry_heartbeat_diagnostics_utc_and_notification_thresholds_match_original() {
    let first = "1970-01-01T00:01:40Z";
    let later = "1970-01-01T00:01:41.000Z";
    let mut steps = setup();
    push(&mut steps, connect("worker", "sample", "own-host", 1));
    push(
        &mut steps,
        action("sync", Some("worker"), catalog("fixture")),
    );
    let mut alert = diagnostic("problem", "worker", "current", first, first);
    alert["source"] = json!("spoofed");
    let reported = push(&mut steps, heartbeat("worker", true, vec![alert.clone()]));
    push(&mut steps, action("clearHints", None, json!({})));
    alert["firstObservedAt"] = json!(later);
    alert["lastObservedAt"] = json!(later);
    let timestamp = push(
        &mut steps,
        at(heartbeat("worker", true, vec![alert.clone()]), 101000),
    );
    let projected = push(
        &mut steps,
        at(action("node", Some("worker"), json!({})), 111000),
    );
    let stale = push(
        &mut steps,
        at(action("node", Some("worker"), json!({})), 111001),
    );
    let duplicate = push(
        &mut steps,
        heartbeat("worker", true, vec![alert.clone(), alert.clone()]),
    );
    let resolved = push(&mut steps, at(heartbeat("worker", true, vec![]), 102000));
    let diagnostics = push(
        &mut steps,
        action("diagnostics", None, json!({"status":"resolved"})),
    );
    let current = push(
        &mut steps,
        action("diagnostics", None, json!({"status":"unresolved"})),
    );
    let mut dates = vec![];
    for date in [
        "2024-02-30T00:00:00Z",
        "2024-02-29T00:00:00+00:00",
        "2024-02-29t00:00:00z",
        "2024-02-29T00:00:00.00Z",
    ] {
        let mut value = diagnostic("bad-date", "worker", "current", first, first);
        value["lastObservedAt"] = json!(date);
        dates.push(push(&mut steps, action("diagnostic", None, value)));
    }
    push(
        &mut steps,
        sql(
            "UPDATE service_nodes SET status_json=json_set(status_json,'$.lastContactAt','invalid-date') WHERE id='worker';",
        ),
    );
    push(&mut steps, json!({"op":"reopen"}));
    let invalid_date = push(&mut steps, action("node", Some("worker"), json!({})));
    let rows = compare(&steps);
    success(&rows, reported);
    success(&rows, timestamp);
    assert!(
        state(&rows, timestamp)["changes"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        rows["rows"][timestamp]["registry"]["changedNodes"],
        json!([])
    );
    assert_eq!(value(&rows, projected)["stale"], false);
    assert_eq!(value(&rows, stale)["stale"], true);
    assert_eq!(value(&rows, projected)["diagnostic"]["code"], "problem");
    assert_eq!(code(&rows, duplicate), "invalid_arguments");
    success(&rows, resolved);
    assert_eq!(value(&rows, current), &json!([]));
    let alert = value(&rows, diagnostics)
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["code"] == "problem")
        .unwrap();
    assert_eq!(alert["firstObservedAt"], first);
    assert_eq!(alert["source"], "service:worker");
    assert_eq!(alert["status"], "resolved");
    for index in dates {
        assert_eq!(code(&rows, index), "invalid_arguments");
    }
    assert_eq!(value(&rows, invalid_date)["stale"], false);
}

#[test]
fn registry_diagnostic_capacity_age_order_and_retention_callback_match_original() {
    let at = "1970-01-12T13:46:40.000Z";
    let mut steps = setup();
    push(
        &mut steps,
        seed_diagnostics((0..4096).map(|i| {
            (
                format!("active-{i:04}"),
                diagnostic(&format!("active-{i}"), "worker", "current", at, at),
            )
        })),
    );
    let mut overflow = at_step(
        action(
            "diagnostic",
            None,
            diagnostic("overflow", "worker", "current", at, at),
        ),
        1_000_000_000,
    );
    overflow["compactDiagnostics"] = json!(true);
    let full = push(&mut steps, overflow);
    push(&mut steps, sql("DELETE FROM diagnostics;"));
    let mut resolved: Vec<_> = (0..10002)
        .map(|i| {
            (
                format!("resolved-{i:05}"),
                diagnostic(&format!("resolved-{i}"), "worker", "resolved", at, at),
            )
        })
        .collect();
    resolved.push((
        "age-boundary".into(),
        diagnostic(
            "age",
            "worker",
            "resolved",
            "1970-01-05T13:46:40.000Z",
            "1970-01-05T13:46:40.000Z",
        ),
    ));
    push(&mut steps, seed_diagnostics(resolved));
    let mut collect = at_step(action("collectDiagnostics", None, json!({})), 1_000_000_000);
    collect["compactDiagnostics"] = json!(true);
    let collected = push(&mut steps, collect);
    let mut result = sql(
        "DELETE FROM diagnostics WHERE identity NOT IN ('resolved-00000','resolved-00001','resolved-00002','age-boundary');",
    );
    result["compactDiagnostics"] = json!(true);
    push(&mut steps, result);
    let survivors = push(&mut steps, action("diagnostics", None, json!({})));
    push(&mut steps, sql("DELETE FROM diagnostics;"));
    let mut protocol = diagnostic("invalid_frame", "worker", "current", at, at);
    protocol["source"] = json!("hive");
    let mut stale = protocol.clone();
    stale["code"] = json!("stale_generation");
    let service = diagnostic("invalid_frame", "worker", "current", at, at);
    push(&mut steps,seed_diagnostics([protocol,stale,service].into_iter().map(|value|{
        let identity=ivy_contracts::hash::hash_json(&IvyJson::from_serde(&json!({"code":value["code"],"resource":value["resource"],"source":value["source"]})).unwrap()).unwrap();(identity,value)
    })));
    let initialized = push(
        &mut steps,
        at_step(action("constructor", None, json!({})), 1_000_000_000),
    );
    let retained = push(
        &mut steps,
        at_step(action("diagnostics", None, json!({})), 1_000_000_000),
    );
    let final_collect = push(
        &mut steps,
        at_step(action("collectRetention", None, json!({})), 1_700_000_000),
    );
    let rows = compare(&steps);
    assert_eq!(code(&rows, full), "limit_exceeded");
    success(&rows, collected);
    assert_eq!(value(&rows, survivors).as_array().unwrap().len(), 1);
    assert_eq!(value(&rows, survivors)[0]["code"], "resolved-2");
    success(&rows, initialized);
    let values = value(&rows, retained).as_array().unwrap();
    assert_eq!(
        values.iter().filter(|v| v["status"] == "current").count(),
        1
    );
    success(&rows, final_collect);
    assert_eq!(
        state(&rows, final_collect)["diagnostics"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}
fn at_step(value: Value, now: u64) -> Value {
    at(value, now)
}

#[test]
fn registry_host_report_sequences_management_ownership_and_staleness_match_original() {
    let mut steps = setup();
    push(
        &mut steps,
        connect("manager", "service-manager", "own-host", 1),
    );
    let report = host_report(1, false);
    let initial = push(
        &mut steps,
        action("reportHost", Some("manager"), report.clone()),
    );
    let replay = push(
        &mut steps,
        at(
            action("reportHost", Some("manager"), report.clone()),
            101000,
        ),
    );
    let mut changed = report.clone();
    changed["status"]["instances"][0]["desiredEnabled"] = json!(true);
    let conflict = push(&mut steps, action("reportHost", Some("manager"), changed));
    let older = push(
        &mut steps,
        action("reportHost", Some("manager"), host_report(0, false)),
    );
    let mut duplicate = host_report(2, false);
    let item = duplicate["status"]["instances"][0].clone();
    duplicate["status"]["instances"]
        .as_array_mut()
        .unwrap()
        .push(item);
    let duplicate_at = push(&mut steps, action("reportHost", Some("manager"), duplicate));
    let mut future = host_report(2, false);
    future["status"]["observedAt"] = json!("1970-01-01T00:01:41.001Z");
    let future_at = push(&mut steps, action("reportHost", Some("manager"), future));
    let mut other = host_report(2, false);
    other["status"]["hostId"] = json!("other-host");
    let other_at = push(&mut steps, action("reportHost", Some("manager"), other));
    let wrong_host = push(&mut steps, connect("worker", "sample", "other-host", 2));
    let worker = push(&mut steps, connect("worker", "sample", "own-host", 2));
    let wrong_reporter = push(
        &mut steps,
        action("reportHost", Some("worker"), host_report(2, true)),
    );
    push(&mut steps, action("clearHints", None, json!({})));
    let same = push(
        &mut steps,
        action("reportHost", Some("manager"), host_report(2, false)),
    );
    let enabled = push(
        &mut steps,
        action("reportHost", Some("manager"), host_report(3, true)),
    );
    push(
        &mut steps,
        action("sync", Some("manager"), catalog("manager")),
    );
    push(&mut steps, heartbeat("manager", true, vec![]));
    let fresh = push(
        &mut steps,
        action("hostObservations", None, json!({"hostId":"own-host"})),
    );
    let stale = push(
        &mut steps,
        at(action("hostObservations", None, json!({})), 110001),
    );
    let missing = push(
        &mut steps,
        action("hostObservations", None, json!({"hostId":"missing"})),
    );
    let rows = compare(&steps);
    success(&rows, initial);
    assert_eq!(value(&rows, initial), value(&rows, replay));
    assert_eq!(code(&rows, conflict), "mutation_conflict");
    assert_eq!(code(&rows, older), "revision_conflict");
    for index in [duplicate_at, future_at] {
        assert_eq!(code(&rows, index), "invalid_arguments");
    }
    for index in [other_at, wrong_host, wrong_reporter] {
        assert_eq!(code(&rows, index), "target_conflict");
    }
    success(&rows, worker);
    assert_eq!(node(&rows, worker, "worker")["desiredEnabled"], false);
    assert_eq!(state(&rows, same)["changes"], json!(["system/hosts"]));
    assert_eq!(
        rows["rows"][same]["registry"]["changedNodes"],
        json!(["worker".encode_utf16().collect::<Vec<_>>()])
    );
    assert_eq!(node(&rows, enabled, "worker")["desiredEnabled"], true);
    assert_eq!(value(&rows, fresh)[0]["stale"], false);
    assert_eq!(value(&rows, stale)[0]["stale"], true);
    assert_eq!(value(&rows, missing), &json!([]));
}

#[test]
fn registry_contract_requirements_reuse_retained_read_domains_and_exact_writes() {
    let mut steps = setup();
    push(&mut steps, sql(super::query::seed(false)));
    let requirements =
        |item: Value| action("checkRequirements", None, json!({"requirements":[item]}));
    let absent = push(&mut steps, requirements(json!({"key":"missing"})));
    let skipped = push(
        &mut steps,
        action(
            "checkRequirements",
            None,
            json!({"requirements":[{"key":"missing","readScope":{"invalid":true}}],"allowUnregistered":true}),
        ),
    );
    let conflict = push(
        &mut steps,
        requirements(
            json!({"key":"fixture/value","writeVersions":["1.0.0"],"readVersions":["1.0.0"]}),
        ),
    );
    let readable = push(
        &mut steps,
        requirements(
            json!({"key":"fixture/value","writeVersions":["1.0.0"],"readVersions":["1.0.0","2.0.0"]}),
        ),
    );
    let write_first = push(
        &mut steps,
        requirements(
            json!({"key":"fixture/value","writeVersions":["missing"],"readVersions":[],"readScope":{"invalid":true}}),
        ),
    );
    let invalid_scope = push(
        &mut steps,
        requirements(
            json!({"key":"fixture/value","writeVersions":[],"readVersions":["1.0.0","2.0.0"],"readScope":{"invalid":true}}),
        ),
    );
    push(&mut steps, connect("worker", "sample", "own-host", 1));
    let mut catalog = catalog("fixture");
    catalog["contracts"] = json!([{"key":"sample/value","version":"1.0.0","owner":{"kind":"service","serviceName":"sample"},"specMarkdown":"Synthetic value.","mediaType":"application/json","jsonSchema":{"type":"object"},"retention":{"objects":{"mode":"retain"},"revisions":{"mode":"all"}}}]);
    catalog["requiredContracts"] =
        json!([{"key":"sample/value","writeVersions":["1.0.0"],"readVersions":["1.0.0"]}]);
    let registered = push(&mut steps, action("sync", Some("worker"), catalog));
    let rows = compare(&steps);
    assert_eq!(code(&rows, absent), "contract_not_found");
    success(&rows, skipped);
    assert_eq!(code(&rows, conflict), "contract_version_conflict");
    success(&rows, readable);
    assert_eq!(code(&rows, write_first), "contract_not_found");
    assert!(!code(&rows, invalid_scope).is_null());
    success(&rows, registered);
}

#[test]
fn registry_sql_and_deferred_commit_faults_preserve_the_original_memory_boundaries() {
    let mut steps = setup();
    push(&mut steps, action("constructor", None, json!({})));
    let mut failed_connects = vec![];
    for trigger in [
        "CREATE TRIGGER fixture_failure BEFORE INSERT ON service_connections BEGIN SELECT RAISE(ABORT,'fixture'); END;",
        "CREATE TABLE fixture_deferred(value TEXT REFERENCES metadata(key) DEFERRABLE INITIALLY DEFERRED) STRICT; CREATE TRIGGER fixture_failure AFTER INSERT ON service_connections BEGIN INSERT INTO fixture_deferred VALUES('missing'); END;",
    ] {
        push(&mut steps, action("clearHints", None, json!({})));
        push(&mut steps, sql(trigger));
        failed_connects.push(push(&mut steps, connect("worker", "sample", "own-host", 7)));
        push(&mut steps, sql("DROP TRIGGER fixture_failure;"));
    }
    push(&mut steps, connect("worker", "sample", "own-host", 8));
    let catalog = full_catalog();
    push(&mut steps, action("sync", Some("worker"), catalog.clone()));
    let mut sync_failures = vec![];
    for trigger in [
        "CREATE TRIGGER fixture_failure BEFORE INSERT ON registries BEGIN SELECT RAISE(ABORT,'fixture'); END;",
        "CREATE TRIGGER fixture_failure AFTER UPDATE ON registries BEGIN INSERT INTO fixture_deferred VALUES('missing'); END;",
    ] {
        push(&mut steps, sql(trigger));
        let mut changed = catalog.clone();
        changed["namespaces"][0]["guideMarkdown"] = json!("Rejected candidate.");
        sync_failures.push(push(&mut steps, action("sync", Some("worker"), changed)));
        push(&mut steps, sql("DROP TRIGGER fixture_failure;"));
    }
    let mut diagnostic_failures = vec![];
    for trigger in [
        "CREATE TRIGGER fixture_failure BEFORE INSERT ON diagnostics BEGIN SELECT RAISE(ABORT,'fixture'); END;",
        "CREATE TRIGGER fixture_failure AFTER INSERT ON diagnostics BEGIN INSERT INTO fixture_deferred VALUES('missing'); END;",
    ] {
        push(&mut steps, action("clearHints", None, json!({})));
        push(&mut steps, sql(trigger));
        diagnostic_failures.push(push(
            &mut steps,
            action(
                "diagnostic",
                None,
                diagnostic(
                    "failure",
                    "worker",
                    "current",
                    "1970-01-01T00:01:40Z",
                    "1970-01-01T00:01:40Z",
                ),
            ),
        ));
        push(&mut steps, sql("DROP TRIGGER fixture_failure;"));
    }
    push(
        &mut steps,
        connect("manager", "service-manager", "own-host", 9),
    );
    let mut host_failures = vec![];
    for trigger in [
        "CREATE TRIGGER fixture_failure BEFORE INSERT ON host_reports BEGIN SELECT RAISE(ABORT,'fixture'); END;",
        "CREATE TRIGGER fixture_failure AFTER INSERT ON host_reports BEGIN INSERT INTO fixture_deferred VALUES('missing'); END;",
    ] {
        push(&mut steps, action("clearHints", None, json!({})));
        push(&mut steps, sql(trigger));
        host_failures.push(push(
            &mut steps,
            action("reportHost", Some("manager"), host_report(1, false)),
        ));
        push(&mut steps, sql("DROP TRIGGER fixture_failure;"));
    }
    let rows = compare(&steps);
    for index in failed_connects {
        assert_eq!(code(&rows, index), "internal_error");
        assert!(state(&rows, index)["nodes"].as_array().unwrap().is_empty());
        assert!(
            state(&rows, index)["changes"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            rows["rows"][index]["registry"]["changedGenerations"],
            json!([["worker".encode_utf16().collect::<Vec<_>>(), 7]])
        );
    }
    for index in sync_failures {
        assert_eq!(code(&rows, index), "registry_invalid");
        let retained: Value = serde_json::from_str(
            state(&rows, index)["registries"][0]["registry_json"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(retained, catalog);
        assert_eq!(node(&rows, index, "worker")["synced"], false);
    }
    for index in diagnostic_failures {
        assert_eq!(code(&rows, index), "internal_error");
        assert!(
            state(&rows, index)["changes"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }
    for index in host_failures {
        assert_eq!(code(&rows, index), "internal_error");
        assert!(state(&rows, index)["hosts"].as_array().unwrap().is_empty());
        assert!(
            state(&rows, index)["managed"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        assert!(
            state(&rows, index)["changes"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            rows["rows"][index]["registry"]["changedNodes"],
            json!(["worker".encode_utf16().collect::<Vec<_>>()])
        );
    }
}

#[test]
fn registry_raw_utf16_identity_and_optional_catalog_boundaries() {
    let raw = vec![0xd800];
    let replacement = vec![0xfffd];
    let mut steps = setup();
    let mut first = connect("raw", "sample", "own-host", 1);
    first["parameterUnits"] = json!({"/serviceNodeId":raw});
    first["lossless"] = json!(true);
    let connected = push(&mut steps, first);
    let mut query = action("node", Some("raw"), json!({}));
    query["idUnits"] = json!(raw);
    query["lossless"] = json!(true);
    let found = push(&mut steps, query);
    let mut alias = connect("raw", "sample", "own-host", 2);
    alias["parameterUnits"] = json!({"/serviceNodeId":replacement});
    alias["lossless"] = json!(true);
    let reconnected = push(&mut steps, alias);
    let mut all = action("nodes", None, json!({}));
    all["lossless"] = json!(true);
    let nodes = push(&mut steps, all);
    let rows = compare(&steps);
    success(&rows, connected);
    success(&rows, found);
    success(&rows, reconnected);
    success(&rows, nodes);
    assert!(
        value(&rows, found)["encoded"]
            .as_str()
            .unwrap()
            .contains("\\ud800")
    );
    assert_eq!(
        rows["rows"][reconnected]["registry"]["changedNodes"],
        json!([raw, replacement])
    );
    assert_eq!(
        state(&rows, reconnected)["nodes"].as_array().unwrap().len(),
        1
    );

    let mut steps = setup();
    push(&mut steps, connect("worker", "sample", "own-host", 1));
    let catalog = catalog("fixture");
    push(&mut steps, action("sync", Some("worker"), catalog.clone()));
    let cached = push(
        &mut steps,
        action(
            "bindings",
            Some("worker"),
            json!({"namespace":catalog["namespaces"][0]}),
        ),
    );
    let mut changed = catalog["namespaces"][0].clone();
    changed["tools"][0]["description"] = json!("Changed direct database value.");
    push(
        &mut steps,
        sql(format!(
            "UPDATE registries SET registry_json={} WHERE node_id='worker';",
            quote(
                &json!({"namespaces":[changed.clone()],"contracts":[],"requiredContracts":[]})
                    .to_string()
            )
        )),
    );
    let retained = push(
        &mut steps,
        action(
            "bindings",
            Some("worker"),
            json!({"namespace":changed.clone()}),
        ),
    );
    push(&mut steps, json!({"op":"reopen"}));
    let restarted = push(
        &mut steps,
        action("bindings", Some("worker"), json!({"namespace":changed})),
    );
    push(
        &mut steps,
        sql("UPDATE registries SET registry_json='null' WHERE node_id='worker';"),
    );
    let null = push(
        &mut steps,
        action("namespace", Some("worker"), json!({"namespace":"fixture"})),
    );
    push(
        &mut steps,
        sql(
            "UPDATE registries SET registry_json='{\"namespaces\":[null]}' WHERE node_id='worker';",
        ),
    );
    let corrupt = push(
        &mut steps,
        action("namespace", Some("worker"), json!({"namespace":"fixture"})),
    );
    let rows = compare(&steps);
    assert_eq!(value(&rows, cached), value(&rows, retained));
    assert_ne!(
        value(&rows, cached)[0]["definitionHash"],
        value(&rows, restarted)[0]["definitionHash"]
    );
    assert_eq!(code(&rows, null), "not_found");
    assert_eq!(code(&rows, corrupt), "internal_error");

    let mut steps = setup();
    push(&mut steps, action("constructor", None, json!({})));
    let repaired = diagnostic(
        "legacy-null",
        "worker",
        "current",
        "1970-01-01T00:01:40Z",
        "1970-01-01T00:01:40Z",
    );
    let identity=ivy_contracts::hash::hash_json(&IvyJson::from_serde(&json!({"code":repaired["code"],"resource":repaired["resource"],"source":repaired["source"]})).unwrap()).unwrap();
    let expected = json!([{"identity":identity,"diagnostic_json":"null"}]);
    push(&mut steps, seed_diagnostics([(identity, Value::Null)]));
    let unfiltered = push(&mut steps, action("diagnostics", None, json!({})));
    let filtered = push(
        &mut steps,
        action("diagnostics", None, json!({"status":"unresolved"})),
    );
    let repaired_at = push(&mut steps, action("diagnostic", None, repaired));
    // A null Diagnostic is outside the published schema. Keep native refusal,
    // rather than reproducing JavaScript's property access or healing coercion.
    let root = Root::new();
    let observed = oracle(&root.0.join("reference.sqlite"), &steps);
    let rows = native_with_runtime(
        &root.0.join("native.sqlite"),
        &steps,
        Some(&observed["registryRuntime"]),
    );
    assert_eq!(code(&rows, unfiltered), "internal_error");
    assert_eq!(code(&rows, filtered), "internal_error");
    assert_eq!(code(&rows, repaired_at), "internal_error");
    for index in [unfiltered, filtered, repaired_at] {
        assert_eq!(state(&rows, index)["diagnostics"], expected);
    }
}
