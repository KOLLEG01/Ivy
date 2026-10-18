use ivy_contracts::{IvyJson, IvyResult, JsString, json::parse, native_contract::NativeContract};
use ivy_sdk::{
    native_evidence::{NativePayloadLimits, validate_native_payload},
    native_parameters::*,
    native_plan::*,
};
fn capture(result: IvyResult<IvyJson>) -> IvyJson {
    match result {
        Ok(value) => IvyJson::object([("ok", true.into()), ("value", value)]),
        Err(error) => IvyJson::object([("ok", false.into()), ("error", error.to_wire())]),
    }
}
fn call(contract: &NativeContract, input: &IvyJson) -> IvyResult<IvyJson> {
    let mut value = input.get("value").unwrap().clone();
    if let Some(generated) = input.get("generated").and_then(IvyJson::as_js_string) {
        let units = input.get("units").unwrap().as_f64().unwrap() as usize;
        let key = if generated == &JsString::from("large-draft") {
            "threadStart"
        } else {
            "params"
        };
        let mut params = value.get(key).unwrap().clone();
        params.insert(
            if generated == &JsString::from("large-payload") {
                "body"
            } else {
                "extra"
            },
            "x".repeat(units).into(),
        );
        value.insert(key, params);
    }
    let method = input
        .get("method")
        .and_then(IvyJson::as_js_string)
        .cloned()
        .unwrap_or_else(|| "turn/interrupt".into());
    let reserved: Vec<_> = input
        .get("reserved")
        .and_then(IvyJson::as_array)
        .unwrap_or(&[])
        .iter()
        .map(|v| v.as_js_string().unwrap().clone())
        .collect();
    let maximum = input
        .get("maximumBytes")
        .and_then(IvyJson::as_f64)
        .unwrap_or(1048576.0) as usize;
    let kind = if input.get("planKind").and_then(IvyJson::as_js_string)
        == Some(&JsString::from("task-board"))
    {
        NativePlanKind::TaskBoard
    } else {
        NativePlanKind::Chat
    };
    let parent = input.get("parentId").and_then(IvyJson::as_js_string);
    match input.get("kind").unwrap().as_str()?.as_str() {
        "input" => contract.validate_input(&method, &value, maximum)?,
        "result" => contract.validate_result(&method, &value, maximum)?,
        "template" => contract.validate_template(&method, &value, &reserved, maximum)?,
        "invocation" => validate_native_invocation(contract, &value, maximum)?,
        "draft" => validate_native_plan_draft(contract, &value, kind)?,
        "params-save" => {
            return save_native_parameters(contract, &method, &value, &reserved, parent);
        }
        "params-read" => {
            return read_native_parameters(contract, &method, &value, &reserved, parent);
        }
        "plan-save" => {
            return save_native_plan(
                contract,
                &value,
                kind,
                parent,
                &input
                    .get("mutationId")
                    .and_then(IvyJson::as_js_string)
                    .cloned()
                    .unwrap_or_else(|| "plan".into()),
            );
        }
        "plan-read" => return read_native_plan(contract, &value, kind, parent),
        "payload" => validate_native_payload(
            contract,
            &value,
            NativePayloadLimits {
                request_bytes: input
                    .get("requestBytes")
                    .and_then(IvyJson::as_f64)
                    .unwrap_or(6291456.0) as usize,
                response_bytes: input
                    .get("responseBytes")
                    .and_then(IvyJson::as_f64)
                    .unwrap_or(25165824.0) as usize,
            },
        )?,
        _ => panic!("Unknown oracle action"),
    }
    Ok(IvyJson::Null)
}
#[test]
fn exact_native_plan_parameters_invocation_and_payload_match_original_sdk() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let output = std::process::Command::new("node")
        .args([
            "--experimental-transform-types",
            "--disable-warning=ExperimentalWarning",
            "tests/fixtures/native-plan-oracle.mjs",
        ])
        .current_dir(&root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let groups: Vec<serde_json::Value> = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(groups.len(), 5);
    let mut count = 0;
    for group in groups {
        let source = group["source"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| {
                std::fs::read_to_string(root.join(group["path"].as_str().unwrap())).unwrap()
            });
        let contract = NativeContract::new(&parse(&source, usize::MAX).unwrap()).unwrap();
        for row in group["rows"].as_array().unwrap() {
            let input = parse(row["input"].as_str().unwrap(), usize::MAX).unwrap();
            let expected = parse(row["expected"].as_str().unwrap(), usize::MAX).unwrap();
            assert_eq!(
                capture(call(&contract, &input)),
                expected,
                "{}: {} input {}",
                group["path"].as_str().or(group["label"].as_str()).unwrap(),
                row["name"],
                row["input"]
            );
            count += 1;
        }
    }
    assert!(count > 1000);
}
