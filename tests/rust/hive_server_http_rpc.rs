use super::*;
use base64::{Engine, engine::general_purpose::STANDARD};
use ivy_contracts::hash::digest;

fn chunk(bytes: impl AsRef<[u8]>) -> Value {
    json!({"base64":STANDARD.encode(bytes)})
}
fn post(id: &str, body: impl AsRef<[u8]>) -> Value {
    json!({"op":"start","id":id,"socket":"s","action":"httpRpc","method":"POST","url":"/ivy/api/v1/rpc","headers":{"authorization":"Bearer initial","content-type":"application/json"},"chunks":[chunk(body)],"response":{"wireBytes":true}})
}
fn one(name: &str, step: Value) -> Value {
    trace(name, vec![connect(), step])
}
fn final_snapshot(row: &Value) -> &Value {
    row["snapshots"].as_array().unwrap().last().unwrap()
}
fn body(row: &Value, id: &str) -> Value {
    let event = final_snapshot(row)["responses"][id]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["kind"] == "bytes")
        .unwrap();
    serde_json::from_slice(&STANDARD.decode(event["base64"].as_str().unwrap()).unwrap()).unwrap()
}
fn status(row: &Value, id: &str) -> u64 {
    final_snapshot(row)["responses"][id]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["kind"] == "head")
        .unwrap()["status"]
        .as_u64()
        .unwrap()
}
fn worker_actions(row: &Value) -> Vec<String> {
    row["snapshots"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|s| s["events"].as_array().unwrap())
        .filter(|e| e["kind"] == "worker")
        .map(|e| e["payload"]["action"].as_str().unwrap().to_owned())
        .collect()
}
fn error_code(row: &Value, id: &str) -> String {
    body(row, id)["error"]["data"]["code"]
        .as_str()
        .unwrap()
        .into()
}
fn framed(id: Value) -> String {
    let mut f = frame();
    f["id"] = id;
    serde_json::to_string(&f).unwrap()
}

#[test]
fn original_http_rpc_public_authentication_cookie_and_content_type_order() {
    let mut cases = vec![];
    let mut missing = post("rpc", b"{");
    missing["headers"] = json!({});
    missing["chunks"] = json!([{"gate":"must-not-read","bytes":1}]);
    cases.push(one("authentication-before-content-type-and-body", missing));
    let mut rejected = one(
        "worker-authentication-error-before-content-type",
        post("rpc", b"{"),
    );
    rejected["steps"][1]["headers"]["content-type"] = json!("invalid");
    rejected["replies"] = json!({"authenticate":[{"error":"unauthenticated"}]});
    cases.push(rejected);
    let mut session = post("rpc", b"{");
    session["headers"] = json!({"cookie":"ivy_session=session-token","origin":"https://example.test","content-type":"text/plain"});
    session["chunks"] = json!([{"gate":"must-not-read","bytes":1}]);
    cases.push(one(
        "session-cookie-refreshed-before-content-type-error",
        session.clone(),
    ));
    session["response"]["throwHeader"] = json!("Set-Cookie");
    cases.push(one("session-cookie-response-error-before-body", session));
    let mut origin = post("rpc", b"{}");
    origin["headers"] =
        json!({"cookie":"ivy_session=session-token","content-type":"application/json"});
    cases.push(one("session-origin-before-storage-and-content", origin));
    for (index, content_type) in [
        Value::Null,
        json!("Application/JSON"),
        json!("application/json "),
        json!(" text/json"),
        json!("application/json;charset=utf-8"),
    ]
    .into_iter()
    .enumerate()
    {
        let mut step = post("rpc", b"{");
        step["headers"]["content-type"] = content_type;
        cases.push(one(&format!("exact-content-type-{index}"), step));
    }
    let mut absent = post("rpc", b"{");
    absent["headers"]
        .as_object_mut()
        .unwrap()
        .remove("content-type");
    cases.push(one("absent-content-type", absent));
    for (name, method, path, authorization, bytes) in [
        (
            "reset-method-before-authentication",
            "GET",
            "/ivy/api/v1/rpc",
            None,
            framed(json!("get")),
        ),
        (
            "reset-path-before-authentication",
            "POST",
            "/ivy/api/v1/other",
            None,
            framed(json!("path")),
        ),
        (
            "reset-content-type-before-credential-guard",
            "POST",
            "/ivy/api/v1/rpc",
            Some("Bearer other"),
            "{".into(),
        ),
        (
            "reset-invocation-credential-error-retains-id",
            "POST",
            "/ivy/api/v1/rpc",
            Some("Bearer other"),
            framed(json!("guarded")),
        ),
        (
            "reset-bootstrap-credential-can-invoke",
            "POST",
            "/ivy/api/v1/rpc",
            Some("Bearer initial"),
            framed(json!("allowed")),
        ),
    ] {
        let mut step = post("rpc", bytes);
        step["method"] = json!(method);
        step["url"] = json!(path);
        if let Some(value) = authorization {
            step["headers"]["authorization"] = json!(value);
        } else {
            step["headers"]
                .as_object_mut()
                .unwrap()
                .remove("authorization");
        }
        if name == "reset-content-type-before-credential-guard" {
            step["headers"]["content-type"] = json!("text/plain");
        }
        let mut input = one(name, step);
        input["options"]["resetBootstrapCredentialDigest"] = json!(digest("initial"));
        cases.push(input);
    }
    let mut get = post("rpc", b"{");
    get["method"] = json!("GET");
    get["headers"] = json!({"cookie":"ivy_session=session-token","content-type":"invalid"});
    get["chunks"] = json!([{"gate":"must-not-read","bytes":1}]);
    cases.push(one(
        "non-post-authenticates-and-refreshes-before-unmatched-route",
        get,
    ));
    let mut other = post("rpc", b"{");
    other["url"] = json!("/ivy/api/v1/other");
    other["chunks"] = json!([{"gate":"must-not-read","bytes":1}]);
    cases.push(one("unmatched-path-does-not-consume-body", other));
    let mut empty = one(
        "empty-reset-option-preserves-public-authentication",
        post("rpc", framed(json!("empty-reset"))),
    );
    empty["options"]["resetBootstrapCredentialDigest"] = json!("");
    cases.push(empty);
    let rows = compare("http-rpc-prefix", cases);
    assert_eq!(error_code(&rows[0], "rpc"), "unauthenticated");
    assert!(worker_actions(&rows[0]).is_empty());
    assert_eq!(worker_actions(&rows[1]), vec!["authenticate"]);
    let session_events = final_snapshot(&rows[2])["responses"]["rpc"]
        .as_array()
        .unwrap();
    assert_eq!(session_events[5]["name"], "Set-Cookie");
    assert_eq!(status(&rows[2], "rpc"), 400);
    assert_eq!(worker_actions(&rows[4]), Vec::<String>::new());
    assert_eq!(error_code(&rows[9], "rpc"), "invalid_frame");
    assert_eq!(worker_actions(&rows[11]), Vec::<String>::new());
    assert_eq!(body(&rows[14], "rpc")["id"], "guarded");
    assert_eq!(error_code(&rows[14], "rpc"), "maintenance_active");
    assert_eq!(status(&rows[15], "rpc"), 200);
    assert_eq!(status(&rows[16], "rpc"), 404);
}

#[test]
fn original_http_rpc_single_parse_correlation_and_actual_invocation() {
    let mut cases = vec![];
    for (name, bytes) in [
        ("empty", vec![]),
        ("malformed", b"{".to_vec()),
        ("malformed-utf8", vec![255]),
        ("null", b"null".to_vec()),
        ("number", b"1".to_vec()),
        ("boolean", b"true".to_vec()),
        ("string", b"\"request\"".to_vec()),
    ] {
        cases.push(one(name, post("rpc", bytes)));
    }
    for (index, id) in [
        json!("one"),
        json!(0),
        json!(""),
        json!(-1),
        json!({"nested":"id"}),
        json!("😀".repeat(128)),
        json!("😀".repeat(129)),
    ]
    .into_iter()
    .enumerate()
    {
        cases.push(one(
            &format!("correlation-{index}"),
            post("rpc", framed(id)),
        ));
    }
    cases.push(one(
        "single-validation-remains-the-worker-responsibility",
        post(
            "rpc",
            br#"{"id":"unchecked","method":"fixture.not-validated"}"#,
        ),
    ));
    let mut unicode = frame();
    unicode["params"]["text"] = json!("é😀");
    let text = serde_json::to_vec(&unicode).unwrap();
    let split = text.iter().position(|v| *v == 0xc3).unwrap() + 1;
    let mut step = post("rpc", []);
    step["chunks"] = json!([chunk(&text[..split]), chunk(&text[split..])]);
    step["headers"]["content-length"] = json!("0");
    cases.push(one("utf8-split-across-chunks-and-actual-byte-charge", step));
    let mut bom = vec![239, 187, 191];
    bom.extend_from_slice(framed(json!("bom")).as_bytes());
    cases.push(one("one-leading-bom", post("rpc", bom.clone())));
    bom.splice(0..0, [239, 187, 191]);
    cases.push(one("second-bom-is-malformed-json", post("rpc", bom)));
    let mut worker = one(
        "worker-failure-keeps-id",
        post("rpc", framed(json!("worker-error"))),
    );
    worker["replies"] =
        json!({"execute":[{"error":"forbidden","message":"Isolated operation refusal."}]});
    cases.push(worker);
    cases.push(one("direct-http-subscription-failure-keeps-id",post("rpc",br#"{"jsonrpc":"2.0","id":"subscription","method":"notifications.subscribe","params":{"filters":[],"changes":[]}}"#)));
    let mut stream = post("rpc", []);
    stream["chunks"] = json!([chunk(b"{"),{"error":"storage_unavailable"}]);
    cases.push(one("stream-failure-before-correlation", stream));
    let mut declared = post("rpc", []);
    declared["headers"]["content-length"] = json!("33554433");
    declared["chunks"] = json!([{"gate":"must-not-read","bytes":1}]);
    cases.push(one("declared-body-limit-before-stream", declared));
    let mut ended = post("rpc", framed(json!("ended")));
    ended["response"]["ended"] = json!(true);
    cases.push(one("ended-single-response-still-invokes", ended));
    let rows = compare("http-rpc-single", cases);
    for row in &rows[..7] {
        assert_eq!(error_code(row, "rpc"), "invalid_frame");
        assert!(body(row, "rpc")["id"].is_null());
        assert_eq!(worker_actions(row), vec!["authenticate"]);
    }
    assert_eq!(body(&rows[7], "rpc")["id"], "one");
    assert_eq!(body(&rows[8], "rpc")["id"], 0);
    assert_eq!(status(&rows[14], "rpc"), 200);
    let events = final_snapshot(&rows[15])["events"].as_array().unwrap();
    let execute = events
        .iter()
        .find(|v| v["payload"]["action"] == "execute")
        .unwrap();
    assert_eq!(
        execute["options"]["reservedBytes"].as_f64(),
        Some(text.len() as f64)
    );
    assert_eq!(body(&rows[18], "rpc")["id"], "worker-error");
    assert_eq!(status(&rows[18], "rpc"), 403);
    assert_eq!(body(&rows[19], "rpc")["id"], "subscription");
    assert_eq!(worker_actions(&rows[19]), vec!["authenticate"]);
    assert_eq!(error_code(&rows[20], "rpc"), "storage_unavailable");
    assert_eq!(error_code(&rows[21], "rpc"), "frame_too_large");
    assert_eq!(worker_actions(&rows[22]), vec!["authenticate", "execute"]);
    assert_eq!(
        final_snapshot(&rows[22])["responses"]["rpc"]
            .as_array()
            .unwrap()
            .len(),
        5
    );
}

#[test]
fn original_http_rpc_batch_limits_order_release_and_response_binding() {
    let a = framed(json!("a"));
    let b = framed(json!("b"));
    let bytes = format!("[ {a},\n{b} ]");
    let mut pending = trace(
        "batch-dispatch-serialization-keeps-queued-and-outer-reservations",
        vec![
            connect(),
            post("rpc", bytes),
            json!({"op":"snapshot","name":"both-invocations"}),
            json!({"op":"probe","id":"full","bytes":0}),
            json!({"op":"close","socket":"s"}),
            json!({"op":"release","gate":"a"}),
            json!({"op":"probe","id":"one-item-released","bytes":0}),
            json!({"op":"snapshot","name":"first-done-second-started-no-response"}),
            json!({"op":"release","gate":"b"}),
            json!({"op":"probe","id":"all-released","bytes":100000}),
        ],
    );
    pending["budget"] = json!([3, 100000, 3, 100000]);
    pending["replies"] = json!({"execute":[{"gate":"a","value":{"kind":"result","value":"first"}},{"gate":"b","value":{"kind":"result","value":"second"}}]});
    let mut cases = vec![pending];
    for (name, bytes) in [
        ("empty-batch", "[]".into()),
        ("duplicate-batch-identities", format!("[{a},{a}]")),
        (
            "batch-count-limit",
            format!(
                "[{}]",
                (0..9)
                    .map(|i| framed(json!(i)))
                    .collect::<Vec<_>>()
                    .join(",")
            ),
        ),
        (
            "invalid-null-identities-have-per-item-failures",
            "[null,{},3]".into(),
        ),
        (
            "mixed-valid-and-direct-and-envelope-failures",
            format!(
                "[{a},null,{{\"jsonrpc\":\"2.0\",\"id\":\"http-only\",\"method\":\"notifications.subscribe\",\"params\":{{\"filters\":[],\"changes\":[]}}}}]"
            ),
        ),
    ] {
        cases.push(one(name, post("rpc", bytes)));
    }
    for size in [65536, 65537] {
        let mut bytes = format!("[{a}]");
        bytes.push_str(&" ".repeat(size - bytes.len()));
        cases.push(one(
            &format!("batch-received-byte-boundary-{size}"),
            post("rpc", bytes),
        ));
    }
    for flag in ["ended", "sent", "destroyed"] {
        let mut step = post("rpc", format!("[{a}]"));
        step["response"][flag] = json!(true);
        cases.push(one(&format!("batch-direct-response-despite-{flag}"), step));
    }
    let mut bootstrap = one(
        "batch-bootstrap-refusals-are-per-item-http-200",
        post("rpc", format!("[{a},{b}]")),
    );
    bootstrap["options"]["resetBootstrapCredentialDigest"] = json!(digest("other"));
    cases.push(bootstrap);
    // Direct catalogue calls bypass serialized storage dispatch in both owners.
    let mut direct=trace("direct-batch-reverse-completion-keeps-input-order",vec![
        connect(),post("rpc",serde_json::to_vec(&json!([
            {"jsonrpc":"2.0","id":"a","method":"packages.catalog","params":{"after":0,"includeUis":true}},
            {"jsonrpc":"2.0","id":"b","method":"packages.catalog","params":{"after":1,"includeUis":true}}
        ])).unwrap()),json!({"op":"snapshot","name":"both-direct-calls"}),
        json!({"op":"release","gate":"direct-b"}),json!({"op":"snapshot","name":"second-direct-done-no-response"}),json!({"op":"release","gate":"direct-a"})
    ]);
    direct["options"]["packageRoot"] = json!("private-packages");
    direct["replies"] = json!({"package.catalog":[{"gate":"direct-a","value":{"marker":"first"}},{"gate":"direct-b","value":{"marker":"second"}}]});
    cases.push(direct);
    let rows = compare("http-rpc-batch", cases);
    assert_eq!(rows[0]["snapshots"][0]["buffered"].as_f64(), Some(0.0));
    assert_eq!(
        rows[0]["snapshots"][1]["results"]["full"]["error"]["data"]["code"],
        "limit_exceeded"
    );
    assert_eq!(
        rows[0]["snapshots"][1]["results"]["one-item-released"]["ok"],
        true
    );
    assert_eq!(
        rows[0]["snapshots"][1]["responses"]["rpc"]
            .as_array()
            .unwrap()
            .len(),
        5
    );
    let replies = body(&rows[0], "rpc");
    assert_eq!(replies[0]["id"], "a");
    assert_eq!(replies[0]["result"], "first");
    assert_eq!(replies[1]["result"], "second");
    for index in [1, 2, 3, 7] {
        assert_ne!(status(&rows[index], "rpc"), 200);
        assert_eq!(worker_actions(&rows[index]), vec!["authenticate"]);
    }
    assert_eq!(status(&rows[6], "rpc"), 200);
    for row in &rows[8..11] {
        assert_eq!(status(row, "rpc"), 200);
        assert!(body(row, "rpc").is_array());
    }
    assert_eq!(status(&rows[11], "rpc"), 200);
    assert_eq!(
        body(&rows[11], "rpc")[0]["error"]["data"]["code"],
        "maintenance_active"
    );
    let direct = body(&rows[12], "rpc");
    assert_eq!(direct[0]["result"]["marker"], "first");
    assert_eq!(direct[1]["result"]["marker"], "second");
    assert_eq!(
        rows[12]["snapshots"][1]["responses"]["rpc"]
            .as_array()
            .unwrap()
            .len(),
        5
    );
}
