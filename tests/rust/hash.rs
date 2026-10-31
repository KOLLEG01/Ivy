use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString,
    hash::{digest, hash_array, hash_json, hash_object_with_array, hmac_sha256, tool_definition_hash, verify_hmac_sha256},
    json::canonical,
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
    canonical(&IvyJson::object(pairs.iter()
        .filter(|(key, _)| key != &JsString::from("discovery")).cloned()), MANAGEMENT_FRAME_BYTES).map(digest)
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

fn compare_generated_array(object: &IvyJson, records: Vec<IvyJson>) -> bool {
    let pairs = object.as_object().unwrap();
    let mut expected = IvyJson::object(pairs.iter().cloned());
    expected.insert("tools", IvyJson::Array(records.clone()));
    let actual = hash_object_with_array(pairs.iter().map(|(key, value)| (key, value)),
        &JsString::from("tools"), records.into_iter().map(Ok)).map_err(|e| (e.code, e.message));
    let expected = canonical(&expected, MANAGEMENT_FRAME_BYTES).map(digest).map_err(|e| (e.code, e.message));
    assert_eq!(actual, expected);
    actual.is_ok()
}

#[test]
fn generated_array_hash_preserves_canonical_fields_duplicates_and_error_order() {
    let object = IvyJson::Object(vec![
        ("tools".into(), IvyJson::Number(f64::NAN)),
        ("guide".into(), IvyJson::Number(f64::NAN)),
        ("guide".into(), IvyJson::String(JsString(vec![0xd800]))),
        (JsString(vec![0xe000]), "bmp".into()),
        (JsString(vec![0xd800, 0xdc00]), "astral".into()),
    ]);
    assert!(compare_generated_array(&object, vec![]));
    for value in [IvyJson::Bool(false), IvyJson::Null, (-0.0).into(), 1e-7.into(),
        IvyJson::object([(JsString(vec![0xd800]), IvyJson::String(JsString(vec![0xdc00])))]),
        IvyJson::Number(f64::NAN), 9007199254740992.0.into()]
    {
        compare_generated_array(&object, vec![value]);
    }
    let visits = std::cell::Cell::new(0);
    let guide = IvyJson::Number(f64::NAN);
    let error = hash_object_with_array([(&JsString::from("guide"), &guide)],
        &JsString::from("tools"), (0..2).map(|_| {
            visits.set(visits.get() + 1); Ok(IvyJson::Null)
        })).unwrap_err();
    assert_eq!(error.code, "invalid_arguments");
    assert_eq!(visits.get(), 0);
    let error = hash_object_with_array([], &JsString::from("tools"), [
        Ok(IvyJson::Null), Err(ivy_contracts::IvyError::new("fixture", "Record read failed.")),
    ]).unwrap_err();
    assert_eq!(error.code, "fixture");
}

#[test]
fn generated_array_hash_keeps_one_node_depth_and_byte_budget() {
    let object = IvyJson::object([] as [(&str, IvyJson); 0]);
    for depth in [126, 127] {
        let mut value = IvyJson::Null;
        for _ in 0..depth { value = IvyJson::Array(vec![value]); }
        assert_eq!(compare_generated_array(&object, vec![value]), depth == 126);
    }
    for count in [399_998, 399_999] {
        assert_eq!(compare_generated_array(&object, vec![IvyJson::Null; count]), count == 399_998);
    }
    // Seventeen punctuation/key bytes and one shared budget across both records.
    let text = MANAGEMENT_FRAME_BYTES - 17;
    for extra in [0, 1] {
        assert_eq!(compare_generated_array(&object, vec![
            "a".repeat(text / 2).into(), "b".repeat(text - text / 2 + extra).into(),
        ]), extra == 0);
    }
}

#[test]
fn canonical_hash_matches_exact_bytes_across_buffer_boundaries() {
    // Escapes and multibyte Unicode cross the fixed hash buffer repeatedly.
    for length in [0, 1, 8190, 8191, 8192, 8193, 16_383, 16_384, 65_536] {
        let mut units = vec![u16::from(b'x'); length];
        units.extend("\u{1f600}\"\\\n\u{1}\u{e000}".encode_utf16());
        units.extend([0xd800, u16::from(b'z'), 0xdc00]);
        let value = IvyJson::object([
            (JsString(vec![0xe000]), IvyJson::String(JsString(units))),
            (JsString(vec![0xd800, 0xdc00]), IvyJson::Array(vec![(-0.0).into(), 1e-7.into(), true.into()])),
        ]);
        assert_eq!(hash_json(&value).unwrap(), digest(canonical(&value, MANAGEMENT_FRAME_BYTES).unwrap()));
        assert!(compare_generated_array(&IvyJson::object([("guide", value.clone())]), vec![value]));
    }
}

fn compare_array_hash(records: Vec<IvyJson>) -> bool {
    let expected=hash_json(&IvyJson::Array(records.clone())).map_err(|error|(error.code,error.message));
    let actual=hash_array(records.into_iter().map(Ok)).map_err(|error|(error.code,error.message));
    assert_eq!(actual,expected);actual.is_ok()
}
#[test]
fn streamed_array_hash_preserves_values_budgets_and_read_error_priority() {
    assert!(compare_array_hash(vec![]));
    for value in [IvyJson::Null,(-0.0).into(),1e-7.into(),IvyJson::Bool(false),
        IvyJson::String(JsString(vec![0xd800,0,0xd801])),IvyJson::Number(f64::NAN)] {
        compare_array_hash(vec![value]);
    }
    for depth in [127,128] {
        let mut value=IvyJson::Null;for _ in 0..depth {value=IvyJson::Array(vec![value]);}
        assert_eq!(compare_array_hash(vec![value]),depth==127);
    }
    for count in [399_999,400_000] {assert_eq!(compare_array_hash(vec![IvyJson::Null;count]),count==399_999);}
    // Seven array/comma/string punctuation bytes, one budget across records.
    for extra in [0,1] {
        let bytes=MANAGEMENT_FRAME_BYTES-7;
        assert_eq!(compare_array_hash(vec!["a".repeat(bytes/2).into(),"b".repeat(bytes-bytes/2+extra).into()]),extra==0);
    }
    let visits=std::cell::Cell::new(0);
    let error=hash_array((0..2).map(|index|{visits.set(visits.get()+1);
        if index==0 {Ok(IvyJson::Number(f64::NAN))}else{Err(IvyError::new("fixture","Read failed."))}})).unwrap_err();
    assert_eq!(error.code,"invalid_arguments");assert_eq!(visits.get(),1);
    assert_eq!(hash_array([Ok(IvyJson::Null),Err(IvyError::new("fixture","Read failed."))]).unwrap_err().code,"fixture");
}
