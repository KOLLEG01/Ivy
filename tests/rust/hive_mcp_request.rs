use super::*;
use ivy_hive::mcp_request::{self as request, Invocation, Storage};
use std::collections::VecDeque;

fn field<'a>(value: &'a IvyJson, key: &str) -> IvyResult<&'a IvyJson> {
    value.get(key).ok_or_else(IvyError::internal)
}
fn text(value: &IvyJson, key: &str) -> IvyResult<JsString> {
    field(value, key)?
        .as_js_string()
        .cloned()
        .ok_or_else(IvyError::internal)
}
fn fail(value: &IvyJson) -> request::Error {
    if value.get("generic") == Some(&IvyJson::Bool(true)) {
        return IvyError::internal().into();
    }
    if let Some(protocol) = value.get("protocol") {
        return request::Error::Protocol(request::ProtocolError {
            code: protocol.get("code").and_then(IvyJson::as_f64).unwrap() as i32,
            message: text(protocol, "message").unwrap(),
            data: field(protocol, "data").unwrap().clone(),
        });
    }
    let mut error = IvyError::new_js(
        text(value, "code").unwrap(),
        text(value, "message").unwrap(),
    );
    if let Some(outcome) = value.get("outcome") {
        error = error.with_provider_outcome(outcome.clone());
    }
    error.details = value.get("details").cloned();
    error.into()
}
struct FixtureStorage<'a> {
    access: request::StoreAccess<'a>,
    input: &'a IvyJson,
    calls: Vec<IvyJson>,
}
impl Storage for FixtureStorage<'_> {
    async fn request(&mut self, message: IvyJson) -> IvyResult<IvyJson> {
        self.calls.push(message.clone());
        if let Some(failure) = self
            .input
            .get("storageFailure")
            .filter(|v| v.get("action") == message.get("action"))
        {
            return Err(match fail(failure) {
                request::Error::Ivy(error) => error,
                _ => IvyError::internal(),
            });
        }
        if message.get("action") == Some(&IvyJson::from("mcp.catalog")) {
            if let Some(bindings) = self.input.get("catalogBindings") {
                return Ok(bindings.clone());
            }
        }
        self.access.request(message).await
    }
}
struct FixtureInvocation<'a> {
    input: &'a IvyJson,
    times: VecDeque<f64>,
    last: f64,
    ids: usize,
    instructions: IvyJson,
    envelopes: Vec<IvyJson>,
    observations: Vec<IvyJson>,
}
impl Invocation for FixtureInvocation<'_> {
    fn new_id(&mut self) -> JsString {
        let index = self.ids;
        self.ids += 1;
        self.input
            .get("ids")
            .and_then(IvyJson::as_array)
            .and_then(|v| v.get(index))
            .and_then(IvyJson::as_js_string)
            .cloned()
            .unwrap_or_else(|| format!("request-{index}").into())
    }
    fn now_ms(&mut self) -> f64 {
        if let Some(value) = self.times.pop_front() {
            self.last = value;
        }
        self.last
    }
    async fn invoke(&mut self, envelope: IvyJson) -> request::Result<IvyJson> {
        self.envelopes.push(envelope.clone());
        // Cross an actual pending future before producing a response. Output
        // projection must still happen after this await, including its failure.
        let mut pending = true;
        std::future::poll_fn(|cx| {
            if pending {
                pending = false;
                cx.waker().wake_by_ref();
                std::task::Poll::Pending
            } else {
                std::task::Poll::Ready(())
            }
        })
        .await;
        if let Some(error) = self.input.get("invokeFailure") {
            return Err(fail(error));
        }
        if envelope.get("method") == Some(&IvyJson::from("discovery.instructions")) {
            return Ok(self.instructions.clone());
        }
        Ok(self.input.get("reply").cloned().unwrap_or(IvyJson::Null))
    }
    fn observe(&mut self, observation: request::Observation) -> request::Result<()> {
        self.observations.push(observation.to_json());
        if let Some(error) = self.input.get("observerFailure") {
            return Err(fail(error));
        }
        Ok(())
    }
}
pub(super) fn execute(
    store: &Store,
    registry: &mut Registry,
    discovery: &mut ivy_hive::discovery::Discovery,
    runtime: &ObservedRuntime,
    input: &IvyJson,
    now: f64,
) -> IvyResult<IvyJson> {
    let defaults=IvyJson::from_serde(&json!({"surface":"ivy","version":"1.2.3","publicBaseUrl":"https://hive.invalid/ivy/","resourceMetadataUrl":"https://hive.invalid/ivy/.well-known/oauth-protected-resource"})).unwrap();
    let mut settings = defaults;
    if let Some(v) = input.get("options") {
        for (k, v) in v.as_object().unwrap_or(&[]) {
            settings.insert(k.clone(), v.clone());
        }
    }
    let options = request::Options {
        surface: text(&settings, "surface")?,
        version: text(&settings, "version")?,
        public_base_url: text(&settings, "publicBaseUrl")?,
        resource_metadata_url: text(&settings, "resourceMetadataUrl")?,
    };
    let mode = text(input, "mode")?;
    let default_body = IvyJson::object([
        ("jsonrpc", "2.0".into()),
        ("id", "request".into()),
        (
            "method",
            if mode == JsString::from("call") {
                "tools/call"
            } else {
                "tools/list"
            }
            .into(),
        ),
        (
            "params",
            IvyJson::object(std::iter::empty::<(&str, IvyJson)>()),
        ),
    ]);
    let body = input.get("body").unwrap_or(&default_body);
    let dynamic = options.surface != JsString::from("ivy_dev")
        && ["initialize", "server/discover"]
            .iter()
            .any(|m| body.get("method") == Some(&IvyJson::from(*m)));
    let instructions = if dynamic && input.get("invokeFailure").is_none() {
        discovery.instructions(store, registry, runtime, now)?
    } else {
        IvyJson::Null
    };
    let times = input
        .get("clock")
        .and_then(IvyJson::as_array)
        .map(|v| v.iter().map(|v| v.as_f64().unwrap()).collect())
        .unwrap_or_default();
    let mut invocation = FixtureInvocation {
        input,
        times,
        last: now,
        ids: 0,
        instructions,
        envelopes: vec![],
        observations: vec![],
    };
    let mut storage = FixtureStorage {
        access: request::StoreAccess {
            store,
            registry,
            discovery,
            runtime,
            now,
        },
        input,
        calls: vec![],
    };
    let mut measured = IvyJson::Null;
    let result: request::Result<IvyJson> = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(async {
            if mode == JsString::from("failure") {
                let error = match fail(field(input, "error")?) {
                    request::Error::Ivy(e) => e,
                    _ => IvyError::internal(),
                };
                return Ok(request::failure(&error, &options.resource_metadata_url)?);
            }
            let instructions = request::instructions(&options, body, &mut invocation).await?;
            let info = request::server_info(&options)?;
            if mode == JsString::from("info") {
                return Ok(info.clone());
            }
            if mode == JsString::from("instructions") {
                return Ok(instructions);
            }
            let params = field(body, "params")?;
            if mode == JsString::from("call") {
                return request::call_tool(
                    &mut storage,
                    &options,
                    body,
                    &text(params, "name")?,
                    params.get("arguments"),
                    runtime,
                    &mut invocation,
                )
                .await;
            }
            let result = request::list_tools(
                &mut storage,
                &options,
                &info,
                params.get("cursor").and_then(IvyJson::as_js_string),
                runtime,
            )
            .await
            .map_err(request::Error::Protocol)?;
            measured = request::measured_list_result(&result, &info);
            Ok(result)
        });
    let (mut result_value, mut protocol, mut thrown) =
        (IvyJson::Null, IvyJson::Null, IvyJson::Null);
    match result {
        Ok(value) => result_value = value,
        Err(request::Error::Protocol(error)) => protocol = error.to_json(),
        Err(request::Error::Ivy(error)) => thrown = error.to_wire(),
    }
    Ok(IvyJson::object([
        ("result", result_value),
        ("protocolError", protocol),
        ("thrown", thrown),
        ("calls", IvyJson::Array(storage.calls)),
        ("observations", IvyJson::Array(invocation.observations)),
        ("envelopes", IvyJson::Array(invocation.envelopes)),
        ("measured", measured),
    ]))
}

fn setup() -> Vec<Value> {
    let mut s = super::setup();
    s.push(sql("UPDATE metadata SET value='0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef' WHERE key='cursor_secret';"));
    s.push(json!({"op":"reopen"}));
    s
}
fn operation(mut params: Value) -> Value {
    if params.get("clock").is_none() {
        params["clock"] = json!([100000, 100025]);
    }
    let mut step = action("mcpRequest", None, params);
    step["op"] = json!("discovery");
    step
}
fn tool(name: &str, kind: &str, read: bool) -> Value {
    mcp::binding(
        name,
        "fixture.read",
        mcp::input(kind),
        json!({"type":"string"}),
        read,
        Some("node"),
    )
}
fn call(bindings: Vec<Value>, arguments: Value) -> Value {
    operation(
        json!({"mode":"call","catalogBindings":bindings,"body":{"id":"outer","method":"tools/call","params":{"name":"fixture_read","arguments":arguments}},"reply":"answer"}),
    )
}

#[test]
fn mcp_request_public_and_development_server_metadata_and_instruction_dispatch_match_original() {
    let mut s = setup();
    let mut info = vec![];
    let mut instruction = vec![];
    for surface in ["ivy", "ivy_dev"] {
        for url in [
            "https://User:Pass@HIVE.invalid:443/ivy/?q=1#part",
            "https://[::1]:8443/space%20name//",
            "https://bücher.invalid/路径/",
            "http://127.0.0.1",
            "https://hive.invalid/base",
            "file://server/C|/base/",
            "custom:opaque  ?q",
            "blob:https://hive.invalid/abc",
        ] {
            info.push(push(
                &mut s,
                operation(json!({"mode":"info","options":{"surface":surface,"publicBaseUrl":url}})),
            ));
        }
        for method in ["initialize", "server/discover", "tools/list", "tools/call"] {
            instruction.push((surface,method,push(&mut s,operation(json!({"mode":"instructions","options":{"surface":surface},"body":{"method":method,"id":"initialize"}})))));
        }
    }
    let failed = push(
        &mut s,
        operation(
            json!({"mode":"instructions","body":{"method":"initialize"},"invokeFailure":{"code":"unauthenticated","message":"Expired.","outcome":"not_executed"}}),
        ),
    );
    let invalid_url = push(
        &mut s,
        operation(
            json!({"mode":"info","body":{"method":"initialize"},"options":{"publicBaseUrl":"relative/path"}}),
        ),
    );
    let r = compare(&s);
    for i in info {
        success(&r, i);
        assert!(
            value(&r, i)["result"]["icons"][0]["src"]
                .as_str()
                .unwrap()
                .ends_with(".svg")
        );
    }
    for (surface, method, i) in instruction {
        success(&r, i);
        assert!(value(&r, i)["result"].as_str().unwrap().len() > 100);
        assert_eq!(
            value(&r, i)["envelopes"].as_array().unwrap().len(),
            usize::from(surface == "ivy" && ["initialize", "server/discover"].contains(&method))
        );
    }
    assert_eq!(
        value(&r, failed)["thrown"]["data"]["code"],
        "unauthenticated"
    );
    assert_eq!(
        value(&r, invalid_url)["thrown"]["data"]["code"],
        "internal_error"
    );
    assert_eq!(
        value(&r, invalid_url)["envelopes"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn mcp_request_errors_preserve_wire_outcomes_details_and_authentication_challenge_match_original() {
    let mut s = setup();
    let mut cases = vec![];
    for code in [
        "unauthenticated",
        "invalid_arguments",
        "invalid_frame",
        "outcome_unknown",
        "provider_contract_error",
    ] {
        cases.push((code,push(&mut s,operation(json!({"mode":"failure","error":{"code":code,"message":"problem ".repeat(310),"outcome":"unknown","details":{"nested":[1,null,"é"]}},"options":{"resourceMetadataUrl":"https://hive.invalid/meta?q=\"quoted\""}})))));
    }
    let foreign = push(
        &mut s,
        operation(json!({"mode":"failure","error":{"generic":true}})),
    );
    let mut raw = operation(
        json!({"mode":"failure","error":{"code":"unauthenticated","message":"","outcome":"completed","details":null}}),
    );
    raw["parameterUnits"] =
        json!({"/error/message":[55296,120,56320],"/options/resourceMetadataUrl":[55296,34,56320]});
    raw["params"]["options"] = json!({"resourceMetadataUrl":""});
    raw["lossless"] = json!(true);
    let raw = push(&mut s, raw);
    let r = compare(&s);
    for (code, i) in cases {
        success(&r, i);
        let result = &value(&r, i)["result"];
        assert_eq!(result["structuredContent"]["error"]["data"]["code"], code);
        assert_eq!(
            result["structuredContent"]["error"]["message"]
                .as_str()
                .unwrap()
                .encode_utf16()
                .count(),
            2048
        );
        assert_eq!(result.get("_meta").is_some(), code == "unauthenticated");
    }
    assert_eq!(
        value(&r, foreign)["result"]["structuredContent"]["error"]["data"]["code"],
        "internal_error"
    );
    success(&r, raw);
}

#[test]
fn mcp_request_calls_admit_current_bindings_once_and_observe_success_failure_and_protocol_errors_match_original()
 {
    let mut s = setup();
    let binding = tool("fixture_read", "string", true);
    let args = json!({"serviceNodeId":"node","input":{"value":"exact input"}});
    let ok = push(&mut s, call(vec![binding.clone()], args.clone()));
    let invalid = push(
        &mut s,
        call(
            vec![binding.clone()],
            json!({"serviceNodeId":"node","input":{"value":3}}),
        ),
    );
    let missing = push(&mut s, call(vec![], args.clone()));
    let mut failed = vec![];
    for error in [
        json!({"code":"unauthenticated","message":"Expired.","outcome":"not_executed"}),
        json!({"code":"deadline_exceeded","message":"Timeout.","outcome":"unknown","details":{"posted":true}}),
        json!({"generic":true}),
        json!({"protocol":{"code":-32602,"message":"Synthetic protocol refusal.","data":{"exact":true}}}),
    ] {
        let mut step = call(vec![binding.clone()], args.clone());
        step["params"]["invokeFailure"] = error;
        failed.push(push(&mut s, step));
    }
    let mut raw_outcomes = vec![];
    for outcome in [
        json!(["unknown"]),
        json!(["completed"]),
        json!([["not_executed"]]),
    ] {
        for details in [None, Some(Value::Null), Some(json!({"kept": true}))] {
            let mut step = call(vec![binding.clone()], args.clone());
            let mut error = json!({"code":"remote","message":"Retained.","outcome":outcome});
            if let Some(details) = &details {
                error["details"] = details.clone();
            }
            step["params"]["invokeFailure"] = error;
            raw_outcomes.push((push(&mut s, step), outcome.clone(), details));
        }
    }
    // Provider calls build/validate their descriptor before dispatch. A canonical
    // core binding without a selector reaches output projection after invoke.
    let bad_output = mcp::binding(
        "fixture_read",
        "hive.system.status",
        json!({"type":"object"}),
        Value::Null,
        true,
        None,
    );
    let late = push(&mut s, call(vec![bad_output], json!({})));
    let mut observe = call(vec![binding], args);
    observe["params"]["observerFailure"] =
        json!({"code":"observer_failed","message":"Observer refused.","outcome":"completed"});
    let observe = push(&mut s, observe);
    let r = compare(&s);
    assert_eq!(
        value(&r, ok)["result"]["structuredContent"],
        json!({"result":"answer"})
    );
    assert_eq!(value(&r, invalid)["envelopes"], json!([]));
    assert_eq!(
        value(&r, invalid)["observations"][0]["code"],
        "invalid_arguments"
    );
    assert_eq!(value(&r, missing)["protocolError"]["code"], -32601);
    assert_eq!(
        value(&r, missing)["observations"][0]["outcome"],
        "not_executed"
    );
    for i in [
        ok, late, observe, failed[0], failed[1], failed[2], failed[3],
    ] {
        assert_eq!(value(&r, i)["envelopes"].as_array().unwrap().len(), 1);
        assert_eq!(value(&r, i)["observations"].as_array().unwrap().len(), 1);
    }
    assert_eq!(
        value(&r, late)["result"]["structuredContent"]["error"]["data"]["code"],
        "internal_error"
    );
    assert_eq!(
        value(&r, failed[3])["observations"][0]["code"],
        "internal_error"
    );
    assert_eq!(
        value(&r, observe)["thrown"]["data"]["code"],
        "observer_failed"
    );
    for (index, outcome, details) in raw_outcomes {
        success(&r, index);
        let result = value(&r, index);
        let observations = result["observations"].as_array().unwrap();
        assert_eq!(observations.len(), 1);
        assert_eq!(observations[0]["code"], "remote");
        assert_eq!(observations[0]["outcome"], outcome);
        let error = &result["result"]["structuredContent"]["error"];
        assert_eq!(error["data"]["outcome"], outcome);
        assert_eq!(error["data"].get("details"), details.as_ref());
        assert_eq!(result["envelopes"].as_array().unwrap().len(), 1);
    }
}

#[test]
fn mcp_request_observation_nullish_identity_precedence_utf16_and_clock_bounds_match_original() {
    let mut s = setup();
    let binding = tool("fixture_read", "string", true);
    let base = json!({"serviceNodeId":"node","input":{"value":"exact input"}});
    let mut cases = vec![];
    for (id, clock, op, mutation) in [
        (
            json!(9007199254740991u64),
            json!([1000, 990]),
            json!(""),
            json!("unused"),
        ),
        (
            json!(9007199254740992u64),
            json!([0, 86400001]),
            Value::Null,
            json!("mutation"),
        ),
        (json!(1.5), json!([0, 100]), json!(7), json!("unused")),
        (
            json!(true),
            json!([100, 200]),
            json!(false),
            json!("unused"),
        ),
    ] {
        let mut args = base.clone();
        args["operationId"] = op;
        args["mutationId"] = mutation;
        let mut b = binding.clone();
        b["definition"]["annotations"]["readOnlyHint"] = json!(false);
        let mut step = call(vec![b], args);
        step["params"]["body"]["id"] = id;
        step["params"]["clock"] = clock;
        cases.push(push(&mut s, step));
    }
    let mut long = call(vec![], base);
    long["params"]["body"]["id"] = json!("a".repeat(127) + "😀tail");
    long["params"]["body"]["params"]["name"] = json!("b".repeat(127) + "😀tail");
    long["params"]["body"]["params"]["arguments"]["operationId"] =
        json!("c".repeat(511) + "😀tail");
    long["lossless"] = json!(true);
    let long = push(&mut s, long);
    let r = compare(&s);
    assert_eq!(value(&r, cases[0])["observations"][0]["latencyMs"], 0);
    assert!(
        value(&r, cases[0])["observations"][0]
            .get("operationIdentity")
            .is_none()
    );
    assert_eq!(
        value(&r, cases[1])["observations"][0]["latencyMs"],
        86400000
    );
    assert_eq!(
        value(&r, cases[1])["observations"][0]["operationIdentity"],
        "mutation"
    );
    for i in &cases[1..] {
        assert!(value(&r, *i)["observations"][0]["requestId"].is_null());
    }
    success(&r, long);
}

fn list(surface: &str) -> Value {
    operation(
        json!({"mode":"list","options":{"surface":surface},"body":{"id":"list","method":"tools/list","params":{"cursor":""}}}),
    )
}
fn follow(mut step: Value, from: usize, select: &str) -> Value {
    step["params"]["body"]["params"]["cursor"] = Value::Null;
    step["identityBindings"] =
        json!([{"from":from,"select":select,"at":"/params/body/params/cursor"}]);
    step
}
fn base_list(bindings: Vec<Value>) -> Value {
    let mut step = list("ivy");
    step["params"]["catalogBindings"] = json!(bindings);
    step
}

#[test]
fn mcp_request_pages_the_actual_catalog_with_private_metadata_budget_and_invalidates_changed_catalogs_match_original()
 {
    let mut s = setup();
    let initial = push(&mut s, list("ivy_dev"));
    let tools = (0..20)
        .map(|i| {
            let mut t = mcp::defined(
                &format!("large{i}"),
                &format!("sample_large_{i}"),
                "ivy_dev",
                "string",
                true,
            );
            t["inputSchema"]["description"] = json!("é".repeat(15000));
            t
        })
        .collect::<Vec<_>>();
    mcp::ready(&mut s, "node", tools.clone());
    let first = push(&mut s, list("ivy_dev"));
    push(&mut s, heartbeat("node", true, vec![]));
    let second = push(
        &mut s,
        follow(list("ivy_dev"), first, "/value/result/nextCursor"),
    );
    let wrong_surface = push(
        &mut s,
        follow(list("ivy"), first, "/value/result/nextCursor"),
    );
    let mut changed = catalog("fixture");
    changed["namespaces"][0]["tools"] = json!(&tools[..19]);
    push(&mut s, action("sync", Some("node"), changed));
    let stale = push(
        &mut s,
        follow(list("ivy_dev"), first, "/value/result/nextCursor"),
    );
    let r = compare(&s);
    let count = value(&r, initial)["result"]["tools"]
        .as_array()
        .unwrap()
        .len();
    let mut names = std::collections::HashSet::new();
    for i in [first, second] {
        let v = value(&r, i);
        assert!(v["protocolError"].is_null());
        assert!(serde_json::to_vec(&v["measured"]).unwrap().len() <= 512 * 1024);
        assert_eq!(v["measured"]["ttlMs"], 0);
        assert_eq!(v["measured"]["cacheScope"], "private");
        assert_eq!(
            v["measured"]["_meta"]["io.modelcontextprotocol/serverInfo"]["name"],
            "ivy_dev"
        );
        for tool in v["result"]["tools"].as_array().unwrap() {
            assert!(names.insert(tool["name"].as_str().unwrap()));
        }
    }
    assert_eq!(names.len(), count + 20);
    assert!(value(&r, first)["result"]["nextCursor"].is_string());
    assert!(value(&r, second)["result"]["nextCursor"].is_null());
    for i in [wrong_surface, stale] {
        assert_eq!(value(&r, i)["protocolError"]["code"], -32602);
        assert_eq!(
            value(&r, i)["protocolError"]["data"]["code"],
            "invalid_cursor"
        );
    }
}

#[test]
fn mcp_request_cursor_boundaries_oversized_descriptor_and_worker_failures_match_original() {
    let mut s = setup();
    let binding = tool("fixture_read", "string", true);
    let first = push(&mut s, base_list(vec![binding.clone()]));
    let mut boundaries = vec![];
    for boundary in [
        json!(-1),
        json!(0.5),
        json!("1"),
        Value::Null,
        json!([0]),
        json!(1),
        json!(2),
    ] {
        let mut cursor = action("cursor", None, json!({"identity":{},"boundary":boundary}));
        cursor["op"] = json!("discovery");
        cursor["identityBindings"] =
            json!([{"from":first,"select":"/value/calls/1/identity","at":"/params/identity"}]);
        let cursor = push(&mut s, cursor);
        boundaries.push(push(
            &mut s,
            follow(base_list(vec![binding.clone()]), cursor, "/value"),
        ));
    }
    let mut unsafe_cursor = action(
        "cursor",
        None,
        json!({"identity":{},"boundary":9007199254740992u64}),
    );
    unsafe_cursor["op"] = json!("discovery");
    unsafe_cursor["identityBindings"] =
        json!([{"from":first,"select":"/value/calls/1/identity","at":"/params/identity"}]);
    let unsafe_cursor = push(&mut s, unsafe_cursor);
    let mut forged = base_list(vec![binding.clone()]);
    forged["params"]["body"]["params"]["cursor"] = json!("not-a-signed-cursor");
    let forged = push(&mut s, forged);
    let empty = push(&mut s, base_list(vec![]));
    let mut huge = binding.clone();
    huge["definition"]["inputSchema"]["description"] = json!("x".repeat(512 * 1024));
    let huge = push(&mut s, base_list(vec![huge]));
    let mut failure = base_list(vec![binding]);
    failure["params"]["storageFailure"] = json!({"action":"mcp.catalog","generic":true});
    let failure = push(&mut s, failure);
    let r = compare(&s);
    for i in &boundaries[..5] {
        assert_eq!(
            value(&r, *i)["protocolError"]["data"]["code"],
            "invalid_cursor"
        );
        assert_eq!(
            value(&r, *i)["protocolError"]["message"],
            "Invalid MCP discovery boundary."
        );
    }
    assert_eq!(value(&r, boundaries[5])["result"]["tools"], json!([]));
    assert_eq!(
        value(&r, boundaries[6])["protocolError"]["message"],
        "MCP discovery cursor does not match the current catalog."
    );
    assert_eq!(code(&r, unsafe_cursor), "invalid_arguments");
    assert_eq!(
        value(&r, forged)["protocolError"]["data"]["code"],
        "invalid_cursor"
    );
    assert_eq!(value(&r, empty)["result"]["tools"], json!([]));
    assert_eq!(
        value(&r, huge)["protocolError"]["data"]["code"],
        "result_too_large"
    );
    assert_eq!(
        value(&r, failure)["protocolError"]["data"]["code"],
        "internal_error"
    );
}

#[test]
fn mcp_request_real_catalog_calls_keep_current_provider_hash_mutation_identity_and_reject_stale_arguments_match_original()
 {
    let mut s = setup();
    let tool = mcp::defined("write", "sample_write", "ivy", "string", false);
    mcp::ready(&mut s, "a", vec![tool.clone()]);
    mcp::ready(&mut s, "b", vec![tool.clone()]);
    let call = operation(
        json!({"mode":"call","body":{"id":42,"method":"tools/call","params":{"name":"sample_write","arguments":{"serviceNodeId":"a","operationId":"original-action","input":{"value":"exact payload"}}}},"reply":"current reply"}),
    );
    let first = push(&mut s, call.clone());
    let mut obsolete = tool;
    obsolete["inputSchema"] = json!({"type":"object","properties":{"replacement":{"type":"string"}},"required":["replacement"],"additionalProperties":false});
    let mut c = catalog("fixture");
    c["namespaces"][0]["tools"] = json!([obsolete]);
    push(&mut s, action("sync", Some("a"), c));
    let stale = push(&mut s, call.clone());
    let mut other = call.clone();
    other["params"]["body"]["params"]["arguments"]["serviceNodeId"] = json!("b");
    let other = push(&mut s, other);
    let mut missing = call.clone();
    missing["params"]["body"]["params"]["arguments"]["serviceNodeId"] = json!("absent");
    let missing = push(&mut s, missing);
    let mut development = call;
    development["params"]["options"] = json!({"surface":"ivy_dev"});
    let development = push(&mut s, development);
    let r = compare(&s);
    for i in [first, other] {
        let v = value(&r, i);
        assert_eq!(
            v["result"]["structuredContent"],
            json!({"result":"current reply"})
        );
        assert_eq!(v["observations"][0]["operationIdentity"], "original-action");
        assert_eq!(v["envelopes"].as_array().unwrap().len(), 1);
        assert!(
            v["envelopes"][0]["params"]["expectedDefinitionHash"]
                .as_str()
                .unwrap()
                .starts_with("sha256:")
        );
    }
    assert_eq!(
        value(&r, first)["envelopes"][0]["params"]["serviceNodeId"],
        "a"
    );
    assert_eq!(
        value(&r, other)["envelopes"][0]["params"]["serviceNodeId"],
        "b"
    );
    for i in [stale, missing] {
        assert_eq!(
            value(&r, i)["result"]["structuredContent"]["error"]["data"]["code"],
            "invalid_arguments"
        );
        assert_eq!(value(&r, i)["envelopes"], json!([]));
    }
    assert_eq!(value(&r, development)["protocolError"]["code"], -32601);
}

#[test]
fn mcp_native_invocation_uses_fresh_ids_wall_clock_and_the_bound_invoker_and_observer() {
    let mut seen = vec![];
    let mut native = request::NativeInvocation {
        invoke: |value| std::future::ready(Ok(value)),
        observe: |observation: request::Observation| {
            seen.push(observation.to_json());
            Ok(())
        },
    };
    let first = native.new_id();
    let second = native.new_id();
    assert_ne!(first, second);
    for id in [&first, &second] {
        assert_eq!(
            uuid::Uuid::parse_str(&String::from_utf16(id.0.as_slice()).unwrap())
                .unwrap()
                .get_version_num(),
            4
        );
    }
    let before = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as f64;
    let now = native.now_ms();
    let after = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as f64;
    assert!((before..=after).contains(&now));
    let envelope = IvyJson::object([
        ("id", IvyJson::String(first)),
        ("method", "discovery.call".into()),
    ]);
    let actual = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(native.invoke(envelope.clone()))
        .unwrap();
    assert_eq!(actual, envelope);
    native
        .observe(request::Observation {
            request_id: IvyJson::Null,
            internal_request_ids: vec![second],
            tool: "fixture_read".into(),
            code: "ok".into(),
            outcome: ivy_contracts::Outcome::Completed.into(),
            latency_ms: 0.0,
            operation_identity: None,
        })
        .unwrap();
    assert_eq!(seen.len(), 1);
}
