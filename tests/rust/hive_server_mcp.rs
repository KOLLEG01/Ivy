use super::*;
use base64::{Engine, engine::general_purpose::STANDARD};
use ivy_hive::{mcp_request, server_mcp as endpoint};

// These composition cases intentionally use empty catalogues. Language policy
// has separate existing tests; no substitute runtime is being qualified here.
struct Language;
impl ivy_hive::retention::Runtime for Language {
    fn parse_date(&self, _: &JsString) -> f64 {
        panic!("No date parsing")
    }
    fn iso_date(&self, _: f64) -> IvyResult<JsString> {
        panic!("No date rendering")
    }
    fn compare_contract_keys(&self, _: &JsString, _: &JsString) -> std::cmp::Ordering {
        panic!("No contract sorting")
    }
}
impl ivy_hive::discovery::Runtime for Language {
    fn lowercase(&self, _: &JsString) -> IvyResult<JsString> {
        panic!("No case folding")
    }
    fn word_character(&self, _: u32) -> bool {
        panic!("No word matching")
    }
    fn compare_english(&self, _: &JsString, _: &JsString) -> std::cmp::Ordering {
        panic!("No catalogue sorting")
    }
}
impl ivy_hive::mcp::Runtime for Language {
    fn uppercase(&self, _: &JsString) -> IvyResult<JsString> {
        panic!("No case folding")
    }
}
pub(super) fn outer_error(error: mcp_request::Error) -> IvyError {
    match error {
        mcp_request::Error::Ivy(error) => error,
        mcp_request::Error::Protocol(_) => IvyError::internal(),
    }
}
pub(super) struct Protocol {
    pub spec: Value,
    pub log: Log,
    pub controls: Controls,
}
impl endpoint::Factory for Protocol {
    type Handler = Self;
    fn create(&mut self, app: &endpoint::Application) -> mcp_request::Result<Self> {
        self.log.lock().unwrap().push(json!({"kind":"mcp-create","info":app.info.to_serde().unwrap(),"configuration":app.configuration().to_serde().unwrap(),"options":app.protocol_options().to_serde().unwrap()}));
        if self.spec["createError"]["error"].is_string() {
            return Err(failure(&self.spec["createError"]).into());
        }
        Ok(Self {
            spec: self.spec.clone(),
            log: self.log.clone(),
            controls: self.controls.clone(),
        })
    }
}
impl endpoint::Handler for Protocol {
    async fn handle(
        &mut self,
        app: &endpoint::Application,
        response: &mut impl http_response::Response,
    ) -> mcp_request::Result<()> {
        self.log.lock().unwrap().push(json!({"kind":"mcp-handle"}));
        if let Some(gate) = self.spec["handleGate"].as_str() {
            pause(&self.controls, gate).await;
        }
        if self.spec["handleError"]["error"].is_string() {
            return Err(failure(&self.spec["handleError"]).into());
        }
        let params = app.body.get("params");
        let result = match app
            .body
            .get("method")
            .and_then(|v| v.as_str().ok())
            .as_deref()
        {
            Some("tools/list") => app
                .list_tools(
                    params
                        .and_then(|v| v.get("cursor"))
                        .and_then(IvyJson::as_js_string),
                    &Language,
                )
                .await
                .map_err(mcp_request::Error::Protocol),
            Some("tools/call") => {
                app.call_tool(
                    params.unwrap().get("name").unwrap().as_js_string().unwrap(),
                    params.unwrap().get("arguments"),
                    &Language,
                )
                .await
            }
            _ => Ok(obj([
                ("serverInfo", app.info.clone()),
                ("configuration", app.configuration()),
            ])),
        };
        let result = match result {
            Ok(value) => value,
            Err(mcp_request::Error::Protocol(error)) => obj([(
                "protocolError",
                obj([
                    ("code", IvyJson::Number(error.code as f64)),
                    ("message", IvyJson::String(error.message)),
                    ("data", error.data),
                ]),
            )]),
            Err(error) => return Err(error),
        };
        if !response.writable_ended() {
            response.write_head(200, obj([("Content-Type", "application/json".into())]))?;
            response.end(ivy_contracts::json::stringify(&result, usize::MAX)?)?;
        }
        Ok(())
    }
    async fn close(&mut self) -> mcp_request::Result<()> {
        self.log.lock().unwrap().push(json!({"kind":"mcp-close"}));
        if let Some(gate) = self.spec["closeGate"].as_str() {
            pause(&self.controls, gate).await;
        }
        if self.spec["closeError"]["error"].is_string() {
            return Err(failure(&self.spec["closeError"]).into());
        }
        Ok(())
    }
}
fn frame(method: &str) -> Value {
    json!({"jsonrpc":"2.0","id":"mcp","method":method,"params":{}})
}
fn post(id: &str, body: impl AsRef<[u8]>) -> Value {
    json!({"op":"start","id":id,"socket":"s","action":"httpMcp","method":"POST","url":"/ivy/mcp","headers":{"authorization":"Bearer initial","content-type":"application/json"},"chunks":[{"base64":STANDARD.encode(body)}],"response":{"wireBytes":true}})
}
fn request(id: &str, method: &str) -> Value {
    post(id, serde_json::to_vec(&frame(method)).unwrap())
}
fn one(name: &str, step: Value) -> Value {
    trace(name, vec![connect(), step])
}
fn last(row: &Value) -> &Value {
    row["snapshots"].as_array().unwrap().last().unwrap()
}
fn events(row: &Value) -> Vec<&Value> {
    row["snapshots"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|v| v["events"].as_array().unwrap())
        .collect()
}
fn actions(row: &Value) -> Vec<&str> {
    events(row)
        .iter()
        .filter_map(|v| v["payload"]["action"].as_str())
        .collect()
}
fn body(row: &Value, id: &str) -> Value {
    let value = last(row)["responses"][id]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["kind"] == "bytes")
        .unwrap();
    serde_json::from_slice(&STANDARD.decode(value["base64"].as_str().unwrap()).unwrap()).unwrap()
}
fn code(row: &Value, id: &str) -> String {
    body(row, id)["error"]["data"]["code"]
        .as_str()
        .unwrap()
        .into()
}
fn kinds(row: &Value) -> Vec<&str> {
    events(row)
        .iter()
        .map(|v| v["kind"].as_str().unwrap())
        .collect()
}

#[test]
fn original_mcp_http_authentication_resource_origin_cookie_and_method_order() {
    let mut cases = vec![];
    for (index, origin) in [
        None,
        Some(json!("https://example.test")),
        Some(json!("https://EXAMPLE.test:443")),
        Some(json!("null")),
        Some(json!("https://other.test")),
    ]
    .into_iter()
    .enumerate()
    {
        let mut step = request("mcp", "tools/list");
        if let Some(origin) = origin {
            step["headers"]["origin"] = origin;
        }
        cases.push(one(&format!("mcp-origin-{index}"), step));
    }
    for method in ["GET", "DELETE"] {
        let mut step = request("mcp", "tools/list");
        step["method"] = json!(method);
        step["headers"] = json!({"cookie":"ivy_session=session-token","origin":"https://example.test","content-type":"invalid"});
        step["chunks"] = json!([{"gate":"must-not-read","bytes":1}]);
        cases.push(one(
            &format!("authenticated-cookie-before-405-{method}"),
            step,
        ));
    }
    let mut missing = request("mcp", "tools/list");
    missing["headers"] = json!({"origin":"https://other.test"});
    cases.push(one("missing-auth-before-origin-and-content", missing));
    let mut cookie = request("mcp", "tools/list");
    cookie["headers"] = json!({"cookie":"ivy_session=session-token","origin":"https://other.test","content-type":"invalid"});
    cases.push(one("cookie-post-origin-before-storage", cookie));
    let mut origin = request("mcp", "tools/list");
    origin["headers"] = json!({"cookie":"ivy_session=session-token","origin":"https://other.test"});
    origin["method"] = json!("GET");
    cases.push(one(
        "cookie-read-auth-before-mcp-origin-and-refresh",
        origin,
    ));
    let mut reset = one(
        "reset-blocks-before-authentication",
        request("mcp", "tools/list"),
    );
    reset["options"]["resetBootstrapCredentialDigest"] = json!("reset");
    cases.push(reset);
    let mut rejected = one(
        "auth-rejection-before-invalid-content",
        request("mcp", "tools/list"),
    );
    rejected["replies"] = json!({"authenticate":[{"error":"unauthenticated"}]});
    cases.push(rejected);
    let mut cache = trace(
        "resource-identity-cache-separated-and-cleared-on-close",
        vec![
            connect(),
            request("a", "tools/list"),
            request("b", "tools/list"),
            request("c", "tools/list"),
            json!({"op":"close","socket":"s"}),
            request("d", "tools/list"),
        ],
    );
    cache["steps"][3]["url"] = json!("/ivy/mcp-dev");
    cases.push(cache);
    let mut header = request("mcp", "tools/list");
    header["headers"] = json!({"cookie":"ivy_session=session-token","origin":"https://example.test","content-type":"invalid"});
    header["response"]["throwHeader"] = json!("Set-Cookie");
    cases.push(one("cookie-response-failure-before-content", header));
    let rows = compare("http-mcp-prefix", cases);
    assert_eq!(body(&rows[0], "mcp")["tools"], json!([]));
    assert_eq!(actions(&rows[1]), vec!["authenticate", "mcp.catalog"]);
    assert_eq!(body(&rows[2], "mcp")["tools"], json!([]));
    for row in &rows[3..5] {
        assert_eq!(code(row, "mcp"), "forbidden");
        assert_eq!(actions(row), vec!["authenticate"]);
    }
    for row in &rows[5..7] {
        assert_eq!(body(row, "mcp")["error"], "method_not_allowed");
        assert!(!kinds(row).contains(&"mcp-create"));
    }
    assert!(actions(&rows[7]).is_empty());
    assert!(actions(&rows[8]).is_empty());
    assert_eq!(actions(&rows[9]), vec!["session.resolve"]);
    assert_eq!(code(&rows[10], "mcp"), "maintenance_active");
    assert_eq!(
        actions(&rows[12])
            .iter()
            .filter(|v| **v == "authenticate")
            .count(),
        3
    );
    let infos = events(&rows[12])
        .into_iter()
        .filter(|v| v["kind"] == "mcp-create")
        .map(|v| v["info"]["name"].clone())
        .collect::<Vec<_>>();
    assert_eq!(
        infos,
        vec![json!("ivy"), json!("ivy"), json!("ivy_dev"), json!("ivy")]
    );
}

#[test]
fn original_mcp_body_instructions_catalogue_cursor_and_observation_binding() {
    let mut cases = vec![];
    for (name, bytes) in [
        ("empty", vec![]),
        ("malformed", b"{".to_vec()),
        ("utf8", vec![255]),
        ("null", b"null".to_vec()),
        ("array", b"[]".to_vec()),
        ("string", b"\"mcp\"".to_vec()),
    ] {
        cases.push(one(name, post("mcp", bytes)));
    }
    let mut content = request("mcp", "tools/list");
    content["headers"]["content-type"] = json!("text/plain");
    content["chunks"] = json!([{"gate":"must-not-read","bytes":1}]);
    cases.push(one("content-type-before-read", content));
    for surface in ["mcp", "mcp-dev"] {
        for method in ["initialize", "server/discover"] {
            let mut step = request("mcp", method);
            step["url"] = json!(format!("/ivy/{surface}"));
            let mut input = one(&format!("{surface}-{method}-instructions"), step);
            input["replies"] = json!({"execute":[{"value":{"kind":"result","value":{"instructions":"Authorized dynamic instructions."}}}]});
            cases.push(input);
        }
    }
    for (name, reply) in [
        ("valid-cursor", json!({"value":{"offset":0}})),
        ("past-end-cursor", json!({"value":{"offset":1}})),
        ("worker-cursor-error", json!({"error":"invalid_cursor"})),
    ] {
        let mut f = frame("tools/list");
        f["params"]["cursor"] = json!("opaque");
        let mut input = one(name, post("mcp", serde_json::to_vec(&f).unwrap()));
        input["replies"] = json!({"mcp.cursor.read":[reply]});
        cases.push(input);
    }
    let mut catalog = one(
        "catalogue-failure-projected-by-real-handler",
        request("mcp", "tools/list"),
    );
    catalog["replies"] = json!({"mcp.catalog":[{"error":"storage_unavailable"}]});
    cases.push(catalog);
    for (index, id) in [json!(-1), json!("😀".repeat(80)), json!(null)]
        .into_iter()
        .enumerate()
    {
        let mut f = frame("tools/call");
        f["id"] = id;
        f["params"] =
            json!({"name":"unknown","arguments":{"input":{"operationId":"o".repeat(600)}}});
        let mut input = one(
            &format!("unknown-tool-observation-{index}"),
            post("mcp", serde_json::to_vec(&f).unwrap()),
        );
        input["observe"] = json!(true);
        cases.push(input);
    }
    let mut f = frame("tools/call");
    f["params"] = json!({"name":"","arguments":{}});
    cases.push(one(
        "absent-observer-and-empty-name-omitted",
        post("mcp", serde_json::to_vec(&f).unwrap()),
    ));
    let mut observed = one(
        "observer-failure-then-close-and-correlated-outer-error",
        post("mcp", serde_json::to_vec(&f).unwrap()),
    );
    observed["observe"] = json!(true);
    observed["observerError"] = json!({"error":"forbidden"});
    cases.push(observed);
    let mut dynamic = one(
        "dynamic-instructions-failure-before-factory",
        request("mcp", "initialize"),
    );
    dynamic["replies"] = json!({"execute":[{"error":"forbidden"}]});
    cases.push(dynamic);
    let mut length = request("mcp", "tools/list");
    length["headers"]["content-length"] = json!("33554433");
    length["chunks"] = json!([{"gate":"must-not-read","bytes":1}]);
    cases.push(one("declared-limit-before-read", length));
    let rows = compare("http-mcp-application", cases);
    for row in &rows[..6] {
        assert_eq!(code(row, "mcp"), "invalid_frame");
        assert!(!kinds(row).contains(&"mcp-create"));
    }
    for (row, method) in rows[7..9].iter().zip(["initialize", "server/discover"]) {
        assert_eq!(actions(row), vec!["authenticate", "execute"]);
        assert_eq!(
            body(row, "mcp")["configuration"]["instructions"],
            "Authorized dynamic instructions."
        );
        let execute = events(row)
            .into_iter()
            .find(|v| v["payload"]["action"] == "execute")
            .unwrap();
        assert_eq!(
            execute["options"]["reservedBytes"].as_f64(),
            Some(serde_json::to_vec(&frame(method)).unwrap().len() as f64)
        );
    }
    for row in &rows[9..11] {
        assert_eq!(actions(row), vec!["authenticate"]);
    }
    assert_eq!(body(&rows[11], "mcp")["tools"], json!([]));
    assert_eq!(
        body(&rows[12], "mcp")["protocolError"]["data"]["code"],
        "invalid_cursor"
    );
    assert_eq!(body(&rows[15], "mcp")["protocolError"]["code"], -32601);
    let observation = events(&rows[15])
        .into_iter()
        .find(|v| v["kind"] == "mcp-observe")
        .unwrap();
    assert_eq!(observation["value"]["requestId"].as_f64(), Some(-1.0));
    assert_eq!(
        observation["value"]["operationIdentity"]
            .as_str()
            .unwrap()
            .len(),
        512
    );
    assert!(!kinds(&rows[18]).contains(&"mcp-observe"));
    assert_eq!(code(&rows[19], "mcp"), "forbidden");
    assert!(kinds(&rows[19]).contains(&"mcp-close"));
    assert!(!kinds(&rows[20]).contains(&"mcp-create"));
    assert_eq!(code(&rows[21], "mcp"), "frame_too_large");
}

#[test]
fn original_mcp_factory_handler_finally_and_retained_request_lease() {
    let mut cases = vec![];
    for (name, control) in [
        (
            "factory-failure",
            json!({"createError":{"error":"forbidden"}}),
        ),
        (
            "handler-failure-close",
            json!({"handleError":{"error":"forbidden"}}),
        ),
        (
            "close-failure-after-success",
            json!({"closeError":{"error":"storage_unavailable"}}),
        ),
        (
            "close-failure-replaces-handler-error",
            json!({"handleError":{"error":"forbidden"},"closeError":{"error":"storage_unavailable"}}),
        ),
    ] {
        let mut step = request("mcp", "tools/list");
        step["mcp"] = control;
        cases.push(one(name, step));
    }
    for gate in ["catalogue", "close"] {
        let mut step = request("mcp", "tools/list");
        if gate == "close" {
            step["mcp"] = json!({"closeGate":"pending"});
        }
        let mut input = trace(
            &format!("pending-{gate}-retains-outer-scope-through-close"),
            vec![
                connect(),
                step,
                json!({"op":"snapshot","name":"pending"}),
                json!({"op":"probe","id":"held","bytes":0}),
                json!({"op":"close","socket":"s"}),
                json!({"op":"release","gate":"pending"}),
                json!({"op":"probe","id":"released","bytes":0}),
            ],
        );
        input["budget"] = json!([1, 100000, 1, 100000]);
        if gate == "catalogue" {
            input["replies"] = json!({"mcp.catalog":[{"gate":"pending","value":[]}]});
        }
        cases.push(input);
    }
    let rows = compare("http-mcp-lifetime", cases);
    assert_eq!(kinds(&rows[0]), vec!["worker", "mcp-create"]);
    assert_eq!(
        kinds(&rows[1]),
        vec!["worker", "mcp-create", "mcp-handle", "mcp-close"]
    );
    // The successful response has already ended when close fails. The outer
    // catch preserves those emitted bytes; an unended handler error is replaced.
    assert_eq!(body(&rows[2], "mcp")["tools"], json!([]));
    assert_eq!(code(&rows[3], "mcp"), "storage_unavailable");
    for row in &rows[4..] {
        assert_eq!(row["snapshots"][0]["active"][0][1], 1);
        assert_eq!(
            last(row)["results"]["held"]["error"]["data"]["code"],
            "limit_exceeded"
        );
        assert_eq!(last(row)["results"]["released"]["ok"], true);
        assert_eq!(last(row)["active"][0][1], 0);
    }
}

#[test]
fn absent_mcp_observer_does_not_evaluate_final_latency_clock() {
    struct Empty;
    impl mcp_request::Storage for Empty {
        async fn request(&mut self, _: IvyJson) -> IvyResult<IvyJson> {
            Ok(IvyJson::Array(vec![]))
        }
    }
    struct Calls {
        observing: bool,
        ticks: usize,
        observations: usize,
    }
    impl mcp_request::Invocation for Calls {
        fn new_id(&mut self) -> JsString {
            panic!("Unknown tool cannot invoke")
        }
        fn now_ms(&mut self) -> f64 {
            self.ticks += 1;
            100000.0
        }
        async fn invoke(&mut self, _: IvyJson) -> mcp_request::Result<IvyJson> {
            panic!("Unknown tool cannot invoke")
        }
        fn observe(&mut self, _: mcp_request::Observation) -> mcp_request::Result<()> {
            assert!(self.observing);
            self.observations += 1;
            Ok(())
        }
        fn observing(&self) -> bool {
            self.observing
        }
    }
    tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(async {
            for observing in [false, true] {
                let mut calls = Calls {
                    observing,
                    ticks: 0,
                    observations: 0,
                };
                let options = mcp_request::Options {
                    surface: "ivy".into(),
                    version: "1.2.3".into(),
                    public_base_url: "https://example.test/ivy/".into(),
                    resource_metadata_url:
                        "https://example.test/ivy/.well-known/oauth-protected-resource".into(),
                };
                let result = mcp_request::call_tool(
                    &mut Empty,
                    &options,
                    &IvyJson::Null,
                    &"unknown".into(),
                    None,
                    &Language,
                    &mut calls,
                )
                .await;
                assert!(matches!(
                    result,
                    Err(mcp_request::Error::Protocol(mcp_request::ProtocolError {
                        code: -32601,
                        ..
                    }))
                ));
                assert_eq!(
                    (calls.ticks, calls.observations),
                    if observing { (2, 1) } else { (1, 0) }
                );
            }
        });
}
