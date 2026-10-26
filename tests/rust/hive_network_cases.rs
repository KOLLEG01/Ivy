use super::*;

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
        first
            .get_mut("catalog")
            .unwrap()
            .get_mut("tools")
            .unwrap()
            .as_array_mut()
            .unwrap()[0]
            .get_mut("definition")
            .unwrap()
            .insert(
                "discovery",
                if mcp {
                    json("{\"mcp\":{\"name\":\"provider_call\"}}")
                } else {
                    obj([])
                },
            );
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
