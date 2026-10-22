use super::mcp_inbound::{at, modern, named, reference, set};
use super::*;
use base64::{Engine, engine::general_purpose::STANDARD};
use ivy_hive::{mcp_inbound as inbound, mcp_legacy as legacy, mcp_parameters as parameters};

fn save(suffix: &str, value: &IvyJson) {
    if let Some(prefix) = std::env::var_os("IVY_HIVE_MCP_PARAMETERS_CORPUS") {
        std::fs::write(
            format!("{}-{suffix}.json", prefix.to_string_lossy()),
            encode(value),
        )
        .unwrap();
    }
}
fn legacy(method: &str) -> IvyJson {
    obj([
        ("name", method.into()),
        (
            "headers",
            json(
                r#"{"accept":"application/json, text/event-stream","content-type":"application/json"}"#,
            ),
        ),
        (
            "body",
            obj([
                ("jsonrpc", "2.0".into()),
                ("id", "request".into()),
                ("method", method.into()),
                ("params", json("{}")),
            ]),
        ),
    ])
}
fn initialize() -> IvyJson {
    let mut row = legacy("initialize");
    set(
        &mut row,
        &["body", "params"],
        json(
            r#"{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"fixture","version":"1.0.0"}}"#,
        ),
    );
    row
}
fn body_params(row: &mut IvyJson) -> &mut IvyJson {
    row.get_mut("body").unwrap().get_mut("params").unwrap()
}
fn decoded(wire: &IvyJson) -> IvyJson {
    let body = STANDARD
        .decode(wire.required("bodyBase64").unwrap().as_str().unwrap())
        .unwrap();
    if body.is_empty() {
        return IvyJson::Null;
    }
    let body = String::from_utf8(body).unwrap();
    json(
        if let Some(body) = body.strip_prefix("event: message\ndata: ") {
            body.trim_end_matches('\n')
        } else {
            &body
        },
    )
}
fn reply(value: ivy_hive::mcp_protocol::Reply) -> IvyJson {
    obj([
        ("status", number(value.status.into())),
        ("headers", value.headers),
        ("bodyBase64", STANDARD.encode(value.body.as_bytes()).into()),
    ])
}
fn terminal(row: &IvyJson) -> Option<IvyJson> {
    let request = at(row);
    let route = inbound::classify(&request);
    inbound::entry_rejection(&request, &route)
        .unwrap()
        .or_else(|| legacy::admission(&request, &route).unwrap())
        .map(reply)
}
fn variants() -> Vec<IvyJson> {
    vec![
        IvyJson::Null,
        true.into(),
        number(1),
        "value".into(),
        json("[]"),
        json("{}"),
        json("[\"item\"]"),
    ]
}
fn parameter_cases() -> Vec<IvyJson> {
    let mut cases = vec![];
    for modern_era in [false, true] {
        for method in [
            "ping",
            "initialize",
            "server/discover",
            "tools/list",
            "tools/call",
        ] {
            let mut base = if modern_era {
                modern(method)
            } else if method == "initialize" {
                initialize()
            } else {
                legacy(method)
            };
            if method == "tools/call" {
                body_params(&mut base).insert("name", "fixture".into());
                if modern_era {
                    set(&mut base, &["headers", "mcp-name"], "fixture".into());
                }
            }
            let tag = format!("{}-{method}", if modern_era { "modern" } else { "legacy" });
            cases.push(named(base.clone(), format!("{tag}-base")));
            let mut absent = base.clone();
            absent.get_mut("body").unwrap().remove("params");
            cases.push(named(absent, format!("{tag}-absent-params")));
            let mut extra = base.clone();
            body_params(&mut extra).insert("foreign", true.into());
            cases.push(named(extra, format!("{tag}-foreign-params")));
            let mut meta = base.clone();
            set(
                &mut meta,
                &["body", "params", "_meta", "progressToken"],
                "progress".into(),
            );
            set(
                &mut meta,
                &[
                    "body",
                    "params",
                    "_meta",
                    "io.modelcontextprotocol/related-task",
                ],
                json(r#"{"taskId":"task","foreign":true}"#),
            );
            set(
                &mut meta,
                &["body", "params", "_meta", "foreign"],
                json(r#"{"kept":true}"#),
            );
            cases.push(named(meta, format!("{tag}-metadata")));
            let mut retry = base.clone();
            body_params(&mut retry).insert("inputResponses", json(r#"{"ignored":"driver"}"#));
            // tools/call validates lifted requestState later, before the callback.
            // Keep this case at the parameter boundary; other methods cover its lift.
            if method != "tools/call" {
                body_params(&mut retry).insert("requestState", number(123));
            }
            cases.push(named(retry, format!("{tag}-retry-lift")));
            if !modern_era {
                let mut reserved = base.clone();
                set(
                    &mut reserved,
                    &[
                        "body",
                        "params",
                        "_meta",
                        "io.modelcontextprotocol/clientInfo",
                    ],
                    true.into(),
                );
                set(
                    &mut reserved,
                    &[
                        "body",
                        "params",
                        "_meta",
                        "io.modelcontextprotocol/logLevel",
                    ],
                    number(99),
                );
                cases.push(named(reserved, format!("{tag}-unclaimed-envelope-lift")));
            }
            let field = match method {
                "tools/list" => Some("cursor"),
                "tools/call" => Some("arguments"),
                _ => None,
            };
            if let Some(field) = field {
                for (i, value) in variants().into_iter().enumerate() {
                    let mut row = base.clone();
                    body_params(&mut row).insert(field, value);
                    cases.push(named(row, format!("{tag}-{field}-{i}")));
                }
            }
            if method == "tools/call" {
                for (i, value) in variants().into_iter().enumerate() {
                    let mut row = base.clone();
                    body_params(&mut row).insert("task", value);
                    cases.push(named(row, format!("{tag}-task-{i}")));
                }
                for (i, value) in variants().into_iter().enumerate() {
                    let mut row = base.clone();
                    set(&mut row, &["body", "params", "task", "ttl"], value);
                    cases.push(named(row, format!("{tag}-ttl-{i}")));
                }
                let mut row = base.clone();
                body_params(&mut row).insert("task", json(r#"{"ttl":1.25,"foreign":true}"#));
                body_params(&mut row).insert("arguments", json(r#"{"__proto__":1,"constructor":2,"10":"ten","2":"two","nested":{"__proto__":true},"\ud800":"value"}"#));
                cases.push(named(row, format!("{tag}-record-projection")));
                for (i, value) in variants().into_iter().enumerate() {
                    let mut row = base.clone();
                    body_params(&mut row).insert("name", value);
                    body_params(&mut row).insert("arguments", IvyJson::Null);
                    body_params(&mut row).insert("task", true.into());
                    cases.push(named(row, format!("{tag}-multiple-issues-{i}")));
                }
                if !modern_era {
                    let mut row = base.clone();
                    body_params(&mut row)
                        .insert("name", IvyJson::String(JsString(vec![0xd800; 160])));
                    cases.push(named(row, "legacy-name-utf16"));
                }
            }
        }
    }
    let base = initialize();
    for field in ["protocolVersion", "capabilities", "clientInfo"] {
        let mut row = base.clone();
        body_params(&mut row).remove(field);
        cases.push(named(row, format!("initialize-missing-{field}")));
        for (index, value) in variants().into_iter().enumerate() {
            let mut row = base.clone();
            body_params(&mut row).insert(field, value);
            cases.push(named(row, format!("initialize-{field}-{index}")));
        }
    }
    for path in [
        vec!["experimental"],
        vec!["experimental", "vendor"],
        vec!["sampling"],
        vec!["sampling", "context"],
        vec!["sampling", "tools"],
        vec!["elicitation"],
        vec!["elicitation", "form"],
        vec!["elicitation", "form", "applyDefaults"],
        vec!["elicitation", "url"],
        vec!["roots"],
        vec!["roots", "listChanged"],
        vec!["tasks"],
        vec!["tasks", "list"],
        vec!["tasks", "cancel"],
        vec!["tasks", "requests"],
        vec!["tasks", "requests", "sampling"],
        vec!["tasks", "requests", "sampling", "createMessage"],
        vec!["tasks", "requests", "elicitation", "create"],
        vec!["extensions"],
        vec!["extensions", "vendor"],
    ] {
        for (index, value) in variants().into_iter().enumerate() {
            let mut row = base.clone();
            let mut full = vec!["body", "params", "capabilities"];
            full.extend(&path);
            set(&mut row, &full, value);
            cases.push(named(
                row,
                format!("initialize-capability-{}-{index}", path.join("-")),
            ));
        }
    }
    let mut row = base.clone();
    set(
        &mut row,
        &["body", "params", "capabilities"],
        json(
            r#"{"unknown":true,"elicitation":{},"tasks":{"unknown":1,"requests":{"sampling":{"createMessage":{},"foreign":1},"unknown":true}}}"#,
        ),
    );
    set(
        &mut row,
        &["body", "params", "clientInfo"],
        json(
            r#"{"version":"1","name":"client","title":"Title","icons":[{"src":"icon","theme":"dark","sizes":["any"],"extra":true}],"extra":true}"#,
        ),
    );
    cases.push(named(row, "initialize-typed-projection-order"));
    for path in [
        vec!["name"],
        vec!["title"],
        vec!["version"],
        vec!["websiteUrl"],
        vec!["description"],
        vec!["icons"],
    ] {
        let mut row = base.clone();
        let mut full = vec!["body", "params", "clientInfo"];
        full.extend(&path);
        set(&mut row, &full, IvyJson::Null);
        cases.push(named(row, format!("initialize-info-{}", path.join("-"))));
    }
    let mut row = base.clone();
    set(
        &mut row,
        &["body", "params", "clientInfo", "icons"],
        json(r#"[{"src":1,"mimeType":true,"sizes":[1,null],"theme":"wrong"}]"#),
    );
    cases.push(named(row, "initialize-icon-multiple-issues"));
    cases
}

#[test]
fn registered_method_parameters_match_actual_sdk_projection_and_diagnostics() {
    let cases = parameter_cases();
    let source = super::mcp_protocol::oracle(&obj([
        ("mode", "parameter-projection".into()),
        ("cases", IvyJson::Array(cases.clone())),
    ]));
    save("source", &source);
    let observations = source.required("observations").unwrap().as_array().unwrap();
    assert_eq!(cases.len(), observations.len());
    let mut outcomes = vec![];
    for (row, observed) in cases.iter().zip(observations) {
        let route = inbound::classify(&at(row));
        let native = if let Some(reply) = terminal(row) {
            obj([("terminal", reply)])
        } else if let Some(result) = parameters::request(
            row.required("body").unwrap(),
            matches!(route, inbound::Route::Modern { .. }),
        ) {
            match result {
                Ok(request) => obj([("request", request)]),
                Err(issues) => obj([
                    (
                        "diagnostic",
                        IvyJson::String(parameters::diagnostic(&issues).unwrap()),
                    ),
                    (
                        "issues",
                        IvyJson::Array(issues.iter().map(parameters::Issue::to_json).collect()),
                    ),
                ]),
            }
        } else {
            obj([("unregistered", true.into())])
        };
        outcomes.push(obj([
            ("name", row.required("name").unwrap().clone()),
            ("native", native.clone()),
            ("source", observed.clone()),
        ]));
    }
    save(
        "comparison",
        &obj([
            ("cases", IvyJson::Array(cases.clone())),
            ("source", source.clone()),
            ("outcomes", IvyJson::Array(outcomes.clone())),
        ]),
    );
    for ((row, observed), outcome) in cases.iter().zip(observations).zip(&outcomes) {
        let wire = observed.required("wire").unwrap();
        let calls = observed.required("calls").unwrap().as_array().unwrap();
        let native = outcome.required("native").unwrap();
        if let Some(reply) = native.get("terminal") {
            assert_eq!(reply, wire, "Transport differs for {}", text(row, "name"));
            assert!(calls.is_empty());
        } else if let Some(request) = native.get("request") {
            assert_eq!(
                calls,
                &[request.clone()],
                "Handler projection differs for {}",
                text(row, "name")
            );
        } else if let Some(diagnostic) = native.get("diagnostic").and_then(IvyJson::as_js_string) {
            assert!(calls.is_empty(), "Invalid params reached handler");
            let response = decoded(wire);
            let error = response.required("error").unwrap();
            let tool = row.required("body").unwrap().get("method") == Some(&"tools/call".into());
            let mut message = if tool {
                JsString::from("Invalid tools/call request: ").0
            } else {
                vec![]
            };
            message.extend(&diagnostic.0);
            assert_eq!(
                error.get("code"),
                Some(&number(if tool { -32602 } else { -32603 })),
                "Schema code differs for {}",
                text(row, "name")
            );
            assert_eq!(
                error.get("message"),
                Some(&IvyJson::String(JsString(message))),
                "Schema diagnostic differs for {}",
                text(row, "name")
            );
        } else {
            assert!(calls.is_empty(), "Unregistered method reached a handler");
        }
    }
}
fn text(value: &IvyJson, key: &str) -> String {
    value.required(key).unwrap().as_str().unwrap()
}

#[test]
fn legacy_stateless_admission_matches_original_http() {
    let mut cases = vec![];
    for version in [
        None,
        Some(""),
        Some("2024-11-05"),
        Some("2025-11-25"),
        Some("2025-01-01"),
        Some(" \t2025-11-25\t "),
        Some("\u{a0}2025-11-25\u{a0}"),
    ] {
        for (index, body) in [
            initialize().required("body").unwrap().clone(),
            json(r#"{"jsonrpc":"2.0","id":"bad-init","method":"initialize","params":{}}"#),
            json(r#"{"jsonrpc":"2.0","id":"ping","method":"ping"}"#),
            json(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#),
            json(r#"{"jsonrpc":"2.0","id":"response","result":{}}"#),
            json(
                r#"{"jsonrpc":"2.0","id":"response","error":{"code":-32603,"message":"Failure"}}"#,
            ),
        ]
        .into_iter()
        .enumerate()
        {
            let mut row = legacy("ping");
            row.insert("body", body);
            if let Some(version) = version {
                set(
                    &mut row,
                    &["headers", "mcp-protocol-version"],
                    version.into(),
                );
            }
            cases.push(named(
                row,
                format!("legacy-version-{}-{index}", version.unwrap_or("absent")),
            ));
        }
    }
    for (index, body) in [
        IvyJson::Array(vec![initialize().required("body").unwrap().clone(), json(r#"{"jsonrpc":"2.0","id":"other","method":"ping"}"#)]),
        json(r#"[{"jsonrpc":"2.0","method":"notifications/initialized"},{"jsonrpc":"2.0","id":"response","result":{}}]"#),
        json(r#"[]"#),
    ].into_iter().enumerate() {
        let mut row = legacy("ping"); row.insert("body", body); cases.push(named(row, format!("legacy-batch-{index}")));
    }
    for (index, path) in [
        vec![
            "capabilities",
            "tasks",
            "requests",
            "sampling",
            "createMessage",
        ],
        vec!["capabilities", "elicitation", "form"],
        vec!["clientInfo", "icons"],
    ]
    .into_iter()
    .enumerate()
    {
        let mut row = initialize();
        let mut full = vec!["body", "params"];
        full.extend(&path);
        set(&mut row, &full, true.into());
        set(
            &mut row,
            &["headers", "mcp-protocol-version"],
            "2025-01-01".into(),
        );
        cases.push(named(row, format!("malformed-initialize-version-{index}")));
    }
    let source = super::mcp_protocol::oracle(&obj([
        ("mode", "baseline".into()),
        ("client", false.into()),
        ("cases", IvyJson::Array(cases.clone())),
    ]));
    save("legacy-source", &source);
    let mut outcomes = vec![];
    for (row, wire) in cases
        .iter()
        .zip(source.required("wire").unwrap().as_array().unwrap())
    {
        let native = terminal(row);
        let expected = obj([
            ("status", wire.required("status").unwrap().clone()),
            ("headers", wire.required("headers").unwrap().clone()),
            ("bodyBase64", wire.required("bodyBase64").unwrap().clone()),
        ]);
        outcomes.push(obj([
            ("name", row.required("name").unwrap().clone()),
            ("native", native.clone().unwrap_or(IvyJson::Null)),
            ("original", expected.clone()),
        ]));
    }
    save(
        "legacy-comparison",
        &obj([
            ("cases", IvyJson::Array(cases.clone())),
            ("source", source.clone()),
            ("outcomes", IvyJson::Array(outcomes.clone())),
        ]),
    );
    for outcome in &outcomes {
        if let Some(native) = outcome.get("native").filter(|v| !v.is_null()) {
            assert_eq!(
                native,
                outcome.required("original").unwrap(),
                "Legacy transport differs for {}",
                text(outcome, "name")
            );
        }
    }
    // The original cached POST corpus remains a separate immutable source cut.
    let reference = reference();
    assert_eq!(
        reference
            .required("cases")
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        57
    );
}
