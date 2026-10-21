use ivy_contracts::{
    IvyJson, JsString,
    json::{parse, stringify},
};
use ivy_hive::admission::{
    AuthenticationRate, AuthenticationRateOptions, RequestBudget, RequestBudgetOptions,
    RequestLease,
};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    io::Write,
    path::Path,
    process::{Command, Stdio},
};

fn number(value: &Value) -> f64 {
    match value.get("$number").and_then(Value::as_str) {
        Some("nan") => f64::NAN,
        Some("infinity") => f64::INFINITY,
        Some("negativeInfinity") => f64::NEG_INFINITY,
        Some("negativeZero") => -0.0,
        Some(other) => panic!("Unknown fixture number: {other}"),
        None => value.as_f64().expect("Numeric fixture input"),
    }
}
fn option(value: &Value, name: &str, default: f64) -> f64 {
    value.get(name).map_or(default, number)
}
fn text(value: &Value, name: &str) -> JsString {
    match value.get(format!("{name}Units")) {
        Some(units) => JsString(
            units
                .as_array()
                .unwrap()
                .iter()
                .map(|v| u16::try_from(v.as_u64().unwrap()).unwrap())
                .collect(),
        ),
        None => value[name].as_str().unwrap().into(),
    }
}
fn rate(options: &Value) -> AuthenticationRate {
    let defaults = AuthenticationRateOptions::default();
    AuthenticationRate::new(AuthenticationRateOptions {
        window_ms: option(options, "windowMs", defaults.window_ms),
        maximum: option(options, "maximum", defaults.maximum),
        per_client: option(options, "perClient", defaults.per_client),
        client_maximum: option(options, "clientMaximum", defaults.client_maximum),
    })
}
fn budget(options: &Value) -> RequestBudget {
    let defaults = RequestBudgetOptions::default();
    RequestBudget::new(RequestBudgetOptions {
        maximum: option(options, "maximum", defaults.maximum),
        maximum_bytes: option(options, "maximumBytes", defaults.maximum_bytes),
        per_consumer: option(options, "perConsumer", defaults.per_consumer),
        per_consumer_bytes: option(options, "perConsumerBytes", defaults.per_consumer_bytes),
        name: if options.get("name").is_some() || options.get("nameUnits").is_some() {
            text(options, "name")
        } else {
            defaults.name
        },
    })
}
fn compare(traces: Vec<Value>) {
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let mut oracle =
        Command::new(std::env::var_os("IVY_TEST_NODE").unwrap_or_else(|| "node".into()))
            .current_dir(source)
            .arg("tests/fixtures/hive-admission-oracle.mjs")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
    oracle
        .stdin
        .take()
        .unwrap()
        .write_all(serde_json::to_string(&traces).unwrap().as_bytes())
        .unwrap();
    let output = oracle.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let expected = parse(std::str::from_utf8(&output.stdout).unwrap(), usize::MAX).unwrap();
    assert_eq!(expected.as_array().unwrap().len(), traces.len());
    let mut now = 1000.0;
    for (index, trace) in traces.iter().enumerate() {
        let mut authentication = rate(&trace["options"]);
        let mut reservations = budget(&trace["options"]);
        let mut leases = HashMap::<u64, RequestLease>::new();
        let expected = &expected.as_array().unwrap()[index];
        assert_eq!(
            expected.get("name"),
            Some(&IvyJson::from(trace["name"].as_str().unwrap()))
        );
        let expected = expected.get("rows").unwrap().as_array().unwrap();
        assert_eq!(expected.len(), trace["steps"].as_array().unwrap().len());
        for (index, step) in trace["steps"].as_array().unwrap().iter().enumerate() {
            let op = step["op"].as_str().unwrap();
            let id = step["id"].as_u64().unwrap_or(0);
            let actual = if matches!(op, "grow" | "release" | "drop") && !leases.contains_key(&id) {
                assert_eq!(
                    step["optional"], true,
                    "Fixture references an unavailable lease"
                );
                IvyJson::object([("skipped", IvyJson::Bool(true))])
            } else {
                let result = match op {
                    "clock" => {
                        now = number(&step["now"]);
                        Ok(())
                    }
                    "admit" => authentication
                        .admit(&text(step, "key"), step.get("now").map_or(now, number)),
                    "acquire" => {
                        assert!(!leases.contains_key(&id), "Fixture reuses a lease identity");
                        reservations
                            .acquire(&text(step, "key"), step.get("bytes").map(number))
                            .map(|lease| {
                                leases.insert(id, lease);
                            })
                    }
                    "grow" => leases.get_mut(&id).unwrap().grow(number(&step["bytes"])),
                    "release" => {
                        leases.get_mut(&id).unwrap().release();
                        Ok(())
                    }
                    "drop" => {
                        leases.remove(&id);
                        Ok(())
                    }
                    "reset" => {
                        if trace["kind"] == "rate" {
                            authentication = rate(&step["options"])
                        } else {
                            reservations = budget(&step["options"])
                        };
                        Ok(())
                    }
                    _ => panic!("Unknown trace operation"),
                };
                match result {
                    Ok(()) => IvyJson::object([("ok", IvyJson::Bool(true))]),
                    Err(error) => IvyJson::object([("error", error.to_wire())]),
                }
            };
            // JSON.stringify renders nonfinite diagnostic numbers as null and
            // retains unpaired UTF-16 in strings; use the real shared codec.
            let actual = parse(&stringify(&actual, usize::MAX).unwrap(), usize::MAX).unwrap();
            assert_eq!(
                actual, expected[index],
                "{} step {index}: {step}",
                trace["name"]
            );
        }
    }
}
fn trace(name: &str, kind: &str, options: Value, steps: Vec<Value>) -> Value {
    json!({"name":name,"kind":kind,"options":options,"steps":steps})
}
fn admit(key: &str, now: i64) -> Value {
    json!({"op":"admit","key":key,"now":now})
}
fn acquire(id: u64, key: &str, bytes: i64) -> Value {
    json!({"op":"acquire","id":id,"key":key,"bytes":bytes})
}

#[test]
fn authentication_windows_identity_capacity_and_error_precedence_match_original() {
    let mut traces = vec![
        trace(
            "existing arrival acceptance",
            "rate",
            json!({"windowMs":1000,"maximum":3,"perClient":2,"clientMaximum":3}),
            vec![
                admit("a", 1000),
                admit("a", 1000),
                admit("a", 1001),
                admit("b", 1001),
                admit("c", 1001),
                admit("a", 2000),
                admit("new", 2001),
            ],
        ),
        trace(
            "existing client capacity acceptance",
            "rate",
            json!({"windowMs":1000,"maximum":10,"perClient":10,"clientMaximum":2}),
            vec![
                admit("a", 1000),
                admit("b", 1000),
                admit("c", 1000),
                admit("c", 2000),
            ],
        ),
        trace(
            "independent global and client clocks",
            "rate",
            json!({"windowMs":10,"maximum":3,"perClient":2,"clientMaximum":3}),
            vec![
                admit("a", 9),
                admit("a", 10),
                admit("a", 11),
                admit("b", 18),
                admit("b", 19),
                admit("c", 20),
                admit("a", 20),
                admit("b", 0),
                admit("a", -50),
                admit("b", 30),
            ],
        ),
        trace(
            "rejections populate bounded identities",
            "rate",
            json!({"windowMs":10,"maximum":0,"perClient":0,"clientMaximum":2}),
            vec![
                admit("a", 0),
                admit("b", 0),
                admit("a", 1),
                admit("c", 1),
                admit("c", 9),
                admit("c", 10),
                admit("a", 10),
            ],
        ),
        trace(
            "captured default clock and UTF16 identities",
            "rate",
            json!({"maximum":5,"perClient":1}),
            vec![
                json!({"op":"clock","now":0}),
                json!({"op":"admit","keyUnits":[55296]}),
                json!({"op":"admit","key":"�"}),
                json!({"op":"admit","keyUnits":[55296]}),
                json!({"op":"admit","keyUnits":[56320]}),
                json!({"op":"clock","now":1000}),
                json!({"op":"admit","keyUnits":[55296]}),
            ],
        ),
    ];
    for options in [
        json!({"windowMs":0}),
        json!({"windowMs":-1}),
        json!({"windowMs":{"$number":"nan"}}),
        json!({"clientMaximum":0}),
        json!({"clientMaximum":{"$number":"nan"}}),
    ] {
        traces.push(trace(
            "numeric window/config boundary",
            "rate",
            options,
            vec![
                admit("__proto__", 0),
                admit("constructor", 0),
                admit("__proto__", 1),
                admit("", -1),
                admit("toString", 1001),
            ],
        ));
    }
    let mut steps = (0..4097)
        .map(|id| admit(&format!("id{id}"), 1000))
        .collect::<Vec<_>>();
    steps.extend([
        admit("expired", 1999),
        admit("expired", 2000),
        admit("expired", 2000),
    ]);
    traces.push(trace(
        "default bounded identity table",
        "rate",
        json!({}),
        steps,
    ));
    let mut steps = (0..257).map(|_| admit("same", 1000)).collect::<Vec<_>>();
    steps.extend((0..257).map(|id| admit(&format!("other{id}"), 1000)));
    steps.extend([admit("same", 1999), admit("same", 2000)]);
    traces.push(trace(
        "default per-client and global bounds",
        "rate",
        json!({}),
        steps,
    ));
    compare(traces);
}

#[test]
fn authentication_interleaving_expiry_and_backward_clocks_match_original() {
    let mut traces = vec![];
    let mut seed = 0x12345678u64;
    for options in [
        json!({"windowMs":1000,"maximum":11,"perClient":3,"clientMaximum":7}),
        json!({"windowMs":7,"maximum":4,"perClient":2,"clientMaximum":5}),
        json!({}),
    ] {
        let mut steps = vec![];
        let mut now = 0i64;
        for index in 0..12_000 {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223) & 0xffff_ffff;
            now += match seed % 13 {
                0 => -1000,
                1 => 1000,
                2 => 999,
                3 => 1,
                _ => 0,
            };
            let mut step = admit(&format!("key{}", (seed >> 8) % 19), now);
            if index % 101 == 0 {
                step["keyUnits"] = json!([55296 + (index % 2) as u16]);
            }
            steps.push(step);
        }
        traces.push(trace(
            "deterministic lazy-expiry trace",
            "rate",
            options,
            steps,
        ));
    }
    compare(traces);
}

#[test]
fn request_slots_bytes_release_identity_rounding_and_utf16_details_match_original() {
    let mut traces = vec![
        trace(
            "existing reservation acceptance",
            "budget",
            json!({"maximum":3,"maximumBytes":100,"perConsumer":2,"perConsumerBytes":60}),
            vec![
                acquire(1, "a", 30),
                acquire(2, "a", 30),
                acquire(3, "a", 0),
                json!({"op":"grow","id":1,"bytes":1}),
                acquire(4, "b", 40),
                json!({"op":"grow","id":4,"bytes":1}),
                json!({"op":"release","id":1}),
                json!({"op":"release","id":1}),
                json!({"op":"release","id":2}),
                json!({"op":"release","id":4}),
                acquire(5, "a", 60),
                json!({"op":"release","id":5}),
            ],
        ),
        trace(
            "slot guard precedes invalid bytes",
            "budget",
            json!({"maximum":1,"maximumBytes":10,"perConsumer":1,"perConsumerBytes":10}),
            vec![
                acquire(1, "a", 10),
                acquire(2, "a", -1),
                acquire(3, "b", -1),
                json!({"op":"grow","id":1,"bytes":-1}),
                json!({"op":"grow","id":1,"bytes":1}),
                json!({"op":"release","id":1}),
                acquire(4, "b", -1),
                acquire(5, "b", 11),
                acquire(6, "b", 10),
            ],
        ),
        trace(
            "failed reservation leaves no charge",
            "budget",
            json!({"maximum":4,"maximumBytes":10,"perConsumer":2,"perConsumerBytes":6}),
            vec![
                acquire(1, "a", 7),
                acquire(2, "a", 6),
                acquire(3, "b", 5),
                acquire(4, "b", 4),
                json!({"op":"grow","id":2,"bytes":1}),
                json!({"op":"grow","id":4,"bytes":1}),
                json!({"op":"release","id":2}),
                acquire(5, "a", 6),
            ],
        ),
        trace(
            "old leases retain old budget identity",
            "budget",
            json!({"maximum":1}),
            vec![
                acquire(1, "a", 1),
                json!({"op":"reset","options":{"maximum":1}}),
                acquire(2, "a", 1),
                json!({"op":"release","id":1}),
                acquire(3, "b", 0),
                json!({"op":"release","id":2}),
                acquire(4, "b", 0),
            ],
        ),
        trace(
            "dropping a handle does not refund accepted work",
            "budget",
            json!({"maximum":1}),
            vec![
                acquire(1, "a", 1),
                json!({"op":"drop","id":1}),
                acquire(2, "b", 0),
            ],
        ),
        trace(
            "default initial reservation",
            "budget",
            json!({"maximumBytes":1024,"perConsumerBytes":1024}),
            vec![
                json!({"op":"acquire","id":1,"key":"a"}),
                acquire(2, "b", 1),
                json!({"op":"release","id":1}),
                json!({"op":"acquire","id":3,"key":"b"}),
            ],
        ),
        trace(
            "default retained byte limits",
            "budget",
            json!({}),
            vec![
                acquire(1, "a", 67_108_863),
                json!({"op":"grow","id":1,"bytes":1}),
                json!({"op":"grow","id":1,"bytes":1}),
                acquire(2, "b", 67_108_864),
                acquire(3, "c", 1),
                json!({"op":"grow","id":1,"bytes":1}),
                json!({"op":"release","id":1}),
                json!({"op":"grow","id":2,"bytes":1}),
                json!({"op":"release","id":2}),
                acquire(4, "a", 67_108_864),
            ],
        ),
        trace(
            "UTF16 client identity",
            "budget",
            json!({"perConsumer":1}),
            vec![
                json!({"op":"acquire","id":1,"keyUnits":[55296],"bytes":0}),
                acquire(2, "�", 0),
                json!({"op":"acquire","id":3,"keyUnits":[55296],"bytes":0}),
                json!({"op":"release","id":1}),
                json!({"op":"acquire","id":4,"keyUnits":[55296],"bytes":0}),
            ],
        ),
        trace(
            "safe individual amounts retain aggregate IEEE rounding",
            "budget",
            json!({"maximumBytes":{"$number":"infinity"},"perConsumerBytes":{"$number":"infinity"}}),
            vec![
                acquire(1, "a", 9_007_199_254_740_991),
                json!({"op":"grow","id":1,"bytes":2}),
                acquire(2, "b", 1),
                json!({"op":"release","id":1}),
                json!({"op":"release","id":2}),
                json!({"op":"reset","options":{"maximumBytes":0}}),
                acquire(3, "c", 1),
            ],
        ),
    ];
    for name in [
        json!({"name":"authentication"}),
        json!({"name":"connection"}),
        json!({"name":""}),
        json!({"nameUnits":[55357,56832,55296,0,65533]}),
    ] {
        let mut options =
            json!({"maximum":1,"maximumBytes":1,"perConsumer":1,"perConsumerBytes":1});
        options
            .as_object_mut()
            .unwrap()
            .extend(name.as_object().unwrap().clone());
        traces.push(trace(
            "named slot and byte errors",
            "budget",
            options,
            vec![
                acquire(1, "a", 1),
                acquire(2, "b", 0),
                json!({"op":"grow","id":1,"bytes":1}),
                json!({"op":"release","id":1}),
                acquire(3, "a", 2),
            ],
        ));
    }
    let mut options = json!({"maximum":0});
    options["nameUnits"] = json!([vec![120; 2042], vec![55357, 56832, 55296]].concat());
    traces.push(trace(
        "wire message code-unit truncation",
        "budget",
        options,
        vec![acquire(1, "a", 0)],
    ));
    let mut steps = vec![acquire(1, "a", 0)];
    for bytes in [
        json!(-1),
        json!(0.5),
        json!(9_007_199_254_740_992u64),
        json!({"$number":"nan"}),
        json!({"$number":"infinity"}),
        json!({"$number":"negativeInfinity"}),
        json!({"$number":"negativeZero"}),
    ] {
        steps.push(json!({"op":"grow","id":1,"bytes":bytes}));
    }
    steps.extend([
        json!({"op":"release","id":1}),
        json!({"op":"grow","id":1,"bytes":0}),
        json!({"op":"grow","id":1,"bytes":{"$number":"nan"}}),
        json!({"op":"release","id":1}),
        acquire(2, "a", 0),
    ]);
    traces.push(trace(
        "numeric reservation validation and release",
        "budget",
        json!({}),
        steps,
    ));
    let mut steps = vec![];
    for (id, bytes) in [
        json!(-1),
        json!(0.5),
        json!(9_007_199_254_740_992u64),
        json!({"$number":"nan"}),
        json!({"$number":"infinity"}),
        json!({"$number":"negativeInfinity"}),
    ]
    .into_iter()
    .enumerate()
    {
        steps.push(json!({"op":"acquire","id":id,"key":"a","bytes":bytes}));
    }
    steps.extend([
        acquire(10, "a", 1),
        acquire(11, "a", 1),
        acquire(12, "a", 1),
        json!({"op":"release","id":10}),
        acquire(13, "a", 1),
    ]);
    traces.push(trace(
        "failed initial numeric reservations do not charge slots",
        "budget",
        json!({"perConsumer":2}),
        steps,
    ));
    let mut steps = (0..257)
        .map(|id| acquire(id, "same", 0))
        .collect::<Vec<_>>();
    steps.extend((0..256).map(|id| json!({"op":"release","id":id})));
    steps.push(acquire(258, "same", 0));
    traces.push(trace("default consumer slots", "budget", json!({}), steps));
    let mut steps = (0..2049)
        .map(|id| acquire(id, &format!("key{id}"), 0))
        .collect::<Vec<_>>();
    steps.extend((0..2048).map(|id| json!({"op":"release","id":id})));
    steps.push(acquire(2050, "recovered", 1));
    traces.push(trace("default global slots", "budget", json!({}), steps));
    compare(traces);
}

#[test]
fn request_reservation_interleaving_and_failed_growth_match_original() {
    let mut traces = vec![];
    let mut seed = 0x9abcdef0u64;
    for options in [
        json!({"maximum":7,"maximumBytes":31,"perConsumer":2,"perConsumerBytes":11}),
        json!({"maximum":2,"maximumBytes":0,"perConsumer":1,"perConsumerBytes":0}),
        json!({"maximum":3,"maximumBytes":5000,"perConsumer":2,"perConsumerBytes":3000}),
    ] {
        let mut steps = vec![];
        let mut id = 0u64;
        for index in 0..10_000 {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223) & 0xffff_ffff;
            let previous = if id == 0 {
                0
            } else {
                id.saturating_sub(16) + (seed >> 8) % id.min(16)
            };
            let step = match seed % 7 {
                0 if id > 0 => {
                    json!({"op":"grow","id":previous,"bytes":((seed>>16)%23) as i64-2,"optional":true})
                }
                1 if id > 0 => json!({"op":"release","id":previous,"optional":true}),
                2 if id > 0 => json!({"op":"drop","id":previous,"optional":true}),
                _ => {
                    let step = acquire(
                        id,
                        &format!("key{}", (seed >> 12) % 5),
                        ((seed >> 18) % 17) as i64,
                    );
                    id += 1;
                    step
                }
            };
            steps.push(step);
            if index % 31 == 30 {
                steps.push(json!({"op":"reset","options":options}));
            }
        }
        traces.push(trace(
            "deterministic provider-lifetime trace",
            "budget",
            options,
            steps,
        ));
    }
    compare(traces);
}
