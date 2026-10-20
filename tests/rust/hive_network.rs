use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString, Outcome,
    hash::digest,
    json::{parse, stringify},
};
use ivy_hive::{
    live_routing::{LiveRouting, PreparedCall},
    retention::Runtime,
    tool_binding_diagnostics::ToolBindingDiagnostics,
    ws_send::{self, Socket},
};
use std::{
    cmp::Ordering,
    collections::{HashMap, VecDeque},
    io::Write,
    process::{Command, Stdio},
    sync::{Arc, Mutex},
};

#[path = "hive_server_rpc.rs"]
mod server_rpc;
#[path = "hive_dispatch_gate.rs"]
mod dispatch_gate;

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
                    owner.update(
                        step.required("node")?,
                        step.get("generation").and_then(IvyJson::as_f64),
                        step.get("catalog"),
                    )?;
                    ok()
                }
                "prepare" | "dashboard" => {
                    let call = if text(step, "op") == "prepare" {
                        owner.prepare(step.required("context")?, step.required("params")?)?
                    } else {
                        owner.prepare_dashboard_image(
                            step.required("context")?,
                            step.required("node")?.as_js_string().unwrap(),
                            step.required("args")?,
                        )?
                    };
                    let value = call.to_json();
                    if let Some(name) = step.get("save") {
                        calls.insert(name.as_str()?, call);
                    }
                    obj([("value", value)])
                }
                "complete" => obj([(
                    "value",
                    owner.complete(
                        &calls[&text(step, "call")],
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
static CORPUS: Mutex<()> = Mutex::new(());
fn compare(label: &str, cases: Vec<IvyJson>) {
    let node = std::env::var_os("IVY_TEST_NODE").unwrap_or_else(|| "node".into());
    let mut child = Command::new(node)
        .current_dir(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."))
        .args([
            "--disable-warning=ExperimentalWarning",
            "tests/fixtures/hive-network-oracle.mjs",
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
        .write_all(encode(&IvyJson::Array(cases.clone())).as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "Original network oracle failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let original = json(&String::from_utf8(output.stdout).unwrap());
    let expected = original.as_array().unwrap();
    assert_eq!(expected.len(), cases.len());
    for (index, (input, original)) in cases.iter().zip(expected).enumerate() {
        let (actual, expected) = match text(input, "kind").as_str() {
            "routing" => (routing(input), original.clone()),
            "diagnostics" => (
                diagnostics(input, original.get("profile").unwrap()),
                original.get("value").unwrap().clone(),
            ),
            _ => (send(input), original.clone()),
        };
        if let Some(path) = std::env::var_os("IVY_HIVE_NETWORK_CORPUS") {
            let _guard = CORPUS.lock().unwrap();
            use std::fs::OpenOptions;
            let mut file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .unwrap();
            writeln!(
                file,
                "{}",
                encode(&obj([
                    ("label", label.into()),
                    ("index", IvyJson::Number(index as f64)),
                    ("input", input.clone()),
                    ("original", original.clone()),
                    ("native", actual.clone())
                ]))
            )
            .unwrap();
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

#[test]
fn original_live_routing_selection_and_precedence() {
    let mut cases = vec![scenario(vec![prepare(context(), params())], true)];
    for mask in 0..16 {
        for explicit in [false, true] {
            let mut first = update("first", 1.0);
            for (index, key) in ["connected", "synced", "ready", "desiredEnabled"]
                .iter()
                .enumerate()
            {
                first
                    .get_mut("node")
                    .unwrap()
                    .insert(*key, (mask & (1 << index) != 0).into());
            }
            let mut p = params();
            if explicit {
                p.insert("serviceNodeId", "first".into());
            }
            cases.push(scenario(vec![first, prepare(context(), p)], true));
        }
    }
    for (key, value) in [
        ("serviceNodeId", "absent"),
        ("hostId", "other"),
        ("serviceName", "other"),
        ("expectedCallerPrincipalId", "changed"),
        ("expectedDefinitionHash", "changed"),
        ("qualifiedName", "absent.call"),
    ] {
        let mut p = params();
        p.insert(key, value.into());
        cases.push(scenario(
            vec![update("first", 1.0), prepare(context(), p)],
            true,
        ));
    }
    for explicit in [false, true] {
        let mut p = params();
        p.insert(
            "resourceRef",
            json("{\"namespace\":\"other\",\"serviceNodeId\":\"other\"}"),
        );
        if explicit {
            p.insert("serviceNodeId", "first".into());
        }
        cases.push(scenario(
            vec![update("first", 1.0), prepare(context(), p)],
            true,
        ));
    }
    let mut p = params();
    p.insert("serviceNodeId", "first".into());
    p.insert("hostId", "other".into());
    let mut first = update("first", 1.0);
    first
        .get_mut("node")
        .unwrap()
        .insert("connected", false.into());
    cases.push(scenario(vec![first, prepare(context(), p)], true));
    cases.push(scenario(
        vec![
            update("z", 1.0),
            update("a", 1.0),
            update("z", 2.0),
            prepare(context(), params()),
        ],
        true,
    ));
    let mut bad = context();
    bad.insert("serviceNodeId", "stale".into());
    bad.insert("generation", number(1));
    cases.push(scenario(vec![prepare(bad, params())], true));
    let mut bad = context();
    bad.insert("principalId", "".into());
    cases.push(scenario(vec![prepare(bad, params())], true));
    for mcp in [false, true] {
        let mut first = update("first", 1.0);
        if !mcp {
            first
                .get_mut("catalog")
                .unwrap()
                .get_mut("tools")
                .unwrap()
                .as_array_mut()
                .unwrap()[0] = binding("provider.call", false);
        }
        let mut c = context();
        c.insert("transport", "mcp".into());
        cases.push(scenario(vec![first, prepare(c, params())], true));
    }
    for validate in [false, true] {
        let mut p = params();
        p.insert("arguments", number(0));
        cases.push(scenario(
            vec![update("first", 1.0), prepare(context(), p)],
            validate,
        ));
    }
    let mut p = params();
    p.insert("operationId", "epoch:7".into());
    cases.push(scenario(
        vec![update("first", 1.0), prepare(context(), p)],
        true,
    ));
    let mut p = params();
    p.insert("expectedDefinitionHash", "old".into());
    let mut throwing = scenario(vec![update("first", 1.0), prepare(context(), p)], true);
    throwing.insert("throwRejected", true.into());
    cases.push(throwing);
    compare("routing-selection", cases);
}

#[test]
fn original_live_routing_generation_catalog_and_notifications() {
    let mut cases = vec![];
    let mut save = prepare(context(), params());
    save.insert("save", "call".into());
    let complete = |result: IvyJson| {
        obj([
            ("op", "complete".into()),
            ("call", "call".into()),
            ("result", result),
        ])
    };
    let absent = obj([("op", "update".into()), ("node", node("first"))]);
    let generation = obj([
        ("op", "update".into()),
        ("node", node("first")),
        ("generation", number(2)),
    ]);
    for validate in [false, true] {
        cases.push(scenario(
            vec![
                absent.clone(),
                prepare(context(), params()),
                update("first", 1.0),
                save.clone(),
                absent.clone(),
                complete("valid".into()),
                complete(number(4)),
                generation.clone(),
                prepare(context(), params()),
                complete("late".into()),
            ],
            validate,
        ));
        let mut changed = update("first", 1.0);
        changed
            .get_mut("catalog")
            .unwrap()
            .get_mut("tools")
            .unwrap()
            .as_array_mut()
            .unwrap()[0]
            .get_mut("definition")
            .unwrap()
            .insert("outputSchema", json("{\"type\":\"integer\"}"));
        cases.push(scenario(
            vec![
                update("first", 1.0),
                save.clone(),
                changed,
                complete("dispatched-schema".into()),
            ],
            validate,
        ));
    }
    for op in ["disconnect", "suspend"] {
        for generation in [1, 2] {
            let step = obj([
                ("op", op.into()),
                ("node", "first".into()),
                ("generation", number(generation)),
            ]);
            let mut c = context();
            c.insert("serviceNodeId", "first".into());
            c.insert("generation", number(1));
            cases.push(scenario(
                vec![
                    update("first", 1.0),
                    step,
                    obj([("op", "caller".into()), ("context", c)]),
                    prepare(context(), params()),
                ],
                true,
            ));
        }
    }
    for principals in [json("[]"), json("[\"provider-principal\"]")] {
        cases.push(scenario(
            vec![
                update("first", 1.0),
                obj([("op", "retire".into()), ("principals", principals)]),
                prepare(context(), params()),
            ],
            true,
        ));
    }
    cases.push(scenario(
        vec![
            update("first", 1.0),
            obj([("op", "clear".into())]),
            prepare(context(), params()),
        ],
        true,
    ));
    for validate in [false, true] {
        for payload in [number(1), "bad".into()] {
            for registered in [false, true] {
                let mut c = context();
                c.insert("serviceNodeId", "first".into());
                c.insert("generation", number(1));
                let mut p =
                    json("{\"namespace\":\"provider\",\"name\":\"changed\",\"version\":\"1.0.0\"}");
                p.insert("payload", payload.clone());
                if !registered {
                    p.insert("version", "2.0.0".into());
                }
                cases.push(scenario(
                    vec![
                        update("first", 1.0),
                        obj([("op", "notification".into()), ("context", c), ("params", p)]),
                    ],
                    validate,
                ));
            }
        }
    }
    for readonly in [true, false] {
        let mut first = update("first", 1.0);
        first
            .get_mut("node")
            .unwrap()
            .insert("serviceName", "dashboards".into());
        first
            .get_mut("catalog")
            .unwrap()
            .get_mut("tools")
            .unwrap()
            .as_array_mut()
            .unwrap()[1]
            .get_mut("definition")
            .unwrap()
            .get_mut("annotations")
            .unwrap()
            .insert("readOnlyHint", readonly.into());
        cases.push(scenario(
            vec![
                first,
                obj([
                    ("op", "dashboard".into()),
                    ("node", "first".into()),
                    ("context", context()),
                    ("args", number(1)),
                ]),
            ],
            true,
        ));
    }
    cases.push(scenario(
        vec![obj([
            ("op", "dashboard".into()),
            ("node", "absent".into()),
            ("context", json("{\"serviceNodeId\":\"stale\"}")),
            ("args", number(1)),
        ])],
        true,
    ));
    compare("routing-lifetime", cases);
}

#[test]
fn original_tool_binding_diagnostic_lifetime() {
    let target = json(
        "{\"callerPrincipalId\":\"caller\",\"serviceNodeId\":\"provider\",\"qualifiedName\":\"provider.call\"}",
    );
    let step = |op: &str, clock: Vec<IvyJson>| {
        obj([
            ("op", op.into()),
            ("target", target.clone()),
            ("clock", IvyJson::Array(clock)),
        ])
    };
    let scenario = |steps: Vec<IvyJson>, throws: bool| {
        obj([
            ("kind", "diagnostics".into()),
            ("throwRecord", throws.into()),
            ("steps", IvyJson::Array(steps)),
        ])
    };
    let mut cases = vec![];
    for throws in [false, true] {
        cases.push(scenario(
            vec![
                step("completed", vec![]),
                step("rejected", vec![number(0)]),
                step("rejected", vec![number(29_999), number(29_999)]),
                step("rejected", vec![number(30_000), number(30_000)]),
                step("completed", vec![number(31_000)]),
                step("completed", vec![]),
                step("rejected", vec![number(32_000)]),
            ],
            throws,
        ));
    }
    cases.push(scenario(
        vec![
            step("rejected", vec![number(0)]),
            step("rejected", vec![IvyJson::Number(8_640_000_000_000_001.0)]),
            step("completed", vec![number(1)]),
        ],
        false,
    ));
    cases.push(scenario(
        vec![
            step("rejected", vec![number(0)]),
            step("completed", vec![IvyJson::Number(8_640_000_000_000_001.0)]),
            step("completed", vec![]),
            step("rejected", vec![number(2)]),
        ],
        false,
    ));
    for last in [
        "not-a-date",
        "1970-01-01T00:00:00.000Z",
        "1970-01-01T00:00:40.000Z",
    ] {
        for status in ["current", "resolved"] {
            let row = obj([
                ("code", "tool_definition_changed".into()),
                ("source", "hive.tool-bindings".into()),
                ("resource", target.clone()),
                ("firstObservedAt", "retained".into()),
                ("lastObservedAt", last.into()),
                ("status", status.into()),
                ("extra", "retained-field".into()),
            ]);
            cases.push(scenario(
                vec![
                    obj([
                        ("op", "restore".into()),
                        ("values", IvyJson::Array(vec![row])),
                    ]),
                    step("rejected", vec![number(30_000), number(30_000)]),
                    step("completed", vec![number(31_000)]),
                ],
                false,
            ));
        }
    }
    let mut reordered = step("rejected", vec![number(1), number(1)]);
    reordered.insert("target",json("{\"qualifiedName\":\"provider.call\",\"serviceNodeId\":\"provider\",\"callerPrincipalId\":\"caller\"}"));
    cases.push(scenario(
        vec![
            step("rejected", vec![number(0)]),
            reordered,
            step("completed", vec![number(2)]),
        ],
        false,
    ));
    compare("diagnostic-lifetime", cases);
}

#[test]
fn original_websocket_send_boundary() {
    let mut cases = vec![];
    let frame = json("{\"jsonrpc\":\"2.0\",\"id\":\"reply\",\"result\":\"😀 é \\ud800\"}");
    for value in [
        IvyJson::Null,
        true.into(),
        "text".into(),
        json("[1,2]"),
        json("{\"2\":2,\"1\":1,\"name\":\"ordered\"}"),
    ] {
        cases.push(obj([("kind", "send".into()), ("value", value)]));
    }
    for closed in [false, true] {
        for callback in [false, true] {
            for throws in [false, true] {
                cases.push(obj([
                    ("kind", "send".into()),
                    ("value", frame.clone()),
                    ("closed", closed.into()),
                    ("callbackError", callback.into()),
                    ("throwSend", throws.into()),
                ]));
            }
        }
    }
    for id in [
        IvyJson::Null,
        "".into(),
        "id".into(),
        number(0),
        IvyJson::Number(-0.0),
        number(-1),
        IvyJson::Number(9_007_199_254_740_991.0),
        IvyJson::Number(9_007_199_254_740_992.0),
        IvyJson::Number(0.5),
        true.into(),
        json("{}"),
        "x".repeat(256).into(),
        "x".repeat(257).into(),
        "😀".repeat(128).into(),
        "😀".repeat(129).into(),
    ] {
        let mut value = frame.clone();
        value.insert("id", id);
        cases.push(obj([
            ("kind", "send".into()),
            ("value", value),
            ("nonfinite", true.into()),
        ]));
    }
    let mut missing = frame.clone();
    missing.remove("id");
    cases.push(obj([
        ("kind", "send".into()),
        ("value", missing),
        ("nonfinite", true.into()),
    ]));
    for size in [33_554_432, 33_554_433] {
        cases.push(obj([
            ("kind", "send".into()),
            (
                "value",
                json("{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":\"\"}"),
            ),
            ("fillBytes", number(size)),
        ]));
    }
    // Overflow, rather than domain validation, enters send's fallback. The
    // integer and UTF-16 boundaries below qualify correlation or termination.
    for id in [
        IvyJson::Null,
        "".into(),
        IvyJson::Number(0.0),
        "😀".repeat(128).into(),
        "😀".repeat(129).into(),
    ] {
        cases.push(obj([
            ("kind", "send".into()),
            (
                "value",
                obj([("jsonrpc", "2.0".into()), ("id", id), ("result", "".into())]),
            ),
            ("fillBytes", IvyJson::Number(33_554_433.0)),
        ]));
    }
    let bytes = encode(&frame).len() as f64;
    for delta in [-1.0, 0.0, 1.0] {
        cases.push(obj([
            ("kind", "send".into()),
            ("value", frame.clone()),
            ("buffered", IvyJson::Number(67_108_864.0 - bytes + delta)),
        ]));
    }
    for encoded in ["", "pre-encoded é"] {
        cases.push(obj([
            ("kind", "send".into()),
            ("value", frame.clone()),
            ("nonfinite", true.into()),
            ("encoded", encoded.into()),
        ]));
    }
    compare("websocket-send", cases);
}
