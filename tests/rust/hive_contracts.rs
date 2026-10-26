use super::{Root, native, reference as oracle};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};

fn definition() -> Value {
    json!({"key":"fixture/content","version":"1.0.0","owner":{"kind":"agent"},"mediaType":"application/json","retention":{"objects":{"mode":"retain"},"revisions":{"mode":"current"}},"specMarkdown":"fixture","jsonSchema":true})
}
fn register(definition: Value) -> Value {
    json!({"op":"register","owner":definition["owner"],"definition":definition})
}
fn compare(steps: &[Value]) -> Value {
    let root = Root::new();
    let expected = oracle(&root.0.join("reference.sqlite"), steps);
    let actual = native(&root.0.join("native.sqlite"), steps);
    assert_eq!(actual["rows"].as_array().unwrap().len(), steps.len());
    for (i, (left, right)) in actual["rows"]
        .as_array()
        .unwrap()
        .iter()
        .zip(expected["rows"].as_array().unwrap())
        .enumerate()
    {
        assert_eq!(
            left, right,
            "Contract/content trace step {i}, {}",
            steps[i]["op"]
        );
    }
    assert_eq!(actual["definitions"], expected["definitions"]);
    actual
}
fn sql(text: &str) -> Value {
    json!({"op":"sql","sql":text})
}
fn get(version: Option<&str>) -> Value {
    let mut value = json!({"op":"getContract","key":"fixture/content"});
    if let Some(version) = version {
        value["version"] = version.into();
    }
    value
}

#[test]
fn contract_registration_keeps_schema_error_order_immutable_families_and_numeric_versions() {
    let base = definition();
    let mut steps = vec![get(None), register(base.clone()), register(base.clone())];
    let mut changed = base.clone();
    changed["specMarkdown"] = "different".into();
    steps.push(register(changed));
    for mode in [
        "owner",
        "media",
        "owned",
        "schema-first",
        "shared-first",
        "long-version",
    ] {
        let mut value = base.clone();
        match mode {
            "owner" => value["owner"] = json!({"kind":"service","serviceName":"fixture"}),
            "media" => {
                value["mediaType"] = "text/plain".into();
                value.as_object_mut().unwrap().remove("jsonSchema");
            }
            "owned" => value["retention"]["objects"]["mode"] = "owned".into(),
            "schema-first" => {
                value["owner"] = json!({"kind":"service","serviceName":"fixture"});
                value["jsonSchema"] = json!({"unknownKeyword":true});
            }
            "shared-first" => {
                value.as_object_mut().unwrap().remove("retention");
                value["jsonSchema"] = json!({"unknownKeyword":true});
            }
            "long-version" => value["version"] = format!("{}.0.0", "9".repeat(125)).into(),
            _ => unreachable!(),
        }
        steps.push(register(value));
    }
    let mut mismatch = register(base.clone());
    mismatch["owner"] = json!({"kind":"hive"});
    steps.push(mismatch);
    for version in ["2.0.0", "10.0.0", "2.10.0", "2.9.0"] {
        let mut value = base.clone();
        value["version"] = version.into();
        steps.push(register(value));
    }
    let mut expire = base.clone();
    expire["version"] = "11.0.0".into();
    expire["retention"] = json!({"objects":{"mode":"expire","maximumAgeDays":10},"revisions":{"mode":"bounded","maximumCount":3}});
    steps.push(register(expire));
    steps.push(get(None));
    steps.push(get(Some("2.9.0")));
    steps.push(get(Some("8.0.0")));
    let mut huge = base.clone();
    let version = format!("{}.0.0", "9".repeat(124));
    huge["version"] = version.clone().into();
    steps.push(register(huge));
    steps.push(get(None));
    let mut invalid = base.clone();
    invalid["jsonSchema"] = json!({"title":"x".repeat(256*1024)});
    steps.push(register(invalid));
    steps.push(json!({"op":"reopen"}));
    steps.push(get(None));
    let result = compare(&steps);
    assert_eq!(
        result["rows"][3]["error"]["data"]["code"],
        "contract_definition_conflict"
    );
    assert_eq!(
        result["rows"][7]["error"]["data"]["code"],
        "registry_invalid"
    );
    assert_eq!(
        result["rows"].as_array().unwrap().last().unwrap()["value"]["version"],
        version
    );
}

fn agent(definition: Value, nonce: &str) -> Value {
    json!({"op":"registerAgent","context":{"credentialDigest":format!("sha256:{}","a".repeat(64))},"now":100000,"params":{"definition":definition,"mutationId":format!("epoch:1000:{nonce}")}})
}
#[test]
fn agent_contract_receipts_replay_after_restart_and_roll_back_work_and_receipt_failures() {
    let base = definition();
    let first = agent(base.clone(), "one");
    let mut changed = base.clone();
    changed["specMarkdown"] = "changed retry".into();
    let mut atomic = base.clone();
    atomic["key"] = "fixture/atomic".into();
    let retry = agent(atomic.clone(), "atomic");
    let mut wrong = base.clone();
    wrong["key"] = "fixture/wrong-owner".into();
    wrong["owner"] = json!({"kind":"service","serviceName":"fixture"});
    wrong["jsonSchema"] = json!({"bad":true});
    let steps = vec![
        json!({"op":"configure","credentials":[{"principalId":"alpha","digest":format!("sha256:{}","a".repeat(64))}]}),
        sql("UPDATE metadata SET value='epoch' WHERE key='runtime_epoch'"),
        first.clone(),
        first.clone(),
        agent(changed, "one"),
        json!({"op":"reopen"}),
        first,
        agent(wrong, "owner"),
        sql(
            "CREATE TRIGGER contract_failure BEFORE INSERT ON contracts BEGIN SELECT RAISE(ABORT,'fixture'); END;",
        ),
        retry.clone(),
        sql(
            "DROP TRIGGER contract_failure; CREATE TRIGGER receipt_failure BEFORE INSERT ON mutations BEGIN SELECT RAISE(ABORT,'fixture'); END;",
        ),
        retry.clone(),
        sql("DROP TRIGGER receipt_failure"),
        retry.clone(),
        retry,
        json!({"op":"reopen"}),
        get(None),
    ];
    let result = compare(&steps);
    assert_eq!(result["rows"][2]["value"], base);
    assert_eq!(
        result["rows"][4]["error"]["data"]["code"],
        "mutation_conflict"
    );
    for index in [9, 11] {
        assert_eq!(result["rows"][index]["error"]["data"]["outcome"], "unknown");
        assert!(
            !result["rows"][index]["state"]["families"]
                .as_array()
                .unwrap()
                .iter()
                .any(|r| r["key"] == "fixture/atomic")
        );
    }
    assert_eq!(result["rows"][13]["value"], atomic);
}

#[test]
fn trusted_registration_keeps_its_callers_sql_transaction_boundary() {
    let mut value = definition();
    value["key"] = "fixture/direct".into();
    let result = compare(&[
        sql(
            "CREATE TRIGGER contract_failure BEFORE INSERT ON contracts BEGIN SELECT RAISE(ABORT,'fixture'); END;",
        ),
        register(value.clone()),
        sql("DROP TRIGGER contract_failure"),
        register(value),
    ]);
    assert_eq!(result["rows"][1]["error"]["data"]["outcome"], "unknown");
    assert!(
        result["rows"][1]["state"]["families"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["key"] == "fixture/direct")
    );
    assert!(result["rows"][3]["error"].is_null());
}

#[test]
fn contract_sql_string_conversion_preserves_original_utf16_family_and_definition_comparisons() {
    let mut bad_key = register(definition());
    bad_key["definitionUnits"] = json!({"key":[102,105,120,116,117,114,101,47,0xd800]});
    bad_key["lossless"] = true.into();
    let mut collision = definition();
    collision["key"] = "fixture/�".into();
    let mut bad_media = definition();
    bad_media["key"] = "fixture/media".into();
    bad_media["mediaType"] = "application/octet-stream".into();
    bad_media.as_object_mut().unwrap().remove("jsonSchema");
    let mut bad_media = register(bad_media);
    bad_media["definitionUnits"] = json!({"mediaType":[0xd800]});
    bad_media["lossless"] = true.into();
    let result = compare(&[
        bad_key.clone(),
        bad_key,
        register(collision),
        bad_media.clone(),
        bad_media,
    ]);
    assert!(result["rows"][0]["error"].is_null());
    assert!(result["rows"][1]["error"].is_null());
    assert_eq!(
        result["rows"][2]["error"]["data"]["code"],
        "contract_definition_conflict"
    );
    assert!(result["rows"][3]["error"].is_null());
    assert_eq!(
        result["rows"][4]["error"]["data"]["code"],
        "contract_owner_mismatch"
    );
}

fn decode(contract: Value, encoding: &str, value: Value) -> Value {
    json!({"op":"decode","contract":contract,"content":{"encoding":encoding,"value":value}})
}
#[test]
fn content_decoding_uses_the_actual_dynamic_compiler_and_exact_media_and_encoding_rules() {
    let mut steps = vec![];
    let schemas = vec![
        json!(true),
        json!(false),
        json!({"type":"object","properties":{"count":{"type":"integer","minimum":2}},"required":["count"],"additionalProperties":false}),
        json!({"type":["string","null"],"pattern":"^é+$","minLength":1}),
        json!({"$ref":"#/$defs/node","$defs":{"node":{"type":"object","properties":{"next":{"$ref":"#/$defs/node"}},"additionalProperties":false}}}),
        json!({"type":"array","items":{"type":"number"},"uniqueItems":true}),
        json!({"properties":{"x":{"type":"number"}},"if":{"required":["x"]},"then":{"required":["y"]},"else":{"type":"object"}}),
        json!({"$id":"https://fixture.invalid/schema","$defs":{"a":{"$id":"child","type":"number"}},"$ref":"child"}),
        json!({"type":"object","patternProperties":{"^x":{"type":"integer"}},"additionalProperties":false}),
    ];
    for schema in schemas {
        let mut contract = definition();
        contract["jsonSchema"] = schema;
        for value in [
            json!(null),
            json!(3),
            json!("éé"),
            json!({"count":3}),
            json!({"count":1}),
            json!({"next":{"next":{}}}),
            json!([1, 2, 3]),
            json!([1, 1]),
            json!({"x":1}),
            json!({"x":1,"y":false}),
        ] {
            steps.push(decode(contract.clone(), "json", value));
        }
    }
    for media in [
        "application/vnd.fixture+json",
        "application/a+b+json",
        "application/+json",
        "application/JSON",
        "Application/json",
        "application/json;charset=utf-8",
        "text/plain",
        "text/html",
        "application/octet-stream",
    ] {
        let contract = json!({"mediaType":media,"jsonSchema":true});
        for encoding in ["json", "text", "base64"] {
            steps.push(decode(contract.clone(), encoding, json!("YQ==")));
        }
    }
    for encoded in [
        "", "YQ==", "YQ", "YR==", "YQ===", "Y Q==", "-w==", "_w==", "/w==", "=", "AA==\n", "é",
    ] {
        steps.push(decode(
            json!({"mediaType":"application/octet-stream"}),
            "base64",
            json!(encoded),
        ));
    }
    for units in [
        vec![0xd800],
        vec![0xdc00],
        vec![0xd800, 0xdc00],
        vec![65, 0, 66],
        vec![0xd800, 65, 0xdc00],
    ] {
        for encoding in ["json", "text", "base64"] {
            let mut step = decode(
                json!({"mediaType":if encoding=="json" {"application/json"} else if encoding=="text" {"text/plain"} else {"application/octet-stream"},"jsonSchema":true}),
                encoding,
                json!(""),
            );
            step["units"] = json!(units);
            steps.push(step);
        }
    }
    for contract in [
        json!({"mediaType":"application/json"}),
        json!({"mediaType":"application/json","jsonSchema":null}),
        json!({"mediaType":"application/json","jsonSchema":{"unknownKeyword":true}}),
    ] {
        steps.push(decode(contract, "json", json!({})));
    }
    let result = compare(&steps);
    let rows = result["rows"].as_array().unwrap();
    assert!(rows.iter().filter(|r| r["error"].is_null()).count() > 20);
    assert!(
        rows.iter()
            .filter(|r| r["error"]["data"]["code"] == "invalid_arguments")
            .count()
            > 50
    );
}

#[test]
fn decoded_content_limits_and_invalid_json_precede_later_contract_validation() {
    use ivy_contracts::limits::{
        BINARY_OBJECT_CONTENT_BYTES, JSON_OBJECT_CONTENT_BYTES, TEXT_OBJECT_CONTENT_BYTES,
    };
    let mut steps = vec![];
    for delta in [-1i64, 0, 1] {
        let size = (JSON_OBJECT_CONTENT_BYTES as i64 + delta - 2) as usize;
        steps.push(decode(definition(), "json", json!("x".repeat(size))));
        let size = (TEXT_OBJECT_CONTENT_BYTES as i64 + delta) as usize;
        steps.push(decode(
            json!({"mediaType":"text/plain"}),
            "text",
            json!("x".repeat(size)),
        ));
        let size = (BINARY_OBJECT_CONTENT_BYTES as i64 + delta) as usize;
        steps.push(decode(
            json!({"mediaType":"application/octet-stream"}),
            "base64",
            json!(STANDARD.encode(vec![0x90; size])),
        ));
    }
    steps.push(decode(
        json!({"mediaType":"application/json","jsonSchema":false}),
        "json",
        json!("x".repeat(JSON_OBJECT_CONTENT_BYTES)),
    ));
    steps.push(decode(
        json!({"mediaType":"application/json"}),
        "json",
        json!("x".repeat(JSON_OBJECT_CONTENT_BYTES)),
    ));
    steps.push(decode(definition(), "json", json!(9007199254740992u64)));
    let result = compare(&steps);
    for index in [0, 1, 2, 3, 4, 5] {
        assert!(result["rows"][index]["error"].is_null());
    }
    for index in 6..11 {
        assert_eq!(
            result["rows"][index]["error"]["data"]["code"],
            "content_too_large"
        );
    }
    assert_eq!(
        result["rows"][11]["error"]["data"]["code"],
        "invalid_arguments"
    );
}
