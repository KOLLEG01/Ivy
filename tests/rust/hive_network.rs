use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString, Outcome,
    hash::digest,
    json::{parse, stringify},
};
#[path = "../fixtures/hive-storage-payload.rs"]
mod payload_fixture;
use ivy_hive::{
    live_routing::{CallContext, LiveRouting, PreparedCall},
    retention::Runtime,
    tool_binding_diagnostics::ToolBindingDiagnostics,
    ws_send::{self, Socket},
};
use std::{
    cmp::Ordering,
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex},
};

#[path = "hive_catalog_fixture.rs"]
mod catalog_fixture;
#[path = "hive_core_calls.rs"]
mod core_calls;
#[path = "hive_dispatch_gate.rs"]
mod dispatch_gate;
#[path = "hive_mcp_dispatch.rs"]
mod mcp_dispatch;
#[path = "hive_mcp_inbound.rs"]
mod mcp_inbound;
#[path = "hive_mcp_parameters.rs"]
mod mcp_parameters;
#[path = "hive_mcp_protocol.rs"]
mod mcp_protocol;
#[path = "hive_mcp_subscription.rs"]
mod mcp_subscription;
#[path = "hive_network_cases.rs"]
mod network_cases;
#[path = "../fixtures/reference-data.rs"]
mod reference_data;
#[path = "hive_server_constructor.rs"]
mod server_constructor;
#[path = "hive_server_lifecycle.rs"]
mod server_lifecycle;
#[path = "hive_server_options.rs"]
mod server_options;
#[path = "hive_server_peers.rs"]
mod server_peers;
#[path = "hive_server_rpc.rs"]
mod server_rpc;
#[path = "hive_tool_routing_cases.rs"]
mod tool_routing_cases;
#[path = "hive_ws_socket_io.rs"]
mod ws_socket_io;

trait FixtureMut {
    fn get_mut(&mut self, name: &str) -> Option<&mut IvyJson>;
    fn as_array_mut(&mut self) -> Option<&mut Vec<IvyJson>>;
}
impl FixtureMut for IvyJson {
    fn get_mut(&mut self, name: &str) -> Option<&mut IvyJson> {
        let IvyJson::Object(pairs) = self else {
            return None;
        };
        pairs
            .iter_mut()
            .find(|(key, _)| key == &JsString::from(name))
            .map(|(_, value)| value)
    }
    fn as_array_mut(&mut self) -> Option<&mut Vec<IvyJson>> {
        if let IvyJson::Array(values) = self {
            Some(values)
        } else {
            None
        }
    }
}

fn json(text: &str) -> IvyJson {
    parse(text, usize::MAX).unwrap()
}
fn encode(value: &IvyJson) -> String {
    stringify(value, usize::MAX).unwrap()
}
fn number(value: i64) -> IvyJson {
    IvyJson::Number(value as f64)
}
fn obj(pairs: impl IntoIterator<Item = (&'static str, IvyJson)>) -> IvyJson {
    IvyJson::object(pairs)
}
fn text(value: &IvyJson, name: &str) -> String {
    value.get(name).unwrap().as_str().unwrap()
}
fn flag(value: &IvyJson, name: &str) -> bool {
    value.get(name) == Some(&true.into())
}
fn ok() -> IvyJson {
    obj([("ok", true.into())])
}
fn error(error: IvyError) -> IvyJson {
    obj([("error", error.to_wire())])
}
fn injected() -> IvyError {
    IvyError::new("fixture_callback", "Isolated callback failed.").with_outcome(Outcome::Unknown)
}
// Historical traces include the former full definition. Compare the fields the
// routing owner retains now; provider wire frames are compared without projection.
fn prepared_from_reference(value: &IvyJson, validate: bool) -> PreparedCall {
    let definition = value.get("definition").unwrap();
    let mut qualified = definition
        .get("namespace")
        .unwrap()
        .as_js_string()
        .unwrap()
        .clone();
    qualified.0.push(b'.' as u16);
    qualified
        .0
        .extend(&definition.get("name").unwrap().as_js_string().unwrap().0);
    PreparedCall {
        context: CallContext {
            service_node_id: value
                .get("serviceNodeId")
                .unwrap()
                .as_js_string()
                .unwrap()
                .clone(),
            generation: value.get("generation").unwrap().as_f64().unwrap(),
            qualified_name: qualified,
            output_schema: validate
                .then(|| Arc::new(definition.get("outputSchema").unwrap().clone())),
            caller_principal_id: value
                .get("callerPrincipalId")
                .unwrap()
                .as_js_string()
                .unwrap()
                .clone(),
        },
        definition_hash: value
            .get("definitionHash")
            .unwrap()
            .as_js_string()
            .unwrap()
            .clone(),
        arguments: value.get("arguments").unwrap().clone(),
        operation_id: value.get("operationId").cloned(),
    }
}
fn prepared_snapshot(call: &PreparedCall) -> IvyJson {
    let context = &call.context;
    let mut value = obj([
        (
            "serviceNodeId",
            IvyJson::String(context.service_node_id.clone()),
        ),
        ("generation", context.generation.into()),
        (
            "qualifiedName",
            IvyJson::String(context.qualified_name.clone()),
        ),
        (
            "definitionHash",
            IvyJson::String(call.definition_hash.clone()),
        ),
        ("arguments", call.arguments.clone()),
        (
            "callerPrincipalId",
            IvyJson::String(context.caller_principal_id.clone()),
        ),
    ]);
    if let Some(schema) = &context.output_schema {
        value.insert("outputSchema", (**schema).clone());
    }
    if let Some(operation) = &call.operation_id {
        value.insert("operationId", operation.clone());
    }
    value
}
fn routing_reference(input: &IvyJson, original: &IvyJson) -> IvyJson {
    let mut expected = original.clone();
    for (step, row) in input
        .get("steps")
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .zip(expected.get_mut("steps").unwrap().as_array_mut().unwrap())
    {
        if matches!(text(step, "op").as_str(), "prepare" | "dashboard") {
            if let Some(value) = row.get_mut("value") {
                *value =
                    prepared_snapshot(&prepared_from_reference(value, flag(input, "validate")));
            }
        }
    }
    expected
}
fn project_prepared_events(rows: &mut IvyJson, validate: bool) {
    for row in rows.as_array_mut().unwrap() {
        for event in row.as_array_mut().unwrap() {
            if event.get("kind") == Some(&"prepared".into()) {
                let value = event.get_mut("value").unwrap();
                *value = prepared_snapshot(&prepared_from_reference(value, validate));
            }
        }
    }
}
fn routing(input: &IvyJson) -> IvyJson {
    let events = Arc::new(Mutex::new(vec![]));
    let callback = events.clone();
    let throws = flag(input, "throwRejected");
    let mut owner = LiveRouting::new(
        flag(input, "validate"),
        Some(Box::new(move |value| {
            callback.lock().unwrap().push(value);
            if throws { Err(injected()) } else { Ok(()) }
        })),
    );
    let mut calls: HashMap<String, PreparedCall> = HashMap::new();
    let mut rows = vec![];
    for step in input.required("steps").unwrap().as_array().unwrap() {
        let result = (|| -> IvyResult<IvyJson> {
            let row = match text(step, "op").as_str() {
                "update" => {
                    let catalog = step.get("catalog").filter(|v| !v.is_null())
                        .map(catalog_fixture::catalog_from_wire).transpose()?;
                    owner.update(
                        step.required("node")?,
                        step.get("generation").and_then(IvyJson::as_f64),
                        catalog.as_ref(),
                    )?;
                    ok()
                }
                "prepare" | "dashboard" => {
                    let call = if text(step, "op") == "prepare" {
                        owner.prepare(step.required("context")?, step.required("params")?.clone())?
                    } else {
                        owner.prepare_dashboard_image(
                            step.required("context")?,
                            step.required("node")?.as_js_string().unwrap(),
                            step.required("args")?.clone(),
                        )?
                    };
                    let value = prepared_snapshot(&call);
                    if let Some(name) = step.get("save") {
                        calls.insert(name.as_str()?, call);
                    }
                    obj([("value", value)])
                }
                "complete" => obj([(
                    "value",
                    owner.complete(
                        &calls[&text(step, "call")].context,
                        step.required("result")?.clone(),
                    )?,
                )]),
                "caller" => {
                    owner.caller(step.required("context")?)?;
                    ok()
                }
                "notification" => {
                    owner.notification(step.required("context")?, step.required("params")?)?;
                    ok()
                }
                "disconnect" => obj([(
                    "value",
                    owner
                        .disconnect(
                            step.required("node")?.as_js_string().unwrap(),
                            step.required("generation")?.as_f64().unwrap(),
                        )
                        .into(),
                )]),
                "suspend" => {
                    owner.suspend(
                        step.required("node")?.as_js_string().unwrap(),
                        step.required("generation")?.as_f64().unwrap(),
                    );
                    ok()
                }
                "retire" => {
                    owner.retire_unconfigured_principals(
                        &step
                            .required("principals")?
                            .as_array()
                            .unwrap()
                            .iter()
                            .map(|v| v.as_js_string().unwrap().clone())
                            .collect::<Vec<_>>(),
                    );
                    ok()
                }
                "clear" => {
                    owner.clear();
                    ok()
                }
                _ => panic!("Unknown routing step"),
            };
            Ok(row)
        })();
        rows.push(result.unwrap_or_else(error));
    }
    obj([
        ("steps", IvyJson::Array(rows)),
        ("events", IvyJson::Array(events.lock().unwrap().clone())),
    ])
}

// Original Date operations are captured as inputs. This fixture qualifies the
// diagnostic owner's ordering/state, not a production date or locale provider.
struct TraceRuntime(IvyJson, Arc<Mutex<Vec<IvyJson>>>);
impl Runtime for TraceRuntime {
    fn parse_date(&self, value: &JsString) -> f64 {
        self.1.lock().unwrap().push(obj([
            ("kind", "parse".into()),
            ("value", IvyJson::String(value.clone())),
        ]));
        self.0
            .get("dates")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r.as_array().unwrap()[0] == IvyJson::String(value.clone()))
            .unwrap()
            .as_array()
            .unwrap()[1]
            .as_f64()
            .unwrap_or(f64::NAN)
    }
    fn iso_date(&self, value: f64) -> IvyResult<JsString> {
        self.1.lock().unwrap().push(obj([
            ("kind", "iso".into()),
            ("value", IvyJson::Number(value)),
        ]));
        self.0
            .get("iso")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r.as_array().unwrap()[0].as_f64() == Some(value))
            .unwrap()
            .as_array()
            .unwrap()[1]
            .as_js_string()
            .cloned()
            .ok_or_else(IvyError::internal)
    }
    fn compare_contract_keys(&self, _: &JsString, _: &JsString) -> Ordering {
        panic!("Diagnostics must not request locale comparisons")
    }
}
fn diagnostics(input: &IvyJson, profile: &IvyJson) -> IvyJson {
    let events = Arc::new(Mutex::new(vec![]));
    let effects = Arc::new(Mutex::new(vec![]));
    let record_effects = effects.clone();
    let now_effects = effects.clone();
    let callback = events.clone();
    let throws = flag(input, "throwRecord");
    let clock = Arc::new(Mutex::new(VecDeque::<f64>::new()));
    let reads = Arc::new(Mutex::new(vec![]));
    let now_clock = clock.clone();
    let now_reads = reads.clone();
    let mut owner = ToolBindingDiagnostics::new(
        Box::new(move |value| {
            record_effects
                .lock()
                .unwrap()
                .push(obj([("kind", "record".into()), ("value", value.clone())]));
            callback.lock().unwrap().push(value);
            if throws { Err(injected()) } else { Ok(()) }
        }),
        Arc::new(move || {
            let value = now_clock
                .lock()
                .unwrap()
                .pop_front()
                .expect("Missing clock input");
            now_reads.lock().unwrap().push(IvyJson::Number(value));
            now_effects.lock().unwrap().push(obj([
                ("kind", "now".into()),
                ("value", IvyJson::Number(value)),
            ]));
            value
        }),
        Arc::new(TraceRuntime(profile.clone(), effects.clone())),
    );
    let mut rows = vec![];
    for step in input.required("steps").unwrap().as_array().unwrap() {
        *clock.lock().unwrap() = step
            .get("clock")
            .and_then(IvyJson::as_array)
            .unwrap_or(&[])
            .iter()
            .map(|v| v.as_f64().unwrap())
            .collect();
        reads.lock().unwrap().clear();
        let result = match text(step, "op").as_str() {
            "restore" => owner.restore(step.get("values").unwrap().as_array().unwrap()),
            "rejected" => owner.rejected(step.get("target").unwrap()),
            "completed" => owner.completed(step.get("target").unwrap()),
            _ => panic!("Unknown diagnostic step"),
        };
        let mut row = result.map(|_| ok()).unwrap_or_else(error);
        row.insert("reads", IvyJson::Array(reads.lock().unwrap().clone()));
        rows.push(row);
    }
    obj([
        ("steps", IvyJson::Array(rows)),
        ("events", IvyJson::Array(events.lock().unwrap().clone())),
        ("effects", IvyJson::Array(effects.lock().unwrap().clone())),
    ])
}
struct Peer {
    events: Vec<IvyJson>,
    input: IvyJson,
}
impl Socket for Peer {
    fn is_open(&self) -> bool {
        !flag(&self.input, "closed")
    }
    fn buffered_amount(&self) -> f64 {
        self.input
            .get("buffered")
            .and_then(IvyJson::as_f64)
            .unwrap_or(0.0)
    }
    fn send_text(&mut self, text: String) -> IvyResult<()> {
        self.events.push(obj([
            ("kind", "send".into()),
            ("byteLength", IvyJson::Number(text.len() as f64)),
            (
                "sha256",
                digest(text.as_bytes())
                    .strip_prefix("sha256:")
                    .unwrap()
                    .into(),
            ),
        ]));
        if flag(&self.input, "throwSend") {
            return Err(IvyError::internal());
        }
        if flag(&self.input, "callbackError") {
            self.terminate()?;
        }
        Ok(())
    }
    fn terminate(&mut self) -> IvyResult<()> {
        self.events.push(obj([("kind", "terminate".into())]));
        Ok(())
    }
}
fn send(input: &IvyJson) -> IvyJson {
    let mut peer = Peer {
        events: vec![],
        input: input.clone(),
    };
    let mut value = input.get("value").unwrap().clone();
    if flag(input, "nonfinite") {
        value.insert("result", IvyJson::Number(f64::NAN));
    }
    if let Some(size) = input.get("fillBytes").and_then(IvyJson::as_f64) {
        value.insert("result", "".into());
        let empty = encode(&value).len();
        value.insert("result", "x".repeat(size as usize - empty).into());
    }
    let encoded = input.get("encoded").map(|v| v.as_str().unwrap());
    let result = ws_send::send(&mut peer, &value, encoded.as_deref());
    match result {
        Ok(()) => obj([("events", IvyJson::Array(peer.events))]),
        Err(e) => obj([
            ("error", e.to_wire()),
            ("events", IvyJson::Array(peer.events)),
        ]),
    }
}
fn lifetime_reference(family: &str, traces: &[serde_json::Value]) -> Vec<serde_json::Value> {
    static CASES: std::sync::OnceLock<serde_json::Value> = std::sync::OnceLock::new();
    let cases = CASES.get_or_init(|| {
        serde_json::from_slice(&reference_data::read(include_bytes!(
            "../fixtures/hive-server-lifetime-reference.json.gz"
        )))
        .unwrap()
    });
    let key = digest(serde_json::to_string(traces).unwrap());
    cases
        .get(family)
        .or_else(|| {
            cases
                .get(if cfg!(windows) { "win32" } else { "linux" })
                .and_then(|platform| platform.get(family))
        })
        .unwrap()
        .get(&key)
        .unwrap_or_else(|| panic!("Missing original lifetime input: {family} {key}"))
        .as_array()
        .unwrap()
        .clone()
}
fn construction_reference(name: &str) -> &'static IvyJson {
    static CASES: std::sync::OnceLock<IvyJson> = std::sync::OnceLock::new();
    let cases = CASES.get_or_init(|| {
        let bytes = reference_data::read(include_bytes!(
            "../fixtures/hive-construction-reference.json.gz"
        ));
        json(std::str::from_utf8(&bytes).unwrap())
    });
    cases
        .get(name)
        .or_else(|| {
            cases
                .get(if cfg!(windows) { "win32" } else { "linux" })
                .and_then(|platform| platform.get(name))
        })
        .unwrap_or_else(|| panic!("Missing original construction data: {name}"))
}
fn compare(label: &str, cases: Vec<IvyJson>) {
    static CASES: std::sync::OnceLock<IvyJson> = std::sync::OnceLock::new();
    let reference = CASES.get_or_init(|| {
        let bytes =
            reference_data::read(include_bytes!("../fixtures/hive-network-reference.json.gz"));
        json(std::str::from_utf8(&bytes).unwrap())
    });
    let key = digest(encode(&IvyJson::Array(cases.clone())));
    let original = reference
        .get("traces")
        .unwrap()
        .get(&key)
        .unwrap_or_else(|| panic!("Missing original network input: {label} {key}"));
    let expected = original.as_array().unwrap();
    assert_eq!(expected.len(), cases.len());
    for (index, (input, original)) in cases.iter().zip(expected).enumerate() {
        let (actual, expected) = match text(input, "kind").as_str() {
            "routing" => (routing(input), routing_reference(input, original)),
            "diagnostics" => (
                diagnostics(input, original.get("profile").unwrap()),
                original.get("value").unwrap().clone(),
            ),
            _ => (send(input), original.clone()),
        };
        if let Some(steps) = input.get("steps").and_then(IvyJson::as_array) {
            assert_eq!(
                actual.get("steps").unwrap().as_array().unwrap().len(),
                steps.len()
            );
            assert_eq!(
                expected.get("steps").unwrap().as_array().unwrap().len(),
                steps.len()
            );
        }
        assert_eq!(actual, expected, "{label} case {index}: {}", encode(input));
    }
}
fn node(id: &str) -> IvyJson {
    obj([
        ("serviceNodeId", id.into()),
        ("hostId", "host".into()),
        ("serviceName", "provider".into()),
        ("principalId", "provider-principal".into()),
        ("connected", true.into()),
        ("synced", true.into()),
        ("ready", true.into()),
        ("desiredEnabled", true.into()),
    ])
}
fn binding(name: &str, mcp: bool) -> IvyJson {
    obj([
        ("qualifiedName", name.into()),
        (
            "definitionHash",
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
        ),
        (
            "definition",
            obj([
                ("namespace", name.split('.').next().unwrap().into()),
                ("name", name.split('.').nth(1).unwrap().into()),
                ("inputSchema", json("{\"type\":\"integer\",\"minimum\":1}")),
                ("outputSchema", json("{\"type\":\"string\"}")),
                ("discovery", obj([("mcp", mcp.into())])),
                ("annotations", obj([("readOnlyHint", true.into())])),
            ]),
        ),
    ])
}
fn catalog() -> IvyJson {
    obj([
        ("namespaces", json("[\"provider\",\"dashboards\"]")),
        (
            "tools",
            IvyJson::Array(vec![
                binding("provider.call", true),
                binding("dashboards.image", false),
            ]),
        ),
        (
            "notifications",
            json(
                "[{\"namespace\":\"provider\",\"name\":\"changed\",\"version\":\"1.0.0\",\"payloadSchema\":{\"type\":\"integer\"}}]",
            ),
        ),
    ])
}
fn context() -> IvyJson {
    json("{\"principalId\":\"caller\",\"transport\":\"http\"}")
}
fn params() -> IvyJson {
    obj([
        ("qualifiedName", "provider.call".into()),
        (
            "expectedDefinitionHash",
            binding("provider.call", true)
                .get("definitionHash")
                .unwrap()
                .clone(),
        ),
        ("arguments", number(1)),
    ])
}
fn update(id: &str, generation: f64) -> IvyJson {
    obj([
        ("op", "update".into()),
        ("node", node(id)),
        ("generation", IvyJson::Number(generation)),
        ("catalog", catalog()),
    ])
}
fn prepare(context: IvyJson, params: IvyJson) -> IvyJson {
    obj([
        ("op", "prepare".into()),
        ("context", context),
        ("params", params),
    ])
}
fn scenario(steps: Vec<IvyJson>, validate: bool) -> IvyJson {
    obj([
        ("kind", "routing".into()),
        ("validate", validate.into()),
        ("steps", IvyJson::Array(steps)),
    ])
}
