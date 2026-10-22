use super::*;
use base64::{Engine, engine::general_purpose::STANDARD};
use ivy_hive::mcp_protocol::{self as codec, Encoding, Reply};

pub(super) fn oracle(input: &IvyJson) -> IvyJson {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let modules = std::env::var_os("IVY_MCP_FIXTURE_NODE_MODULES")
        .unwrap_or_else(|| root.join("node_modules").into_os_string());
    let mut child =
        Command::new(std::env::var_os("IVY_TEST_NODE").unwrap_or_else(|| "node".into()))
            .current_dir(root)
            .env("IVY_MCP_FIXTURE_NODE_MODULES", modules)
            .args([
                "--disable-warning=ExperimentalWarning",
                "tests/fixtures/hive-mcp-http-oracle.mjs",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(encode(input).as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "Original MCP fixture failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    json(&String::from_utf8(output.stdout).unwrap())
}
fn save(suffix: &str, value: &IvyJson) {
    if let Some(prefix) = std::env::var_os("IVY_HIVE_MCP_FRAMING_CORPUS") {
        std::fs::write(
            format!("{}-{suffix}.json", prefix.to_string_lossy()),
            encode(value),
        )
        .unwrap();
    }
}
fn row(name: impl Into<JsString>, body: IvyJson) -> IvyJson {
    obj([("name", IvyJson::String(name.into())), ("body", body)])
}
fn frame(id: IvyJson) -> IvyJson {
    obj([
        ("jsonrpc", "2.0".into()),
        ("id", id),
        ("method", "ping".into()),
    ])
}
#[test]
fn message_shapes_match_pinned_sdk() {
    let mut cases = Vec::new();
    for (index, id) in [
        number(0),
        IvyJson::Number(-0.0),
        number(-1),
        IvyJson::Number(1.25),
        IvyJson::Null,
        true.into(),
        "".into(),
        IvyJson::String(JsString(vec![0xd800])),
        IvyJson::Number(9_007_199_254_740_991.0),
        IvyJson::Number(-9_007_199_254_740_991.0),
        IvyJson::Number(9_007_199_254_740_992.0),
        IvyJson::Number(1e20),
    ]
    .into_iter()
    .enumerate()
    {
        cases.push(row(format!("id-{index}"), frame(id)));
    }
    for (index, value) in [
        json("{}"),
        IvyJson::Null,
        json("[]"),
        "value".into(),
        number(1),
        true.into(),
    ]
    .into_iter()
    .enumerate()
    {
        let mut request = frame("request".into());
        request.insert("params", value.clone());
        cases.push(row(format!("params-{index}"), request));
        let mut request = frame("request".into());
        request.insert("params", obj([("_meta", value.clone())]));
        cases.push(row(format!("meta-{index}"), request));
        cases.push(row(
            format!("result-{index}"),
            obj([
                ("jsonrpc", "2.0".into()),
                ("id", "request".into()),
                ("result", value),
            ]),
        ));
    }
    for (index, value) in [
        "progress".into(),
        number(0),
        IvyJson::Number(1.25),
        true.into(),
        IvyJson::Null,
        IvyJson::String(JsString(vec![0xdfff])),
        IvyJson::Number(9_007_199_254_740_992.0),
    ]
    .into_iter()
    .enumerate()
    {
        let mut request = frame("request".into());
        request.insert(
            "params",
            obj([("_meta", obj([("progressToken", value.clone())]))]),
        );
        cases.push(row(format!("progress-{index}"), request));
        let mut notification = obj([
            ("jsonrpc", "2.0".into()),
            ("method", "notifications/fixture".into()),
        ]);
        notification.insert(
            "params",
            obj([("_meta", obj([("progressToken", value.clone())]))]),
        );
        cases.push(row(format!("notification-progress-{index}"), notification));
        cases.push(row(
            format!("result-progress-{index}"),
            obj([
                ("jsonrpc", "2.0".into()),
                ("id", "request".into()),
                ("result", obj([("_meta", obj([("progressToken", value)]))])),
            ]),
        ));
    }
    for (index, value) in [
        json("{}"),
        json(r#"{"taskId":"task","extra":true}"#),
        json(r#"{"taskId":1}"#),
        IvyJson::Null,
        json("[]"),
    ]
    .into_iter()
    .enumerate()
    {
        let mut request = frame("request".into());
        request.insert(
            "params",
            obj([(
                "_meta",
                obj([("io.modelcontextprotocol/related-task", value)]),
            )]),
        );
        cases.push(row(format!("task-meta-{index}"), request));
    }
    for (index, body) in [
        json("{}"), json(r#"{"jsonrpc":"1.0","id":1,"method":"ping"}"#),
        json(r#"{"id":1,"method":"ping"}"#), json(r#"{"jsonrpc":"2.0","method":""}"#),
        json(r#"{"jsonrpc":"2.0","method":"ping","id":null}"#),
        json(r#"{"jsonrpc":"2.0","method":"ping","extra":true}"#),
        json(r#"{"jsonrpc":"2.0","id":1,"method":"ping","extra":true}"#),
        json(r#"{"jsonrpc":"2.0","id":1,"method":1}"#),
        json(r#"{"jsonrpc":"2.0","id":1,"result":{"_meta":{"io.modelcontextprotocol/serverInfo":null}}}"#),
        json(r#"{"jsonrpc":"2.0","id":1,"result":{"_meta":null}}"#),
        json(r#"{"jsonrpc":"2.0","id":1,"result":{},"extra":true}"#),
        json(r#"{"jsonrpc":"2.0","error":{"code":-1,"message":"failure","extra":true}}"#),
        json(r#"{"jsonrpc":"2.0","id":null,"error":{"code":-1,"message":"failure"}}"#),
        json(r#"{"jsonrpc":"2.0","id":1,"error":{"code":1.25,"message":"failure"}}"#),
        json(r#"{"jsonrpc":"2.0","id":1,"error":{"code":-1,"message":"\ud800","data":null}}"#),
        json(r#"{"jsonrpc":"2.0","id":1,"error":{"code":-1,"message":null}}"#),
        json(r#"{"jsonrpc":"2.0","id":1,"error":{"code":-1,"message":"failure"},"extra":true}"#),
        IvyJson::Null, json("[]"), json(r#"{"jsonrpc":"2.0","method":"ping","params":{"extra":{"nested":[null,true]}}}"#),
    ].into_iter().enumerate() { cases.push(row(format!("shape-{index}"), body)); }
    let original = oracle(&obj([
        ("mode", "message-shapes".into()),
        ("cases", IvyJson::Array(cases.clone())),
    ]));
    let expected = original.required("shapes").unwrap().as_array().unwrap();
    let native: Vec<_> = cases
        .iter()
        .map(|input| {
            obj([
                ("name", input.required("name").unwrap().clone()),
                (
                    "kind",
                    codec::message_kind(input.required("body").unwrap())
                        .map(|k| k.name().into())
                        .unwrap_or(IvyJson::Null),
                ),
            ])
        })
        .collect();
    save(
        "shapes",
        &obj([
            ("cases", IvyJson::Array(cases)),
            ("original", original.clone()),
            ("native", IvyJson::Array(native.clone())),
        ]),
    );
    assert_eq!(
        &native, expected,
        "Strict JSON-RPC shapes differ from pinned SDK"
    );
}
fn reply_value(reply: Reply) -> IvyJson {
    obj([
        ("status", number(reply.status.into())),
        ("headers", reply.headers),
        ("bodyBase64", STANDARD.encode(reply.body.as_bytes()).into()),
    ])
}
#[test]
fn framing_and_rejection_correlation_match_original_http() {
    let original = if let Some(path) = std::env::var_os("IVY_HIVE_MCP_HTTP_REFERENCE") {
        json(&std::fs::read_to_string(path).unwrap())
    } else {
        oracle(&obj([
            ("mode", "baseline".into()),
            ("client", false.into()),
        ]))
    };
    let cases = original.required("cases").unwrap().as_array().unwrap();
    let wire = original.required("wire").unwrap().as_array().unwrap();
    assert_eq!(cases.len(), wire.len());
    let mut comparisons = Vec::new();
    for (input, observed) in cases.iter().zip(wire) {
        if input.get("method").is_some_and(|v| v != &"POST".into()) {
            continue;
        }
        let body = input.required("body").unwrap();
        let bytes = STANDARD.decode(text(observed, "bodyBase64")).unwrap();
        let raw = String::from_utf8(bytes).unwrap();
        let native = if codec::message_kind(body).is_none() {
            Reply::invalid_message(body).unwrap()
        } else if raw.is_empty() {
            Reply::accepted()
        } else {
            let sse = observed.required("headers").unwrap().get("content-type")
                == Some(&"text/event-stream".into());
            let message = json(if sse {
                raw.strip_prefix("event: message\ndata: ")
                    .unwrap()
                    .strip_suffix("\n\n")
                    .unwrap()
            } else {
                &raw
            });
            let id = if text(input, "name").starts_with("accept-") {
                IvyJson::Null
            } else {
                codec::response_id(body)
            };
            let encoding = if sse { Encoding::Sse } else { Encoding::Json };
            if let Some(result) = message.get("result") {
                Reply::success(encoding, id, result.clone()).unwrap()
            } else {
                let status = observed.required("status").unwrap().as_f64().unwrap() as u16;
                // The original response identifies the already chosen failure
                // stage; error payload/status are controlled in this codec test.
                let IvyJson::Object(fields) = &message else {
                    panic!("RPC object")
                };
                if fields[1].0 == JsString::from("error") {
                    Reply::transport_error(status, id, message.required("error").unwrap().clone())
                        .unwrap()
                } else {
                    Reply::protocol_error(
                        status,
                        encoding,
                        id,
                        message.required("error").unwrap().clone(),
                    )
                    .unwrap()
                }
            }
        };
        let expected = obj([
            ("status", observed.required("status").unwrap().clone()),
            ("headers", observed.required("headers").unwrap().clone()),
            (
                "bodyBase64",
                observed.required("bodyBase64").unwrap().clone(),
            ),
        ]);
        let actual = reply_value(native);
        comparisons.push(obj([
            ("name", input.required("name").unwrap().clone()),
            ("input", input.clone()),
            ("original", expected.clone()),
            ("native", actual.clone()),
        ]));
        assert_eq!(
            actual,
            expected,
            "MCP response bytes differ for {}",
            text(input, "name")
        );
    }
    save("wire", &IvyJson::Array(comparisons));
}
