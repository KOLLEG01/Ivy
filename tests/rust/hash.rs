use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString,
    hash::{hash_json, hmac_sha256, tool_definition_hash, verify_hmac_sha256},
    limits::MANAGEMENT_FRAME_BYTES,
};

fn bytes(value: &serde_json::Value) -> Vec<u8> {
    value
        .as_array()
        .unwrap()
        .iter()
        .map(|v| u8::try_from(v.as_u64().unwrap()).unwrap())
        .collect()
}

#[test]
fn hmac_bytes_and_full_tag_verification_match_node() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let output = std::process::Command::new("node")
        .arg("tests/fixtures/hash-oracle.mjs")
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rows: Vec<serde_json::Value> = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(rows.len(), 149);
    for (i, row) in rows.iter().enumerate() {
        let key = bytes(&row["key"]);
        let message = bytes(&row["message"]);
        assert_eq!(
            hmac_sha256(&key, &message).as_slice(),
            bytes(&row["signature"]),
            "sign case {i}"
        );
        for check in row["verification"].as_array().unwrap() {
            assert_eq!(
                verify_hmac_sha256(&key, &message, &bytes(&check["signature"])),
                check["valid"].as_bool().unwrap(),
                "verify case {i}"
            );
        }
        let mut changed_message = message.clone();
        changed_message.push(0);
        assert!(!verify_hmac_sha256(
            &key,
            &changed_message,
            &bytes(&row["signature"])
        ));
    }
}

#[test]
fn persisted_hex_looking_secret_is_text_not_decoded_bytes() {
    let secret = "0123456789abcdef".repeat(4);
    let decoded: Vec<u8> = [0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef].repeat(4);
    let body = b"eyJib3VuZGFyeSI6MX0";
    let tag = hmac_sha256(secret.as_bytes(), body);
    assert_ne!(tag, hmac_sha256(&decoded, body));
    assert!(verify_hmac_sha256(secret.as_bytes(), body, &tag));
    assert!(!verify_hmac_sha256(&decoded, body, &tag));
}

fn copied_tool_hash(definition: &IvyJson) -> IvyResult<String> {
    let IvyJson::Object(pairs) = definition else {
        return Err(IvyError::new("invalid_arguments", "Expected a tool definition."));
    };
    hash_json(&IvyJson::object(pairs.iter()
        .filter(|(key, _)| key != &JsString::from("discovery")).cloned()))
}

fn compare_tool_hash(definition: &IvyJson) -> bool {
    let expected = copied_tool_hash(definition).map_err(|e| (e.code, e.message));
    let actual = tool_definition_hash(definition).map_err(|e| (e.code, e.message));
    assert_eq!(actual, expected);
    actual.is_ok()
}

#[test]
fn tool_hash_projection_preserves_opaque_fields_utf16_duplicates_and_errors() {
    for definition in [IvyJson::Null, true.into(), 17.0.into(), "tool".into(), IvyJson::Array(vec![])] {
        assert!(!compare_tool_hash(&definition));
    }
    let mut nested = IvyJson::Number(f64::NAN);
    for _ in 0..140 { nested = IvyJson::Array(vec![nested]); }
    let mut definition = IvyJson::Object(vec![
        ("name".into(), "fixture".into()),
        ("discovery".into(), nested),
        ("inputSchema".into(), false.into()),
        ("outputSchema".into(), IvyJson::object([
            (JsString(vec![0xd800]), IvyJson::String(JsString(vec![0xdc00]))),
            ("opaque".into(), IvyJson::Array(vec![(-0.0).into(), 1e-7.into(), true.into()])),
        ])),
        (JsString(vec![0xe000]), "bmp".into()),
        (JsString(vec![0xd800, 0xdc00]), "astral".into()),
        ("name".into(), "last fixture name".into()),
        ("discovery".into(), IvyJson::Number(f64::INFINITY)),
    ]);
    assert!(compare_tool_hash(&definition));
    let stable = tool_definition_hash(&definition).unwrap();
    definition.remove("discovery");
    definition.remove("discovery");
    assert_eq!(tool_definition_hash(&definition).unwrap(), stable);
    definition.insert("inputSchema", IvyJson::object([("discovery", IvyJson::Number(f64::NAN))]));
    assert!(!compare_tool_hash(&definition));
    definition.insert("inputSchema", 9007199254740992.0.into());
    assert!(!compare_tool_hash(&definition));
    // The last duplicate replaces an invalid earlier value before canonicalization.
    assert!(compare_tool_hash(&IvyJson::Object(vec![
        ("x".into(), IvyJson::Number(f64::NAN)), ("x".into(), IvyJson::Null),
    ])));
    assert!(!compare_tool_hash(&IvyJson::Object(vec![
        ("x".into(), IvyJson::Null), ("x".into(), IvyJson::Number(f64::NAN)),
    ])));
}

#[test]
fn tool_hash_projection_keeps_shared_node_depth_and_byte_limits() {
    for depth in [127, 128] {
        let mut value = IvyJson::Null;
        for _ in 0..depth { value = IvyJson::Array(vec![value]); }
        assert_eq!(compare_tool_hash(&IvyJson::object([("schema", value)])), depth == 127);
    }
    for second in [199_998, 199_999] {
        let values = |count| IvyJson::Array((0..count).map(|_| IvyJson::Null).collect());
        let definition = IvyJson::object([("a", values(199_999)), ("b", values(second))]);
        assert_eq!(compare_tool_hash(&definition), second == 199_998);
    }
    // Fifteen bytes of object/key/string punctuation, with one budget across fields.
    let text = MANAGEMENT_FRAME_BYTES - 15;
    for extra in [0, 1] {
        let definition = IvyJson::object([
            ("a", "a".repeat(text / 2).into()),
            ("b", "b".repeat(text - text / 2 + extra).into()),
        ]);
        assert_eq!(compare_tool_hash(&definition), extra == 0);
    }
}
