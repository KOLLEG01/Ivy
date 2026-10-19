use super::*;
use ivy_contracts::IvyResult;
use ivy_hive::{
    configurations::Configurations,
    discovery::Discovery,
    kernel::{Call, Kernel, Options, Parts},
    registry::Registry,
};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    sync::Arc,
    time::Duration,
};
#[path = "hive_kernel_domains.rs"]
mod domains;

fn options() -> Options {
    Options {
        version: "1.0.0".into(),
        build_id: format!("sha256:{}", "a".repeat(64)).into(),
        public_base_url: "https://example.invalid/base".into(),
        validate_messages: false,
        reset_bootstrap_credential_digest: None,
    }
}
fn compose(path: &Path, profile: &Value, clock: Rc<Cell<f64>>) -> Kernel {
    let mut store = Store::open(path).unwrap();
    let cursor_secret = "0123456789abcdef".repeat(4);
    if store.metadata("cursor_secret").unwrap().as_deref() != Some(cursor_secret.as_str()) {
        store.set_metadata("cursor_secret", &cursor_secret).unwrap();
        drop(store);
        store = Store::open(path).unwrap();
    }
    let runtime = Rc::new(registry::ObservedRuntime::new(profile));
    store.bind_query_runtime(Arc::new(registry::ObservedRuntime::new(profile)));
    let registry = Rc::new(RefCell::new(
        Registry::new(&mut store, runtime.as_ref(), clock.get()).unwrap(),
    ));
    let discovery = Discovery::new(runtime.as_ref()).unwrap();
    let configurations = Configurations::new(&mut store).unwrap();
    Kernel::compose(
        Parts {
            store,
            registry,
            discovery,
            configurations,
        },
        options(),
        runtime,
        Rc::new(move || clock.get()),
    )
    .unwrap()
}
fn bad_output(call: &mut Call<'_>, _: &IvyJson, _: &IvyJson, _: &str) -> IvyResult<IvyJson> {
    call.store
        .set_metadata("kernel-handler-effect", "written")?;
    Ok(false.into())
}
fn slow(call: &mut Call<'_>, _: &IvyJson, _: &IvyJson, _: &str) -> IvyResult<IvyJson> {
    std::thread::sleep(Duration::from_millis(1100));
    call.store.check_budget()?;
    Ok(IvyJson::Null)
}
fn update_options(kernel: &mut Kernel, step: &Value) {
    if let Some(value) = step["options"].get("validateMessages") {
        kernel.options.validate_messages = value == true;
    }
    if let Some(value) = step["options"].get("resetBootstrapCredentialDigest") {
        kernel.options.reset_bootstrap_credential_digest = value.as_str().map(JsString::from);
    }
}
fn native(path: &Path, steps: &[Value], profile: &Value) -> Value {
    assert_eq!(steps[0]["method"], "constructor");
    let clock = Rc::new(Cell::new(100000.0));
    let mut kernel = Some(compose(path, profile, clock.clone()));
    let mut observations = vec![];
    let inventory_trace = steps.iter().any(|step| {
        step["request"]["method"]
            .as_str()
            .is_some_and(|method| method.starts_with("inventory."))
    });
    for original in steps {
        let mut step = original.clone();
        if step["identityBindings"].is_array() {
            identity::bind(&mut step, &observations);
        }
        clock.set(step["now"].as_f64().unwrap_or(100000.0));
        if step["method"] == "reopen" {
            drop(kernel.take());
            kernel = Some(compose(path, profile, clock.clone()));
        }
        let kernel = kernel.as_mut().unwrap();
        update_options(kernel, &step);
        let result = (|| -> IvyResult<IvyJson> {
            match step["op"].as_str().unwrap() {
                "kernel" => match step["method"].as_str().unwrap() {
                    "constructor" | "reopen" => Ok(IvyJson::Null),
                    "execute" => {
                        let mut request = IvyJson::from_serde(&step["request"]).unwrap();
                        object_write::apply_units(&mut request, &step);
                        kernel.execute(&IvyJson::from_serde(&step["context"]).unwrap(), &request)
                    }
                    "register" | "registerSlow" => kernel
                        .add(
                            step["name"].as_str().unwrap(),
                            if step["method"] == "registerSlow" {
                                slow
                            } else {
                                bad_output
                            },
                        )
                        .map(|_| IvyJson::Null),
                    "binding" => kernel
                        .parts
                        .discovery
                        .core_binding(&step["name"].as_str().unwrap().into()),
                    "node" => kernel.parts.registry.borrow().node(
                        &kernel.parts.store,
                        &step["id"].as_str().unwrap().into(),
                        &registry::ObservedRuntime::new(profile),
                        clock.get(),
                    ),
                    "pageError" => {
                        let params = IvyJson::from_serde(&step["params"]).unwrap();
                        let mut identity = IvyJson::object([("method", "fixture.pager".into())]);
                        for (key, value) in params.as_object().unwrap() {
                            if key != &JsString::from("cursor") {
                                identity.insert(key.clone(), value.clone());
                            }
                        }
                        ivy_hive::paging::try_catalog_page(
                            &kernel.parts.store,
                            &mut [
                                IvyJson::object([("id", "a".into())]),
                                IvyJson::object([("id", "b".into())]),
                            ],
                            &identity,
                            |_| {
                                Err(IvyError::new(
                                    "fixture_failure",
                                    "Catalog key requested failure.",
                                ))
                            },
                            params
                                .get("limit")
                                .and_then(IvyJson::as_f64)
                                .map(|v| v as usize),
                            params.get("cursor").and_then(IvyJson::as_js_string),
                        )
                    }
                    _ => panic!("Unexpected kernel fixture method"),
                },
                "configure" => kernel
                    .parts
                    .store
                    .configure_credentials(
                        &step["credentials"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .map(|v| Credential {
                                principal_id: v["principalId"].as_str().unwrap().into(),
                                digest: v["digest"].as_str().unwrap().into(),
                            })
                            .collect::<Vec<_>>(),
                    )
                    .map(|_| IvyJson::Null),
                "sql" => kernel
                    .parts
                    .store
                    .connection()
                    .execute_batch(step["sql"].as_str().unwrap())
                    .map_err(|e| kernel.parts.store.query_error(e))
                    .map(|_| IvyJson::Null),
                "register" => ivy_hive::objects::register(
                    &mut kernel.parts.store,
                    &IvyJson::from_serde(&step["definition"]).unwrap(),
                    &IvyJson::from_serde(&step["owner"]).unwrap(),
                ),
                _ => panic!("Unexpected kernel fixture operation"),
            }
        })();
        let (value, error) = match result {
            Ok(v) => (wire(v), Value::Null),
            Err(e) => (Value::Null, wire(e.to_wire())),
        };
        let registry = kernel.parts.registry.borrow();
        let mut state = snapshot(&kernel.parts.store, &step);
        state["deploymentReports"] = rows(
            &kernel.parts.store,
            "SELECT * FROM deployment_reports ORDER BY host_id,deployment_id",
        );
        if inventory_trace {
            state["inventorySets"] = rows(
                &kernel.parts.store,
                "SELECT * FROM inventory_sets ORDER BY node_id,namespace,kind",
            );
            state["inventory"] = rows(
                &kernel.parts.store,
                "SELECT * FROM inventory ORDER BY node_id,namespace,kind,native_id",
            );
        }
        observations.push(json!({"value":value,"error":error,"state":state,
            "registry":{"changedNodes":registry.changed_nodes().iter().map(|v|&v.0).collect::<Vec<_>>(),
            "changedGenerations":registry.changed_generations().iter().map(|(id,g)|json!([id.0,wire(IvyJson::Number(*g))])).collect::<Vec<_>>()}}));
    }
    let store = &kernel.as_ref().unwrap().parts.store;
    json!({"rows":observations,"definitions":rows(store,"SELECT type,name,tbl_name,sql FROM sqlite_schema WHERE sql IS NOT NULL ORDER BY type,name"),
        "metadata":rows(store,"SELECT * FROM metadata WHERE key NOT IN ('cursor_secret','runtime_epoch') ORDER BY key"),"topics":rows(store,"SELECT * FROM topics ORDER BY topic,version")})
}
fn compare(steps: &[Value]) -> Value {
    let root = Root::new();
    let observed = oracle(&root.0.join("observed.sqlite"), steps);
    let actual = native(
        &root.0.join("native.sqlite"),
        steps,
        &observed["registryRuntime"],
    );
    let mut bound = steps.to_vec();
    for (i, step) in bound.iter_mut().enumerate() {
        if step["request"]["params"]["create"].is_object()
            || step["request"]["method"] == "hostConfigurations.put"
        {
            let value = &actual["rows"][i]["value"]["value"];
            let id = value["object"]["id"]
                .as_str()
                .or_else(|| value["objectId"].as_str())
                .map(str::to_owned)
                .unwrap_or_else(|| format!("00000000-0000-4000-8000-{i:012}"));
            assert_eq!(uuid::Uuid::parse_str(&id).unwrap().get_version_num(), 4);
            step["createdId"] = id.into();
        }
    }
    let expected = oracle(&root.0.join("reference.sqlite"), &bound);
    assert_eq!(actual["rows"].as_array().unwrap().len(), steps.len());
    for (i, (a, b)) in actual["rows"]
        .as_array()
        .unwrap()
        .iter()
        .zip(expected["rows"].as_array().unwrap())
        .enumerate()
    {
        assert_eq!(a, b, "Kernel step{i}: {}", steps[i]);
        if matches!(
            steps[i]["op"].as_str(),
            Some("configure" | "sql" | "register")
        ) || steps[i]["requireSuccess"] == true
        {
            assert!(a["error"].is_null(), "Fixture step{i}: {}", a["error"]);
        }
    }
    for key in ["definitions", "metadata", "topics"] {
        assert_eq!(actual[key], expected[key]);
    }
    actual
}
fn op(method: &str) -> Value {
    json!({"op":"kernel","method":method,"now":100000})
}
fn sql(text: impl Into<String>) -> Value {
    json!({"op":"sql","sql":text.into()})
}
fn setup() -> Vec<Value> {
    let mut steps = vec![op("constructor")];
    steps.extend(object_write::setup());
    steps
}
fn context(transport: &str) -> Value {
    json!({"principalId":"alpha","credentialDigest":format!("sha256:{}","a".repeat(64)),"transport":transport})
}
fn request(method: &str, params: Value) -> Value {
    let mut step = op("execute");
    step["context"] = context("http");
    step["request"] = json!({"jsonrpc":"2.0","id":"request","method":method,"params":params});
    step
}
fn push(steps: &mut Vec<Value>, step: Value) -> usize {
    let index = steps.len();
    steps.push(step);
    index
}
fn bind(step: &mut Value, pointer: &str, from: usize, select: &str) {
    if !step["identityBindings"].is_array() {
        step["identityBindings"] = json!([]);
    }
    step["identityBindings"]
        .as_array_mut()
        .unwrap()
        .push(json!({"at":pointer,"from":from,"select":select}));
}
fn error(result: &Value, index: usize, code: &str) {
    assert_eq!(result["rows"][index]["error"]["data"]["code"], code);
}
fn success(result: &Value, index: usize) {
    assert!(
        result["rows"][index]["error"].is_null(),
        "Step {index}: {}",
        result["rows"][index]["error"]
    );
}
fn connect(id: &str, generation: Option<f64>) -> Value {
    let mut step = request(
        "service.connect",
        json!({"serviceNodeId":id,"hostId":"host","serviceName":"fixture","version":"1.0.0","buildId":format!("sha256:{}","b".repeat(64)),"hiveProtocol":1}),
    );
    step["context"] = context("ws");
    if let Some(generation) = generation {
        step["context"]["generation"] = generation.into();
    }
    step
}
fn service_request(method: &str, id: &str, params: Value) -> Value {
    let mut step = request(method, params);
    step["context"] = context("ws");
    step["context"]["serviceNodeId"] = id.into();
    step["context"]["generation"] = 1.into();
    step
}

#[test]
fn kernel_envelope_context_protocol_and_operation_guards_match_original_in_order() {
    let mut s = setup();
    let status = push(&mut s, request("system.status", json!({})));
    s[status]["options"] = json!({"validateMessages":true});
    let mut indices = vec![];
    for value in [
        Value::Null,
        json!([]),
        json!({}),
        json!({"jsonrpc":"2.0","id":-1,"method":"system.status","params":{}}),
        json!({"jsonrpc":"2.0","id":"id","method":"system.status","params":{},"extra":true}),
    ] {
        let mut step = request("system.status", json!({}));
        step["request"] = value;
        indices.push(push(&mut s, step));
    }
    let mut stale = request("system.status", json!({}));
    stale["context"]["serviceNodeId"] = "node".into();
    stale["request"] = Value::Null;
    let stale = push(&mut s, stale);
    let unknown = push(&mut s, request("toString", json!({})));
    let mut units = request("placeholder", json!({}));
    units["parameterUnits"] = json!({"/method":[55296]});
    let units = push(&mut s, units);
    let mut unsupported = vec![];
    for protocol in [json!(2), json!("1"), Value::Null] {
        let mut step = request("service.connect", json!({"hiveProtocol":protocol}));
        step["context"] = context("ws");
        unsupported.push(push(&mut s, step));
    }
    let invalid_connect = push(
        &mut s,
        request("service.connect", json!({"hiveProtocol":1})),
    );
    let access = push(
        &mut s,
        request(
            "registry.sync",
            json!({"namespaces":[],"contracts":[],"requiredContracts":[]}),
        ),
    );
    let mut deletion = request(
        "objects.delete",
        json!({"objectId":"missing","expectedRevision":1,"mutationId":"epoch:100000:delete"}),
    );
    deletion["context"] = context("mcp");
    let blocked_delete = push(&mut s, deletion.clone());
    deletion["request"]["params"]
        .as_object_mut()
        .unwrap()
        .remove("mutationId");
    let invalid_delete = push(&mut s, deletion);
    let missing_handler = push(
        &mut s,
        request("packages.catalog", json!({"after":0,"includeUis":false})),
    );
    let routed = push(
        &mut s,
        request(
            "tools.call",
            json!({"qualifiedName":"fixture.tool","arguments":{},"expectedDefinitionHash":format!("sha256:{}","a".repeat(64))}),
        ),
    );
    let result = compare(&s);
    success(&result, status);
    for i in indices {
        error(&result, i, "invalid_frame");
    }
    error(&result, stale, "stale_generation");
    error(&result, unknown, "not_found");
    error(&result, units, "not_found");
    for i in unsupported {
        error(&result, i, "unsupported_protocol");
    }
    error(&result, invalid_connect, "invalid_arguments");
    error(&result, access, "stale_generation");
    error(&result, blocked_delete, "forbidden");
    error(&result, invalid_delete, "invalid_arguments");
    error(&result, missing_handler, "not_found");
    error(&result, routed, "internal_error");
}

#[test]
fn kernel_object_dispatch_preserves_real_receipts_content_rollback_retry_and_reopen() {
    let mut s = setup();
    let mut definition = object_write::setup()[2]["definition"].clone();
    definition["key"] = "fixture/kernel".into();
    let register = push(
        &mut s,
        request(
            "contracts.register",
            json!({"mutationId":"epoch:100000:register","definition":definition}),
        ),
    );
    s[register]["options"] = json!({"validateMessages":true});
    let get = push(
        &mut s,
        request("contracts.get", json!({"key":"fixture/kernel"})),
    );
    let validate = push(
        &mut s,
        request(
            "contracts.validate",
            json!({"key":"fixture/kernel","contractVersion":"1.0.0","content":{"encoding":"json","value":{"n":1}}}),
        ),
    );
    let create = push(
        &mut s,
        request(
            "objects.write",
            json!({"mutationId":"epoch:100000:create","create":{"name":"kernel","contractKey":"fixture/kernel","parentId":null,"ownerObjectId":null,"icon":null},"contractVersion":"1.0.0","references":{},"content":{"encoding":"json","value":{"n":1}}}),
        ),
    );
    let read = push(&mut s, request("objects.read", json!({"path":"/kernel"})));
    let mut write = request(
        "objects.write",
        json!({"mutationId":"epoch:100000:update","objectId":"bound","expectedRevision":1,"contractVersion":"1.0.0","references":{},"content":{"encoding":"json","value":{"n":2}}}),
    );
    bind(
        &mut write,
        "/request/params/objectId",
        create,
        "/value/value/object/id",
    );
    let update = push(&mut s, write.clone());
    let replay = push(&mut s, write.clone());
    write["request"]["params"]["mutationId"] = "epoch:100000:retry".into();
    write["request"]["params"]["expectedRevision"] = 2.into();
    write["request"]["params"]["content"]["value"]["n"] = 3.into();
    s.push(sql("CREATE TRIGGER kernel_revision_failure BEFORE INSERT ON revisions WHEN NEW.revision=3 BEGIN SELECT RAISE(ABORT,'fixture failure'); END;"));
    let failed = push(&mut s, write.clone());
    s.push(sql("DROP TRIGGER kernel_revision_failure;"));
    let retry = push(&mut s, write);
    let mut history = request("objects.history", json!({"objectId":"bound","limit":20}));
    bind(
        &mut history,
        "/request/params/objectId",
        create,
        "/value/value/object/id",
    );
    let history = push(&mut s, history);
    let legacy_output = push(
        &mut s,
        request("objects.list", json!({"parentId":null,"limit":50})),
    );
    let mut list = request("objects.list", json!({"parentId":null,"limit":50}));
    list["options"] = json!({"validateMessages":false});
    let list = push(&mut s, list);
    let mut query = request(
        "objects.query",
        json!({"contractKey":"fixture/kernel","select":["object.name","data:/n"]}),
    );
    query["options"] = json!({"validateMessages":true});
    let query = push(&mut s, query);
    s.push(op("reopen"));
    let reopened = push(&mut s, request("objects.read", json!({"path":"/kernel"})));
    let result = compare(&s);
    for i in [
        register, get, validate, create, read, update, replay, retry, history, list, query,
        reopened,
    ] {
        success(&result, i);
    }
    error(&result, failed, "internal_error");
    error(&result, legacy_output, "internal_error");
    assert_eq!(
        result["rows"][legacy_output]["error"]["data"]["outcome"],
        "completed"
    );
    assert_eq!(
        result["rows"][update]["value"],
        result["rows"][replay]["value"]
    );
    assert_eq!(
        result["rows"][reopened]["value"]["value"]["content"]["value"],
        json!({"n":3})
    );
}

#[test]
fn kernel_invalid_registry_input_marks_readiness_and_diagnostics_before_failure_and_recovers() {
    let mut s = setup();
    s.push(connect("node", Some(1.0)));
    let sync = json!({"namespaces":[],"contracts":[],"requiredContracts":[]});
    let synced = push(
        &mut s,
        service_request("registry.sync", "node", sync.clone()),
    );
    s.push(service_request(
        "service.heartbeat",
        "node",
        json!({"ready":true,"diagnostics":[]}),
    ));
    let failed = push(&mut s, service_request("registry.sync", "node", json!({})));
    let mut node = op("node");
    node["id"] = "node".into();
    let unready = push(&mut s, node.clone());
    let recovered = push(
        &mut s,
        service_request("registry.sync", "node", sync.clone()),
    );
    s.push(service_request(
        "service.heartbeat",
        "node",
        json!({"ready":true,"diagnostics":[]}),
    ));
    s.push(sql("CREATE TRIGGER kernel_diagnostic_failure BEFORE INSERT ON diagnostics BEGIN SELECT RAISE(ABORT,'fixture failure'); END;"));
    let diagnostic_failure = push(&mut s, service_request("registry.sync", "node", json!({})));
    let saved_before_failure = push(&mut s, node.clone());
    s.push(sql("DROP TRIGGER kernel_diagnostic_failure;"));
    let retry = push(&mut s, service_request("registry.sync", "node", json!({})));
    let missing = push(
        &mut s,
        service_request("registry.sync", "missing", json!({})),
    );
    let mut stale = service_request("registry.sync", "node", json!({}));
    stale["context"]["generation"] = 0.into();
    let stale = push(&mut s, stale);
    let result = compare(&s);
    success(&result, synced);
    success(&result, recovered);
    error(&result, failed, "invalid_arguments");
    error(&result, diagnostic_failure, "internal_error");
    error(&result, retry, "invalid_arguments");
    error(&result, missing, "not_found");
    error(&result, stale, "stale_generation");
    for i in [unready, saved_before_failure] {
        assert_eq!(result["rows"][i]["value"]["ready"], false);
        assert_eq!(result["rows"][i]["value"]["synced"], false);
    }
}

#[test]
fn kernel_connection_generation_reset_and_completed_output_failures_keep_original_boundaries() {
    let mut s = setup();
    let mut generations = vec![];
    for (i, g) in [
        None,
        Some(10.0),
        None,
        Some(2.0),
        None,
        Some(9_007_199_254_740_991.0),
        None,
    ]
    .into_iter()
    .enumerate()
    {
        generations.push(push(&mut s, connect(&format!("node{i}"), g)));
    }
    let invalid = push(&mut s, connect("invalid", Some(0.0)));
    let mut held = request("events.head", json!({}));
    held["options"] = json!({"resetBootstrapCredentialDigest":format!("sha256:{}","a".repeat(64))});
    let held = push(&mut s, held);
    let read = push(&mut s, request("objects.read", json!({"path":"/json"})));
    let mut wrong = request("objects.read", json!({"path":"/json"}));
    wrong["context"]["credentialDigest"] = format!("sha256:{}", "b".repeat(64)).into();
    let wrong = push(&mut s, wrong);
    let mut malformed = request("system.status", json!({}));
    malformed["request"] = Value::Null;
    let malformed = push(&mut s, malformed);
    let mut clear = request("system.status", json!({}));
    clear["options"] = json!({"resetBootstrapCredentialDigest":""});
    let clear = push(&mut s, clear);
    let mut bad = op("register");
    bad["name"] = "system.status".into();
    let duplicate = push(&mut s, bad.clone());
    bad["name"] = "unknown.operation".into();
    let unknown = push(&mut s, bad.clone());
    bad["name"] = "packages.catalog".into();
    s.push(bad);
    let mut output = request("packages.catalog", json!({"after":0,"includeUis":false}));
    output["options"] = json!({"validateMessages":true});
    let completed = push(&mut s, output.clone());
    output["options"] = json!({"validateMessages":false});
    let unchecked = push(&mut s, output);
    let mut slow = op("registerSlow");
    slow["name"] = "notifications.subscribe".into();
    s.push(slow);
    let timed = push(
        &mut s,
        request("notifications.subscribe", json!({"filters":[]})),
    );
    let after = push(&mut s, request("system.status", json!({})));
    let result = compare(&s);
    for (i, g) in
        generations
            .into_iter()
            .zip([1.0, 10.0, 11.0, 2.0, 12.0, 9_007_199_254_740_991.0, 1.0])
    {
        success(&result, i);
        assert_eq!(result["rows"][i]["value"]["value"]["generation"], g);
    }
    error(&result, invalid, "stale_generation");
    error(&result, held, "maintenance_active");
    success(&result, read);
    error(&result, wrong, "maintenance_active");
    error(&result, malformed, "invalid_frame");
    success(&result, clear);
    error(&result, duplicate, "internal_error");
    error(&result, unknown, "internal_error");
    error(&result, completed, "internal_error");
    assert_eq!(
        result["rows"][completed]["error"]["data"]["outcome"],
        "completed"
    );
    success(&result, unchecked);
    assert_eq!(result["rows"][unchecked]["value"]["value"], false);
    error(&result, timed, "limit_exceeded");
    success(&result, after);
    assert!(
        result["rows"][completed]["state"]["metadata"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["key"] == "kernel-handler-effect" && row["value"] == "written")
    );
}

#[test]
fn kernel_recursive_discovery_reapplies_caller_input_reset_and_mcp_restrictions() {
    let mut s = setup();
    let mut binding = op("binding");
    binding["name"] = "hive.objects.read".into();
    let read_binding = push(&mut s, binding.clone());
    let mut call = request(
        "discovery.call",
        json!({"serviceName":"hive","qualifiedName":"hive.objects.read","expectedDefinitionHash":"bound","arguments":{"path":"/json"}}),
    );
    bind(
        &mut call,
        "/request/params/expectedDefinitionHash",
        read_binding,
        "/value/definitionHash",
    );
    let good = push(&mut s, call.clone());
    let mut mcp_read = call.clone();
    mcp_read["context"] = context("mcp");
    let mcp_read = push(&mut s, mcp_read);
    let mut changed = call.clone();
    changed["request"]["params"]["expectedCallerPrincipalId"] = "beta".into();
    let changed = push(&mut s, changed);
    let mut arguments = call.clone();
    arguments["request"]["params"]["arguments"] = json!({});
    let arguments = push(&mut s, arguments);
    let mut hash = call.clone();
    hash.as_object_mut().unwrap().remove("identityBindings");
    hash["request"]["params"]["expectedDefinitionHash"] =
        format!("sha256:{}", "0".repeat(64)).into();
    let hash = push(&mut s, hash);
    let mut deletion = request(
        "discovery.call",
        json!({"serviceName":"hive","qualifiedName":"hive.objects.delete","expectedDefinitionHash":format!("sha256:{}","0".repeat(64)),"arguments":{"objectId":"missing","expectedRevision":1,"mutationId":"epoch:100000:delete"}}),
    );
    deletion["context"] = context("mcp");
    let deletion = push(&mut s, deletion);
    let routed = push(
        &mut s,
        request(
            "discovery.call",
            json!({"serviceName":"fixture","serviceNodeId":"node","qualifiedName":"fixture.tool","expectedDefinitionHash":format!("sha256:{}","a".repeat(64)),"arguments":{}}),
        ),
    );
    call["options"] = json!({"resetBootstrapCredentialDigest":format!("sha256:{}","a".repeat(64))});
    let held = push(&mut s, call);
    let result = compare(&s);
    success(&result, good);
    success(&result, mcp_read);
    error(&result, changed, "caller_changed");
    error(&result, arguments, "invalid_arguments");
    error(&result, hash, "tool_definition_changed");
    error(&result, deletion, "not_found");
    error(&result, routed, "internal_error");
    error(&result, held, "maintenance_active");
}

#[test]
fn kernel_browser_configuration_reads_search_and_saves_use_actual_redaction_and_authentication() {
    let mut s = setup();
    s.push(sql(format!(
        "INSERT INTO browser_sessions VALUES('browser','sha256:{}','1970-01-01T00:00:00.000Z');",
        "a".repeat(64)
    )));
    let configuration = json!({"schemaVersion":1,"hostId":"host","runtimeRoot":"C:/fixture/runtime","artifactRoot":"C:/fixture/artifacts","stagingRoot":"C:/fixture/staging","publicBaseUrl":"https://example.invalid/base","executables":{"node":"node"},"instances":[{"instanceId":"worker","serviceNodeId":"worker","componentId":"fixture-worker","enabled":true,"engine":"process","credential":"synthetic-secret","secretPaths":["/settings/token"],"settings":{"token":"synthetic-token","value":1}}]});
    let put = push(
        &mut s,
        request(
            "hostConfigurations.put",
            json!({"hostId":"host","mutationId":"epoch:100000:config","configuration":configuration}),
        ),
    );
    let mut browser = context("http");
    browser["sessionDigest"] = "browser".into();
    let mut read = request("objects.read", json!({"objectId":"bound"}));
    read["context"] = browser.clone();
    bind(
        &mut read,
        "/request/params/objectId",
        put,
        "/value/value/objectId",
    );
    let read = push(&mut s, read);
    let mut get = request("hostConfigurations.get", json!({"hostId":"host"}));
    get["context"] = browser.clone();
    let get = push(&mut s, get);
    let mut raw = s[put].clone();
    raw["context"] = browser.clone();
    raw["request"]["params"]["mutationId"] = "epoch:100000:raw".into();
    let raw = push(&mut s, raw);
    let mut query = request(
        "objects.query",
        json!({"contractKey":"ivy/host-configuration"}),
    );
    query["context"] = browser.clone();
    let query = push(&mut s, query);
    let mut search = request("objects.search", json!({"text":"host","limit":20}));
    search["context"] = browser.clone();
    let search = push(&mut s, search);
    let edit = push(
        &mut s,
        request("hostConfigurations.edit", json!({"hostId":"host"})),
    );
    let mut save = request(
        "hostConfigurations.save",
        json!({"hostId":"host","expectedRevision":1,"mutationId":"epoch:100000:save","configuration":{}}),
    );
    save["context"] = browser;
    bind(
        &mut save,
        "/request/params/configuration",
        edit,
        "/value/value/configuration",
    );
    let save = push(&mut s, save);
    let full = push(
        &mut s,
        request("hostConfigurations.get", json!({"hostId":"host"})),
    );
    let result = compare(&s);
    for i in [put, read, search, edit, save, full] {
        success(&result, i);
    }
    for i in [get, raw, query] {
        error(&result, i, "forbidden");
    }
    assert_eq!(
        result["rows"][read]["value"]["value"]["content"]["value"]["instances"][0]["credential"],
        json!({"$ivySecret":{"instanceId":"worker","path":"/credential"}})
    );
    assert_eq!(
        result["rows"][full]["value"]["value"]["configuration"]["instances"][0]["credential"],
        "synthetic-secret"
    );
    assert!(
        !result["rows"][search]["value"]["value"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["object"]["contractKey"] == "ivy/host-configuration")
    );
}
