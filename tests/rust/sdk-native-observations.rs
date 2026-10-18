use ivy_contracts::{IvyJson, IvyResult, json::parse, native_contract::NativeContract};
use ivy_sdk::native_observations::NativeObservations;
fn capture(result: IvyResult<IvyJson>) -> IvyJson {
    match result {
        Ok(value) => IvyJson::object([("ok", true.into()), ("value", value)]),
        Err(error) => IvyJson::object([("ok", false.into()), ("error", error.to_wire())]),
    }
}
#[test]
fn retained_observation_layouts_match_typescript() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let output = std::process::Command::new("node")
        .args([
            "--experimental-transform-types",
            "--disable-warning=ExperimentalWarning",
            "tests/fixtures/native-observations-oracle.mjs",
        ])
        .current_dir(&root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rows: Vec<serde_json::Value> = serde_json::from_slice(&output.stdout).unwrap();
    assert!(rows.len() > 40);
    let mut count = 0;
    for row in rows {
        let source = if let Some(path) = row["path"].as_str() {
            std::fs::read_to_string(root.join(path)).unwrap()
        } else {
            row["source"].as_str().unwrap().into()
        };
        let contract = NativeContract::new(&parse(&source, usize::MAX).unwrap()).unwrap();
        let observations = NativeObservations::new(&contract);
        let label = row["path"].as_str().or(row["label"].as_str()).unwrap();
        for call in row["calls"].as_array().unwrap() {
            let input = parse(call["input"].as_str().unwrap(), usize::MAX).unwrap();
            let expected = parse(call["expected"].as_str().unwrap(), usize::MAX).unwrap();
            let invoke = || {
                if call["kind"].as_str() == Some("thread") {
                    observations.native_thread_state(&input).map(|v| {
                        IvyJson::object([
                            ("thread", v.thread),
                            ("state", IvyJson::String(v.state)),
                            ("canStartTurn", v.can_start_turn.into()),
                        ])
                    })
                } else {
                    let turn = parse(call["turnId"].as_str().unwrap(), usize::MAX)
                        .unwrap()
                        .into_js_string()
                        .unwrap();
                    observations
                        .native_turn_items(&input, &turn)
                        .map(IvyJson::Array)
                }
            };
            assert_eq!(capture(invoke()), expected, "{label}: {call}");
            // Returned data belongs to the caller; mutation cannot poison cached layouts.
            if let Ok(mut value) = invoke() {
                match &mut value {
                    IvyJson::Object(_) => value.insert("changed", true.into()),
                    IvyJson::Array(items) => items.clear(),
                    _ => unreachable!(),
                }
            }
            assert_eq!(capture(invoke()), expected, "cached {label}: {call}");
            count += 1;
        }
    }
    assert!(count > 4000);
}
