use super::*;
use base64::{Engine, engine::general_purpose::STANDARD};
use ivy_hive::mcp_inbound::{self as inbound, Request as Inbound};

fn original(input: &IvyJson) -> IvyJson {
    super::mcp_protocol::oracle(input)
}
fn save(suffix: &str, value: &IvyJson) {
    if let Some(prefix) = std::env::var_os("IVY_HIVE_MCP_INBOUND_CORPUS") {
        std::fs::write(
            format!("{}-{suffix}.json", prefix.to_string_lossy()),
            encode(value),
        )
        .unwrap();
    }
}
pub(super) fn reference() -> IvyJson {
    if let Some(path) = std::env::var_os("IVY_HIVE_MCP_HTTP_REFERENCE") {
        json(&std::fs::read_to_string(path).unwrap())
    } else {
        original(&obj([
            ("mode", "baseline".into()),
            ("client", false.into()),
        ]))
    }
}
fn headers() -> IvyJson {
    json(r#"{"accept":"application/json, text/event-stream","content-type":"application/json"}"#)
}
pub(super) fn modern(method: &str) -> IvyJson {
    let mut headers = headers();
    headers.insert("mcp-protocol-version", inbound::VERSION.into());
    headers.insert("mcp-method", method.into());
    obj([
        ("name", method.into()),
        ("headers", headers),
        (
            "body",
            obj([
                ("jsonrpc", "2.0".into()),
                ("id", "request".into()),
                ("method", method.into()),
                (
                    "params",
                    obj([(
                        "_meta",
                        obj([
                            (inbound::VERSION_META, inbound::VERSION.into()),
                            (inbound::CAPABILITIES_META, json("{}")),
                        ]),
                    )]),
                ),
            ]),
        ),
    ])
}
pub(super) fn at<'a>(row: &'a IvyJson) -> Inbound<'a> {
    let headers = row.required("headers").unwrap();
    Inbound {
        http_method: if row.get("method") == Some(&"GET".into()) {
            "GET"
        } else if row.get("method") == Some(&"DELETE".into()) {
            "DELETE"
        } else {
            "POST"
        },
        body: row.required("body").unwrap(),
        protocol_version: headers
            .get("mcp-protocol-version")
            .and_then(IvyJson::as_js_string),
        method: headers.get("mcp-method").and_then(IvyJson::as_js_string),
        name: headers.get("mcp-name").and_then(IvyJson::as_js_string),
        content_type: headers.get("content-type").and_then(IvyJson::as_js_string),
        accept: headers.get("accept").and_then(IvyJson::as_js_string),
    }
}
pub(super) fn set(value: &mut IvyJson, path: &[&str], next: IvyJson) {
    if path.len() == 1 {
        value.insert(path[0], next);
    } else {
        if value.get(path[0]).is_none() {
            value.insert(path[0], json("{}"));
        }
        set(value.get_mut(path[0]).unwrap(), &path[1..], next);
    }
}
pub(super) fn named(mut row: IvyJson, name: impl Into<JsString>) -> IvyJson {
    row.insert("name", IvyJson::String(name.into()));
    row
}
fn classification_cases(mut cases: Vec<IvyJson>) -> Vec<IvyJson> {
    let values = [
        IvyJson::Null,
        true.into(),
        number(1),
        "bad".into(),
        json("[]"),
        json("{}"),
    ];
    for (index, value) in values.iter().enumerate() {
        for key in [
            inbound::VERSION_META,
            inbound::CAPABILITIES_META,
            "io.modelcontextprotocol/clientInfo",
            "io.modelcontextprotocol/logLevel",
        ] {
            for method in ["tools/list", "initialize", "notifications/fixture"] {
                let mut row = modern(method);
                set(&mut row, &["body", "params", "_meta", key], value.clone());
                if method.starts_with("notifications/") {
                    row.get_mut("body").unwrap().remove("id");
                }
                cases.push(named(row, format!("meta-{method}-{key}-{index}")));
            }
        }
        for key in [
            "experimental",
            "sampling",
            "elicitation",
            "roots",
            "extensions",
        ] {
            let mut row = modern("tools/list");
            set(
                &mut row,
                &["body", "params", "_meta", inbound::CAPABILITIES_META, key],
                value.clone(),
            );
            cases.push(named(row, format!("capability-{key}-{index}")));
        }
    }
    let info = json(
        r#"{"name":"client","title":"Title","icons":[{"src":"icon","mimeType":"image/png","sizes":["any"],"theme":"light"}],"version":"1","websiteUrl":"https://example.invalid","description":"description"}"#,
    );
    for (index, value) in values.iter().enumerate() {
        for key in [
            "name",
            "title",
            "icons",
            "version",
            "websiteUrl",
            "description",
        ] {
            let mut row = modern("tools/list");
            set(
                &mut row,
                &[
                    "body",
                    "params",
                    "_meta",
                    "io.modelcontextprotocol/clientInfo",
                ],
                info.clone(),
            );
            set(
                &mut row,
                &[
                    "body",
                    "params",
                    "_meta",
                    "io.modelcontextprotocol/clientInfo",
                    key,
                ],
                value.clone(),
            );
            cases.push(named(row, format!("info-{key}-{index}")));
        }
        for key in ["src", "mimeType", "sizes", "theme"] {
            let mut icon = json(r#"{"src":"icon"}"#);
            icon.insert(key, value.clone());
            let mut next = info.clone();
            next.insert("icons", IvyJson::Array(vec![icon]));
            let mut row = modern("tools/list");
            set(
                &mut row,
                &[
                    "body",
                    "params",
                    "_meta",
                    "io.modelcontextprotocol/clientInfo",
                ],
                next,
            );
            cases.push(named(row, format!("icon-{key}-{index}")));
        }
        for (group, key) in [
            ("sampling", "context"),
            ("sampling", "tools"),
            ("elicitation", "form"),
            ("elicitation", "url"),
            ("roots", "listChanged"),
            ("experimental", "custom"),
            ("extensions", "custom"),
        ] {
            let mut row = modern("tools/list");
            set(
                &mut row,
                &[
                    "body",
                    "params",
                    "_meta",
                    inbound::CAPABILITIES_META,
                    group,
                    key,
                ],
                value.clone(),
            );
            cases.push(named(row, format!("nested-{group}-{key}-{index}")));
        }
        let mut row = modern("tools/list");
        set(
            &mut row,
            &[
                "body",
                "params",
                "_meta",
                inbound::CAPABILITIES_META,
                "elicitation",
                "form",
                "applyDefaults",
            ],
            value.clone(),
        );
        cases.push(named(row, format!("apply-defaults-{index}")));
    }
    for method in ["initialize", "tools/list", "notifications/fixture"] {
        for version in ["2025-11-25", inbound::VERSION, "2030-01-01", ""] {
            for header in [
                None,
                Some("2025-11-25"),
                Some(inbound::VERSION),
                Some("2030-01-01"),
            ] {
                let mut row = modern(method);
                set(
                    &mut row,
                    &["body", "params", "_meta", inbound::VERSION_META],
                    version.into(),
                );
                if let Some(header) = header {
                    set(
                        &mut row,
                        &["headers", "mcp-protocol-version"],
                        header.into(),
                    );
                } else {
                    row.get_mut("headers")
                        .unwrap()
                        .remove("mcp-protocol-version");
                }
                if method.starts_with("notifications/") {
                    row.get_mut("body").unwrap().remove("id");
                    row.get_mut("body")
                        .unwrap()
                        .get_mut("params")
                        .unwrap()
                        .get_mut("_meta")
                        .unwrap()
                        .remove(inbound::CAPABILITIES_META);
                }
                cases.push(named(row, format!("version-{method}-{version}-{header:?}")));
            }
        }
        for missing in ["all", inbound::VERSION_META, inbound::CAPABILITIES_META] {
            let mut row = modern(method);
            if missing == "all" {
                row.get_mut("body")
                    .unwrap()
                    .get_mut("params")
                    .unwrap()
                    .remove("_meta");
            } else {
                row.get_mut("body")
                    .unwrap()
                    .get_mut("params")
                    .unwrap()
                    .get_mut("_meta")
                    .unwrap()
                    .remove(missing);
            }
            if method.starts_with("notifications/") {
                row.get_mut("body").unwrap().remove("id");
            }
            cases.push(named(row, format!("missing-{method}-{missing}")));
        }
    }
    for (index,body) in [
        json("[]"),json(r#"[{"jsonrpc":"2.0","id":1,"method":"ping"}]"#),
        json(r#"[{"jsonrpc":"2.0","result":{},"id":1},{"jsonrpc":"2.0","error":{"code":1,"message":"x"}},{"jsonrpc":"2.0","method":"notifications/fixture"}]"#),
        json(r#"[{}]"#),IvyJson::Array(vec![modern("tools/list").required("body").unwrap().clone()]),
        json(r#"{"jsonrpc":"2.0","id":1,"result":{}}"#),json(r#"{"jsonrpc":"2.0","error":{"code":1,"message":"x"}}"#),
        json(r#"{"jsonrpc":"2.0","id":1}"#),json(r#"{"jsonrpc":"2.0","id":1,"method":null}"#),
        json(r#"{"jsonrpc":"2.0","id":1,"method":1}"#),
    ].into_iter().enumerate(){let mut row=modern("tools/list");row.insert("body",body);cases.push(named(row,format!("outer-{index}")));}
    for key in ["__proto__", "2", "1", "nested.key"] {
        let mut row = modern("tools/list");
        set(
            &mut row,
            &[
                "body",
                "params",
                "_meta",
                inbound::CAPABILITIES_META,
                "experimental",
                key,
            ],
            number(1),
        );
        cases.push(named(row, format!("record-key-{key}")));
    }
    let mut row = modern("tools/list");
    set(
        &mut row,
        &["headers", "mcp-protocol-version"],
        " \t2026-07-28\t ".into(),
    );
    set(
        &mut row,
        &["headers", "mcp-method"],
        " \ttools/list\t ".into(),
    );
    cases.push(named(row, "header-ows"));
    for version in ["2025-11-25", inbound::VERSION] {
        let mut row = modern("notifications/fixture");
        row.get_mut("body").unwrap().remove("id");
        set(
            &mut row,
            &["body", "params", "_meta", inbound::VERSION_META],
            version.into(),
        );
        set(
            &mut row,
            &["headers", "mcp-protocol-version"],
            version.into(),
        );
        set(&mut row, &["headers", "mcp-method"], "different".into());
        cases.push(named(row, format!("notification-method-{version}")));
    }
    let mut row = modern("notifications/fixture");
    row.get_mut("body").unwrap().remove("id");
    row.get_mut("body")
        .unwrap()
        .get_mut("params")
        .unwrap()
        .get_mut("_meta")
        .unwrap()
        .remove(inbound::CAPABILITIES_META);
    set(
        &mut row,
        &["body", "params", "_meta", inbound::VERSION_META],
        number(1),
    );
    cases.push(named(
        row,
        "notification-malformed-claim-with-missing-capabilities",
    ));
    cases
}
#[test]
fn inbound_classification_matches_pinned_sdk() {
    let reference = reference();
    let cases = classification_cases(
        reference
            .required("cases")
            .unwrap()
            .as_array()
            .unwrap()
            .to_vec(),
    );
    let observed = original(&obj([
        ("mode", "classification".into()),
        ("cases", IvyJson::Array(cases.clone())),
    ]));
    let native: Vec<_> = cases
        .iter()
        .map(|row| {
            obj([
                ("name", row.required("name").unwrap().clone()),
                (
                    "route",
                    inbound::classify(&at(row)).to_json(row.required("body").unwrap()),
                ),
            ])
        })
        .collect();
    save(
        "classification",
        &obj([
            ("cases", IvyJson::Array(cases)),
            ("original", observed.clone()),
            ("native", IvyJson::Array(native.clone())),
        ]),
    );
    let expected = observed
        .required("classifications")
        .unwrap()
        .as_array()
        .unwrap();
    assert_eq!(native.len(), expected.len());
    for (actual, expected) in native.iter().zip(expected) {
        assert_eq!(actual, expected, "Inbound classification differs");
    }
}
fn reply(reply: ivy_hive::mcp_protocol::Reply) -> IvyJson {
    obj([
        ("status", number(reply.status.into())),
        ("headers", reply.headers),
        ("bodyBase64", STANDARD.encode(reply.body.as_bytes()).into()),
    ])
}
fn observe(cases: Vec<IvyJson>, source: &IvyJson) -> Vec<IvyJson> {
    let observations = source.required("wire").unwrap().as_array().unwrap();
    assert_eq!(cases.len(), observations.len());
    let mut result = Vec::new();
    for (row, expected) in cases.iter().zip(observations) {
        if at(row).http_method != "POST" {
            continue;
        }
        let request = at(row);
        let route = inbound::classify(&request);
        let native = inbound::entry_rejection(&request, &route)
            .unwrap()
            .map(reply);
        let expected = obj([
            ("status", expected.required("status").unwrap().clone()),
            ("headers", expected.required("headers").unwrap().clone()),
            (
                "bodyBase64",
                expected.required("bodyBase64").unwrap().clone(),
            ),
        ]);
        if let Some(actual) = &native {
            assert_eq!(
                actual,
                &expected,
                "Entry reply differs for {}",
                text(row, "name")
            );
        } else {
            let body = STANDARD.decode(text(&expected, "bodyBase64")).unwrap();
            if let Ok(body) = std::str::from_utf8(&body) {
                if body.starts_with('{') {
                    let body = json(body);
                    let error = body.get("error");
                    assert!(
                        !matches!(
                            error.and_then(|e| e.get("code")).and_then(IvyJson::as_f64),
                            Some(-32020.0 | -32022.0 | -32600.0)
                        ) && error
                            .and_then(|e| e.get("data"))
                            .and_then(|d| d.get("envelope"))
                            .is_none(),
                        "Entry rejection was missed for {}",
                        text(row, "name")
                    );
                }
            }
        }
        result.push(obj([
            ("name", row.required("name").unwrap().clone()),
            ("input", row.clone()),
            ("route", route.to_json(request.body)),
            ("original", expected),
            ("native", native.unwrap_or(IvyJson::Null)),
        ]));
    }
    result
}
fn header_cases() -> Vec<IvyJson> {
    let mut cases = Vec::new();
    for (index, content) in [
        None,
        Some(""),
        Some("text/plain"),
        Some("application/json; charset=utf-8"),
        Some("Application/JSON"),
        Some("application/jsonx"),
        Some("\u{a0}application/json\u{a0}; charset=utf-8"),
    ]
    .into_iter()
    .enumerate()
    {
        let mut row = modern("tools/list");
        if let Some(value) = content {
            set(&mut row, &["headers", "content-type"], value.into());
        } else {
            row.get_mut("headers").unwrap().remove("content-type");
        }
        cases.push(named(row, format!("content-{index}")));
    }
    for method in ["tools/list", "initialize", "ping", "unknown.method"] {
        let mut row = modern(method);
        row.get_mut("headers").unwrap().remove("mcp-method");
        cases.push(named(row, format!("missing-method-{method}")));
        let mut row = modern(method);
        set(&mut row, &["headers", "mcp-method"], "different".into());
        cases.push(named(row, format!("mismatch-method-{method}")));
    }
    for (method, source) in [
        ("tools/call", "name"),
        ("prompts/get", "name"),
        ("resources/read", "uri"),
    ] {
        for (index, header) in [
            None,
            Some("unknown"),
            Some("different"),
            Some(" \tunknown\t "),
            Some("=?base64?dW5rbm93bg==?="),
            Some("=?base64?dW5rbm93bg?="),
            Some("=?base64?/?="),
            Some("=?base64?/w==?="),
            Some("=?base64?="),
            Some("=?base64??="),
            Some("=?base64?77u/dW5rbm93bg==?="),
        ]
        .into_iter()
        .enumerate()
        {
            let mut row = modern(method);
            set(&mut row, &["body", "params", source], "unknown".into());
            if let Some(value) = header {
                set(&mut row, &["headers", "mcp-name"], value.into());
            }
            cases.push(named(row, format!("name-{method}-{index}")));
        }
    }
    for (name, body_name, header) in [
        ("padding-bits", "f", "=?base64?Zh==?="),
        ("nul", "\0", "=?base64?AA==?="),
        ("bom", "unknown", "=?base64?77u/dW5rbm93bg==?="),
    ] {
        let mut row = modern("tools/call");
        set(&mut row, &["body", "params", "name"], body_name.into());
        set(&mut row, &["headers", "mcp-name"], header.into());
        cases.push(named(row, name));
    }
    for (index, name) in [
        json(r#""\ud800""#),
        json(r#""\udfff""#),
        "long".into(),
        IvyJson::Null,
        number(1),
    ]
    .into_iter()
    .enumerate()
    {
        let mut row = modern("tools/call");
        set(&mut row, &["body", "params", "name"], name);
        set(&mut row, &["headers", "mcp-name"], "other".into());
        set(
            &mut row,
            &["body", "id"],
            IvyJson::String(JsString(vec![0xd800; 160])),
        );
        cases.push(named(row, format!("body-name-{index}")));
    }
    for mutation in [
        "unsupported-missing-method",
        "bad-envelope-mismatch",
        "bad-shape-missing-accept",
        "bad-method-correlation",
        "missing-method-correlation",
        "bad-content-before-shape",
        "legacy-accept-case",
        "legacy-accept-substring",
    ] {
        let mut row = modern("tools/list");
        match mutation {
            "unsupported-missing-method" => {
                set(
                    &mut row,
                    &["body", "params", "_meta", inbound::VERSION_META],
                    "2030-01-01".into(),
                );
                set(
                    &mut row,
                    &["headers", "mcp-protocol-version"],
                    "2030-01-01".into(),
                );
                row.get_mut("headers").unwrap().remove("mcp-method");
            }
            "bad-envelope-mismatch" => {
                set(
                    &mut row,
                    &["body", "params", "_meta", inbound::CAPABILITIES_META],
                    IvyJson::Null,
                );
                set(&mut row, &["headers", "mcp-method"], "wrong".into());
            }
            "bad-shape-missing-accept" => {
                set(&mut row, &["body", "id"], true.into());
                row.get_mut("headers").unwrap().remove("accept");
            }
            "bad-method-correlation" => set(&mut row, &["body", "method"], number(1)),
            "missing-method-correlation" => {
                row.get_mut("body").unwrap().remove("method");
            }
            "bad-content-before-shape" => {
                set(&mut row, &["headers", "content-type"], "text/plain".into());
                set(&mut row, &["body", "id"], true.into());
            }
            "legacy-accept-case" | "legacy-accept-substring" => {
                row.get_mut("body")
                    .unwrap()
                    .get_mut("params")
                    .unwrap()
                    .remove("_meta");
                row.get_mut("headers")
                    .unwrap()
                    .remove("mcp-protocol-version");
                set(
                    &mut row,
                    &["headers", "accept"],
                    if mutation == "legacy-accept-case" {
                        "APPLICATION/JSON, TEXT/EVENT-STREAM"
                    } else {
                        "xapplication/jsonx, xtext/event-streamx"
                    }
                    .into(),
                );
            }
            _ => unreachable!(),
        }
        cases.push(named(row, mutation));
    }
    let mut row = modern("notifications/fixture");
    row.get_mut("body").unwrap().remove("id");
    set(&mut row, &["body", "method"], json(r#""\ud800""#));
    cases.push(named(row, "notification-method-utf16"));
    let mut row = modern("tools/list");
    set(
        &mut row,
        &["headers", "mcp-protocol-version"],
        "\u{a0}2026-07-28\u{a0}".into(),
    );
    cases.push(named(row, "version-header-nbsp-is-not-ows"));
    cases
}
#[test]
fn entry_headers_and_rejection_bytes_match_original_http() {
    let reference = reference();
    let cached = observe(
        reference
            .required("cases")
            .unwrap()
            .as_array()
            .unwrap()
            .to_vec(),
        &reference,
    );
    let cases = header_cases();
    let source = original(&obj([
        ("mode", "baseline".into()),
        ("client", false.into()),
        ("cases", IvyJson::Array(cases.clone())),
    ]));
    save("entry-source", &source);
    let fresh = observe(cases, &source);
    save(
        "entry",
        &obj([
            ("cached", IvyJson::Array(cached)),
            ("source", source),
            ("fresh", IvyJson::Array(fresh)),
        ]),
    );
}
