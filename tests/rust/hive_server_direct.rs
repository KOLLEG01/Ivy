use super::*;
use ivy_hive::discovery::CoreCalls;

fn frame(method: &str, params: Value) -> Value {
    json!({"jsonrpc":"2.0","id":"call","method":method,"params":params})
}
fn http(method: &str, params: Value) -> Value {
    let mut caller = context(None);
    caller["transport"] = json!("http");
    json!({"op":"direct","name":"call","context":caller,"frame":frame(method,params)})
}
fn core(method: &str, arguments: Value) -> Value {
    let name = format!("hive.{method}");
    // Subscriptions are direct server methods but absent from the retained
    // discovery inventory. Preserve that not_found boundary explicitly.
    let hash = if method == "notifications.subscribe" {
        assert!(CoreCalls::default().binding(&name.clone().into()).is_err());
        format!("sha256:{}", "a".repeat(64))
    } else {
        CoreCalls::default()
            .binding(&name.clone().into())
            .unwrap()
            .get("definitionHash")
            .unwrap()
            .as_str()
            .unwrap()
    };
    json!({"serviceName":"hive","qualifiedName":name,"expectedDefinitionHash":hash,"arguments":arguments})
}
fn trace(name: impl Into<String>, steps: Vec<Value>) -> Value {
    json!({"name":name.into(),"realDirect":true,"steps":steps})
}
fn snapshot(packages: Vec<Value>) -> Value {
    json!({"objectId":"catalog-object","objectRevision":1,"catalog":{"schemaVersion":1,"revision":1,"packages":packages}})
}
fn package_complete(name: &str, value: Value) -> Value {
    json!({"op":"complete","name":name,"value":value})
}
fn authorization() -> Value {
    let build = digest(b"fixture/1.0.0");
    json!({"componentId":"fixture","version":"1.0.0","buildId":build,"archiveHash":digest(b"abc"),"bytes":3,
        "manifest":{"schemaVersion":1,"componentId":"fixture","kind":"service","version":"1.0.0","buildId":build,"connectsToHive":false,
            "requirements":{"node":">=24.18.0 <25.0.0","hiveProtocol":1,"contracts":[]},
            "entrypoint":{"executable":"node","args":["dist/main.js"],"timeoutMs":5000},
            "readiness":{"timeoutMs":5000,"command":{"executable":"node","args":["dist/health.js"],"timeoutMs":5000}},
            "shutdown":{"timeoutMs":1000},"restart":{"policy":"always","minimumDelayMs":100,"maximumDelayMs":500}}})
}
fn publishers(t: &mut Value) {
    t["options"]["packagePublisherPrincipalIds"] = json!(["provider-principal"]);
}

#[test]
fn original_core_recursion_session_validation_and_fallback() {
    let mut traces = vec![];
    for (name, request) in super::super::tool_routing_cases::requests() {
        let mut step = http("unused", json!({}));
        step["frame"] = request;
        traces.push(trace(format!("canonical-{name}"), vec![step]));
    }
    for method in [
        "notifications.subscribe",
        "packages.catalog",
        "packages.authorizeUpload",
        "objects.read",
    ] {
        let args = if method == "notifications.subscribe" {
            json!({"filters":[]})
        } else if method == "packages.catalog" {
            json!({"after":0,"includeUis":true})
        } else {
            json!({})
        };
        let mut t = trace(
            format!("core-{method}"),
            vec![http("discovery.call", core(method, args))],
        );
        t["packages"] = json!(false);
        traces.push(t);
    }
    for (field, value) in [
        ("expectedCallerPrincipalId", json!("other")),
        ("serviceNodeId", json!("node")),
        ("operationId", json!("operation")),
        (
            "expectedDefinitionHash",
            json!(format!("sha256:{}", "a".repeat(64))),
        ),
        ("arguments", Value::Null),
    ] {
        let mut params = core("packages.catalog", json!({"after":0,"includeUis":true}));
        params[field] = value;
        traces.push(trace(
            format!("core-order-{field}"),
            vec![http("discovery.call", params)],
        ));
    }
    for (transport, clock) in [("http", json!([0, 10])), ("ws", json!([0, 0, 10]))] {
        let params = core("packages.catalog", json!({"after":0,"includeUis":true}));
        let mut caller = context(None);
        caller["expiresAt"] = json!(10);
        caller["transport"] = json!(transport);
        let steps = if transport == "http" {
            vec![
                json!({"op":"direct","name":"expired","context":caller,"frame":frame("discovery.call",params),"clock":clock}),
            ]
        } else {
            vec![
                json!({"op":"connect","name":"c","context":caller}),
                json!({"op":"message","peer":"c","frame":frame("discovery.call",params),"clock":clock}),
            ]
        };
        let mut t = trace(format!("recursive-expiry-{transport}"), steps);
        t["now"] = json!(0);
        traces.push(t);
    }
    let mut session = trace(
        "direct-subscription-keeps-session-and-changes",
        vec![
            connect("c", None),
            message(
                "c",
                frame(
                    "notifications.subscribe",
                    json!({"filters":[],"changes":["objects/a"]}),
                ),
            ),
            json!({"op":"changes","scopes":["objects/a/child"]}),
            message(
                "c",
                frame("discovery.call", core("objects.read", json!({}))),
            ),
        ],
    );
    session["options"]["validateMessages"] = json!(true);
    session["expectKinds"] = json!(["send", "worker"]);
    traces.push(session);
    for value in [
        Value::Null,
        json!([]),
        json!({}),
        json!({"method":"unknown"}),
    ] {
        let mut step = http("unused", json!({}));
        step["frame"] = value;
        let mut t = trace("unrecognized-method-falls-through", vec![step]);
        t["expectKinds"] = json!(["direct-fallback"]);
        traces.push(t);
    }
    compare("direct-core", traces);
}

#[test]
fn original_package_start_order_permissions_output_and_retained_authorization() {
    let auth = authorization();
    let mut entry = auth.clone();
    entry["revision"] = json!(1);
    entry["publishedAt"] = json!("1970-01-01T00:00:00.000Z");
    entry["publisherPrincipalId"] = json!("provider-principal");
    let mut traces = vec![];
    for (name, method, params, configured, worker) in [
        (
            "input-before-configuration",
            "packages.catalog",
            json!({"after":-1,"includeUis":true}),
            false,
            true,
        ),
        (
            "missing-configuration",
            "packages.catalog",
            json!({"after":0,"includeUis":true}),
            false,
            true,
        ),
        (
            "configuration-before-publisher",
            "packages.authorizeUpload",
            auth.clone(),
            false,
            true,
        ),
        (
            "missing-worker",
            "packages.catalog",
            json!({"after":0,"includeUis":true}),
            true,
            false,
        ),
        (
            "forbidden-publisher",
            "packages.authorizeUpload",
            auth.clone(),
            true,
            true,
        ),
    ] {
        let mut t = trace(name, vec![http(method, params)]);
        t["packages"] = json!(configured);
        t["packageWorker"] = json!(worker);
        traces.push(t);
    }
    for validate in [false, true] {
        for value in [snapshot(vec![]), json!({"malformed":true})] {
            let mut t = trace(
                format!("catalog-output-{validate}-{value}"),
                vec![
                    http("packages.catalog", json!({"after":7,"includeUis":false})),
                    package_complete("package:1", value),
                ],
            );
            t["options"]["validateMessages"] = json!(validate);
            t["expectKinds"] = json!(["package-worker", "direct-return"]);
            traces.push(t);
        }
        let mut t = trace(
            format!("authorize-identical-{validate}"),
            vec![
                http("packages.authorizeUpload", auth.clone()),
                package_complete("package:1", snapshot(vec![entry.clone()])),
            ],
        );
        publishers(&mut t);
        t["options"]["validateMessages"] = json!(validate);
        t["expectKinds"] = json!(["package-worker", "direct-ok"]);
        traces.push(t);
    }
    let mut conflicting = entry.clone();
    conflicting["archiveHash"] = json!(digest(b"different"));
    let mut t = trace(
        "authorize-retains-immutable-version-guard",
        vec![
            http("packages.authorizeUpload", auth.clone()),
            package_complete("package:1", snapshot(vec![conflicting])),
        ],
    );
    publishers(&mut t);
    t["expectKinds"] = json!(["package-worker", "direct-error"]);
    traces.push(t);
    let mut invalid = auth.clone();
    invalid["manifest"]["version"] = json!("2.0.0");
    let mut t = trace(
        "package-policy-validation-is-an-eager-promise-rejection",
        vec![http("packages.authorizeUpload", invalid)],
    );
    publishers(&mut t);
    t["expectKinds"] = json!(["now", "direct-return", "direct-error"]);
    traces.push(t);
    let mut dropped = http("packages.authorizeUpload", auth.clone());
    dropped["dropResult"] = json!(true);
    let mut t = trace(
        "unpolled-dropped-authorization-retains-grant-continuation",
        vec![dropped, package_complete("package:1", snapshot(vec![]))],
    );
    publishers(&mut t);
    t["expectKinds"] = json!(["package-worker", "direct-return", "iso"]);
    traces.push(t);
    let mut t = trace(
        "worker-rejection-retains-raw-details",
        vec![
            http("packages.catalog", json!({"after":0,"includeUis":true})),
            json!({"op":"complete","name":"package:1","error":{"code":"held","messageUnits":[101,55296],"outcome":"completed","details":null}}),
        ],
    );
    t["expectKinds"] = json!(["direct-error"]);
    traces.push(t);
    let mut t = trace(
        "recursive-package-catalog-keeps-input-and-output",
        vec![
            http(
                "discovery.call",
                core("packages.catalog", json!({"after":2,"includeUis":true})),
            ),
            package_complete("package:1", snapshot(vec![])),
        ],
    );
    t["options"]["validateMessages"] = json!(true);
    t["expectKinds"] = json!(["direct-ok"]);
    traces.push(t);
    let mut t = trace(
        "recursive-authorize-keeps-original-caller",
        vec![
            http(
                "discovery.call",
                core("packages.authorizeUpload", auth.clone()),
            ),
            package_complete("package:1", snapshot(vec![entry.clone()])),
        ],
    );
    publishers(&mut t);
    t["options"]["validateMessages"] = json!(true);
    t["expectKinds"] = json!(["direct-ok"]);
    traces.push(t);
    let mut second = http("packages.catalog", json!({"after":0,"includeUis":false}));
    second["name"] = json!("second");
    let mut t = trace(
        "pending-authorization-does-not-serialize-independent-catalog",
        vec![
            http("packages.authorizeUpload", auth),
            second,
            package_complete("package:2", snapshot(vec![])),
            package_complete("package:1", snapshot(vec![entry])),
        ],
    );
    publishers(&mut t);
    t["expectKinds"] = json!(["package-worker", "direct-ok"]);
    traces.push(t);
    compare("direct-packages", traces);
}

#[test]
fn original_direct_provider_selection_forwarding_and_completion() {
    let mut traces = vec![];
    let base: Value = serde_json::from_str(&encode(&super::super::params())).unwrap();
    for method in ["tools.call", "discovery.call"] {
        for validate in [false, true] {
            let mut params = base.clone();
            params["serviceName"] = json!("provider");
            params["serviceNodeId"] = json!("node");
            let mut t = trace(
                format!("provider-{method}-{validate}"),
                vec![
                    connect("p", Some(("node", 1.0))),
                    update("node", 1.0),
                    connect("c", None),
                    message("c", frame(method, params.clone())),
                    json!({"op":"result","peer":"p","frame":{"jsonrpc":"2.0","id":"hive-call-fixture-1","result":"done"}}),
                ],
            );
            t["options"]["validateMessages"] = json!(validate);
            t["expectKinds"] = json!(["id", "send"]);
            traces.push(t);
            params.as_object_mut().unwrap().remove("serviceNodeId");
            traces.push(trace(
                format!("missing-exact-node-{method}"),
                vec![http(method, params)],
            ));
        }
    }
    for (name, field, value) in [
        (
            "definition",
            "expectedDefinitionHash",
            json!(format!("sha256:{}", "b".repeat(64))),
        ),
        ("caller", "expectedCallerPrincipalId", json!("different")),
        ("input", "arguments", json!(0)),
        ("conflict", "serviceName", json!("different")),
    ] {
        let mut params = base.clone();
        params["serviceNodeId"] = json!("node");
        params[field] = value;
        let mut t = trace(
            format!("provider-guard-{name}"),
            vec![update("node", 1.0), http("tools.call", params)],
        );
        t["options"]["validateMessages"] = json!(true);
        traces.push(t);
    }
    let mut params = base.clone();
    params["serviceNodeId"] = json!("node");
    let mut t = trace(
        "completion-rechecks-generation",
        vec![
            connect("p", Some(("node", 1.0))),
            update("node", 1.0),
            http("tools.call", params),
            update("node", 2.0),
            json!({"op":"result","peer":"p","frame":{"jsonrpc":"2.0","id":"hive-call-fixture-1","result":"stale"}}),
        ],
    );
    t["expectKinds"] = json!(["direct-error"]);
    traces.push(t);
    compare("direct-provider", traces);
}
