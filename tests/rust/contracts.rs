use ivy_contracts::{IvyJson, JsString, hash::*, json::*, operation::*};

#[test]
fn nfc_matches_node_scalars_decompositions_reordering_and_utf16_boundaries() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let output = std::process::Command::new("node")
        .arg("tests/fixtures/nfc-oracle.mjs")
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let oracle: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let units = |value: &serde_json::Value| -> Vec<u16> {
        value
            .as_array()
            .unwrap()
            .iter()
            .map(|v| u16::try_from(v.as_u64().unwrap()).unwrap())
            .collect()
    };
    let mut mappings: std::collections::HashMap<u32, (Vec<u16>, Vec<u16>)> = oracle["mappings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| {
            (
                u32::try_from(row[0].as_u64().unwrap()).unwrap(),
                (units(&row[1]), units(&row[2])),
            )
        })
        .collect();
    assert!(mappings.len() > 13_000);
    let mut scalars = 0;
    for point in 0..=0x10ffff {
        let Some(c) = char::from_u32(point) else {
            continue;
        };
        scalars += 1;
        let input = JsString::from(c.to_string());
        let expected = if let Some((nfc, nfd)) = mappings.remove(&point) {
            assert_eq!(JsString(nfd).normalize_nfc().0, nfc, "NFD U+{point:04X}");
            nfc
        } else {
            input.0.clone()
        };
        assert_eq!(
            input.normalize_nfc().0,
            expected,
            "U+{point:04X}; Node {} ICU {} Unicode {}",
            oracle["node"],
            oracle["icu"],
            oracle["unicode"]
        );
    }
    assert_eq!(Some(scalars), oracle["scalarCount"].as_u64());
    assert!(mappings.is_empty());
    assert!(oracle["contexts"].as_array().unwrap().len() > 20_000);
    for (index, row) in oracle["contexts"].as_array().unwrap().iter().enumerate() {
        assert_eq!(
            JsString(units(&row["input"])).normalize_nfc().0,
            units(&row["nfc"]),
            "context {index}"
        );
    }
}

#[test]
fn uppercase_and_trim_start_match_node_scalars_units_and_surrogates() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let output = std::process::Command::new("node")
        .arg("tests/fixtures/upper-trim-oracle.mjs")
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let oracle: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let units = |value: &serde_json::Value| -> Vec<u16> {
        value
            .as_array()
            .unwrap()
            .iter()
            .map(|v| u16::try_from(v.as_u64().unwrap()).unwrap())
            .collect()
    };
    let mut mappings: std::collections::HashMap<u32, Vec<u16>> = oracle["mappings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| {
            (
                u32::try_from(row[0].as_u64().unwrap()).unwrap(),
                units(&row[1]),
            )
        })
        .collect();
    assert!(mappings.len() > 1400);
    let mut scalars = 0;
    for point in 0..=0x10ffff {
        let Some(c) = char::from_u32(point) else {
            continue;
        };
        scalars += 1;
        let input = JsString::from(c.to_string());
        let expected = mappings.remove(&point).unwrap_or_else(|| input.0.clone());
        assert_eq!(
            input.to_uppercase().0,
            expected,
            "U+{point:04X}; Rust {:?}; Node {} Unicode {}",
            char::UNICODE_VERSION,
            oracle["node"],
            oracle["unicode"]
        );
    }
    assert_eq!(Some(scalars), oracle["scalarCount"].as_u64());
    assert!(mappings.is_empty());
    let whitespace: std::collections::HashSet<u16> =
        units(&oracle["whitespace"]).into_iter().collect();
    assert_eq!(whitespace.len(), 25);
    let trim_whitespace: std::collections::HashSet<u16> =
        units(&oracle["trimWhitespace"]).into_iter().collect();
    assert_eq!(trim_whitespace.len(), 25);
    for unit in 0..=u16::MAX {
        let input = JsString(vec![unit, 120]);
        let expected = if whitespace.contains(&unit) {
            vec![120]
        } else {
            input.0.clone()
        };
        assert_eq!(input.trim_start().0, expected, "U+{unit:04X}");
        let input = JsString(vec![unit, 120, unit]);
        let expected = if trim_whitespace.contains(&unit) {
            vec![120]
        } else {
            input.0.clone()
        };
        assert_eq!(input.trim().0, expected, "full trim U+{unit:04X}");
    }
    for row in oracle["contexts"].as_array().unwrap() {
        let input = JsString(units(&row["input"]));
        assert_eq!(input.to_uppercase().0, units(&row["upper"]));
        assert_eq!(input.trim_start().0, units(&row["trimStart"]));
        assert_eq!(input.trim().0, units(&row["trim"]));
    }
}

#[test]
fn default_and_english_lowercase_match_node_scalars_context_and_unpaired_units() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let output = std::process::Command::new("node")
        .args(["tests/fixtures/english-lowercase-oracle.mjs"])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let oracle: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(oracle["defaultMatchesEnglish"], true);
    let units = |value: &serde_json::Value| -> Vec<u16> {
        value
            .as_array()
            .unwrap()
            .iter()
            .map(|v| u16::try_from(v.as_u64().unwrap()).unwrap())
            .collect()
    };
    let mut mappings: std::collections::HashMap<u32, Vec<u16>> = oracle["mappings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| {
            (
                u32::try_from(row[0].as_u64().unwrap()).unwrap(),
                units(&row[1]),
            )
        })
        .collect();
    assert!(mappings.len() > 1300);
    let mut scalars = 0;
    for point in 0..=0x10ffff {
        let Some(c) = char::from_u32(point) else {
            continue;
        };
        scalars += 1;
        let input = JsString::from(c.to_string());
        let expected = mappings.remove(&point).unwrap_or_else(|| input.0.clone());
        assert_eq!(
            input.to_lowercase().0,
            expected,
            "U+{point:04X}; Rust Unicode {:?}; Node {} Unicode {}",
            char::UNICODE_VERSION,
            oracle["node"],
            oracle["unicode"]
        );
    }
    assert_eq!(Some(scalars), oracle["scalarCount"].as_u64());
    assert!(mappings.is_empty());
    assert!(oracle["contexts"].as_array().unwrap().len() > 12000);
    for row in oracle["contexts"].as_array().unwrap() {
        let input = JsString(units(&row["input"]));
        assert_eq!(input.to_lowercase().0, units(&row["output"]), "{input:?}");
        assert_eq!(input.to_english_lowercase(), input.to_lowercase());
    }
}

#[test]
fn win32_normalization_and_default_lowercase_match_node_without_host_io() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let output = std::process::Command::new("node")
        .arg("tests/fixtures/win32-path-oracle.mjs")
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let oracle: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let rows = oracle["rows"].as_array().unwrap();
    assert!(rows.len() > 5000);
    for row in rows {
        let input = parse(row["input"].as_str().unwrap(), 100_000).unwrap();
        let normalized = ivy_contracts::js_path::normalize_win32(input.as_js_string().unwrap());
        assert_eq!(
            IvyJson::String(normalized.clone()),
            parse(row["normalized"].as_str().unwrap(), 100_000).unwrap(),
            "path {}",
            row["input"]
        );
        assert_eq!(
            IvyJson::String(normalized.to_lowercase()),
            parse(row["key"].as_str().unwrap(), 100_000).unwrap(),
            "key {}",
            row["input"]
        );
    }
}

#[test]
fn native_frame_bounds_match_the_existing_contract() {
    use ivy_contracts::limits::*;
    let schema: serde_json::Value =
        serde_json::from_str(include_str!("../../specs/schemas/agent.schema.json")).unwrap();
    let properties = &schema["$defs"]["FrameLimits"]["properties"];
    for (name, bound) in [
        ("requestFrameBytes", NATIVE_REQUEST_FRAME_BYTES),
        ("receivedFrameBytes", NATIVE_FRAME_BYTES),
        ("answerFrameBytes", NATIVE_ANSWER_FRAME_BYTES),
        ("managementFrameBytes", MANAGEMENT_FRAME_BYTES),
    ] {
        assert_eq!(
            properties[name]["const"].as_u64(),
            Some(bound as u64),
            "{name}"
        );
    }
}

#[test]
fn url_projection_matches_node_constructor_fields_and_failures() {
    use ivy_contracts::whatwg_url::{clear_url_credentials, parse_url};
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    for mode in ["projection", "credentials"] {
        let output = std::process::Command::new("node")
            .args([
                "--experimental-transform-types",
                "--disable-warning=ExperimentalWarning",
                "tests/fixtures/whatwg-url-oracle.mjs",
            ])
            .arg(mode)
            .current_dir(&root)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let rows: Vec<serde_json::Value> = serde_json::from_slice(&output.stdout).unwrap();
        assert!(rows.len() > 700);
        for row in rows {
            let input = parse(row["input"].as_str().unwrap(), 100_000).unwrap();
            let expected = parse(row["result"].as_str().unwrap(), 100_000).unwrap();
            let actual = if mode == "credentials" {
                match clear_url_credentials(input.as_js_string().unwrap()) {
                    Ok(value) => {
                        IvyJson::object([("ok", true.into()), ("value", IvyJson::String(value))])
                    }
                    Err(error) => {
                        IvyJson::object([("ok", false.into()), ("error", error.to_wire())])
                    }
                }
            } else {
                match parse_url(input.as_js_string().unwrap()) {
                    Ok(u) => IvyJson::object([
                        ("ok", true.into()),
                        (
                            "value",
                            IvyJson::object([
                                ("href", u.href.into()),
                                ("protocol", u.protocol.into()),
                                ("hostname", u.hostname.into()),
                                ("username", u.username.into()),
                                ("password", u.password.into()),
                                ("hash", u.hash.into()),
                            ]),
                        ),
                    ]),
                    Err(error) => {
                        IvyJson::object([("ok", false.into()), ("error", error.to_wire())])
                    }
                }
            };
            assert_eq!(actual, expected, "URL input: {}", row["input"]);
        }
    }
}

#[test]
fn projected_fields_preserve_undefined_and_canonical_error_order() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let output = std::process::Command::new("node")
        .args([
            "--experimental-transform-types",
            "--disable-warning=ExperimentalWarning",
            "tests/fixtures/json-fields-oracle.mjs",
        ])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rows: Vec<serde_json::Value> = serde_json::from_slice(&output.stdout).unwrap();
    assert!(rows.len() > 300);
    for row in rows {
        let input = parse(row["input"].as_str().unwrap(), 100_000).unwrap();
        let mut pairs: Vec<(JsString, Option<IvyJson>)> = Vec::new();
        match input
            .get("generated")
            .and_then(|value| value.as_str().ok())
            .as_deref()
        {
            Some("nodes") => pairs.extend([
                (
                    "a".into(),
                    Some(IvyJson::Array(
                        (0..input.get("count").unwrap().as_f64().unwrap() as usize)
                            .map(|_| IvyJson::Null)
                            .collect(),
                    )),
                ),
                ("b".into(), None),
            ]),
            Some("depth") => {
                let mut value = IvyJson::Null;
                for _ in 0..input.get("depth").unwrap().as_f64().unwrap() as usize {
                    value = IvyJson::Array(vec![value]);
                }
                pairs.extend([("a".into(), Some(value)), ("b".into(), None)]);
            }
            Some("number") => {
                let value = match input.get("number").unwrap().as_str().unwrap().as_str() {
                    "nan" => f64::NAN,
                    "infinity" => f64::INFINITY,
                    _ => f64::NEG_INFINITY,
                };
                pairs.extend([
                    ("a".into(), Some(IvyJson::Number(value))),
                    ("b".into(), None),
                ]);
            }
            _ => {
                for field in input.get("fields").unwrap().as_array().unwrap() {
                    pairs.push((
                        field.get("key").unwrap().as_js_string().unwrap().clone(),
                        field.get("value").cloned(),
                    ));
                }
            }
        }
        let actual = match canonical_fields(
            pairs
                .iter()
                .map(|(key, value)| (key.clone(), value.as_ref())),
            input.get("maximumBytes").unwrap().as_f64().unwrap() as usize,
        ) {
            Ok(value) => IvyJson::object([("ok", true.into()), ("value", value.into())]),
            Err(error) => IvyJson::object([("ok", false.into()), ("error", error.to_wire())]),
        };
        assert_eq!(
            actual,
            parse(row["result"].as_str().unwrap(), 100_000).unwrap(),
            "fields input: {}",
            row["input"]
        );
    }
}

#[test]
fn ordinary_json_node_counts_are_independent_of_validation_limits() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let output = std::process::Command::new("node")
        .args([
            "--experimental-transform-types",
            "--disable-warning=ExperimentalWarning",
            "tests/fixtures/json-oracle.mjs",
            "wide",
        ])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let cases: Vec<serde_json::Value> = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(cases.len(), 8);
    for case in cases {
        let count = case["count"].as_u64().unwrap() as usize;
        let array = format!("[{}null]", "null,".repeat(count - 1));
        let input = if case["envelope"] == true {
            format!(r#"{{"values":{array}}}"#)
        } else {
            array
        };
        let value = parse(&input, input.len()).unwrap();
        let encoded = stringify(&value, input.len()).unwrap();
        assert_eq!(encoded.len() as u64, case["bytes"].as_u64().unwrap());
        assert_eq!(
            digest(&encoded),
            format!("sha256:{}", case["sha256"].as_str().unwrap())
        );
        assert_eq!(
            stringify(&value, input.len() - 1).unwrap_err().code,
            "content_too_large"
        );
        for (name, result) in [
            ("canonical", canonical(&value, 4_000_000).map(|_| ())),
            ("validateJson", validate_json(&value, 4_000_000)),
        ] {
            assert_eq!(
                result.err().map(|e| e.code),
                case[name].as_str().map(str::to_owned),
                "{case}"
            );
        }
    }
    let malformed = format!("[{}]", "null,".repeat(400_001));
    assert_eq!(
        parse(&malformed, malformed.len()).unwrap_err().code,
        "invalid_arguments"
    );
    assert_eq!(
        parse(&malformed, malformed.len() - 1).unwrap_err().code,
        "content_too_large"
    );
    let strict = ParseOptions {
        reject_duplicate_keys: true,
        maximum_container_depth: Some(1),
    };
    let wide = format!("[{}null]", "null,".repeat(400_000));
    assert!(parse_with_options(&wide, wide.len(), strict).is_ok());
}
#[test]
fn long_unicode_strings_preserve_utf16_and_parser_boundaries() {
    let text =
        "\u{7f}\u{80}\u{7ff}\u{800}\u{d7ff}\u{e000}\u{ffff}\u{10000}\u{10ffff}".repeat(32_768);
    let input = format!(r#"{{"{text}":["{text}\n\ud800","tail"]}}"#);
    let value = parse(&input, input.len()).unwrap();
    let (key, values) = &value.as_object().unwrap()[0];
    assert_eq!(key, &JsString::from(text.as_str()));
    let mut expected: Vec<u16> = text.encode_utf16().collect();
    expected.extend([10, 0xd800]);
    let values = values.as_array().unwrap();
    assert_eq!(values[0].as_js_string().unwrap().0, expected);
    assert_eq!(values[1].as_str().unwrap(), "tail");
    assert_eq!(
        parse(&input, input.len() - 1).unwrap_err().code,
        "content_too_large"
    );
    for malformed in ["\"é", "\"😀\\", "\"界\\é\"", "\"é\\u界000\"", "[\"😀\",é]"] {
        assert_eq!(parse(malformed, 100).unwrap_err().code, "invalid_arguments");
    }
}
#[test]
fn surrogate_identifiers_error_messages_and_strict_native_json() {
    let parent = parse(r#""\ud800:00042:\udfff""#, 100).unwrap();
    let parent = parent.as_js_string().unwrap();
    let identity = parse_operation_id_js(parent).unwrap();
    assert_eq!(identity.runtime_epoch.0, [0xd800]);
    assert_eq!(identity.nonce.0, [0xdfff]);
    let child = derive_operation_id_js(parent, &IvyJson::Null).unwrap();
    assert_eq!(parse_operation_id_js(&child).unwrap().issued_at_unix_ms, 42);
    let message = JsString(vec![0xd800, b'x' as u16]);
    let error =
        ivy_contracts::IvyError::new("provider_failed", "error").with_js_message(message.clone());
    assert_eq!(
        error.to_wire().get("message"),
        Some(&IvyJson::String(message))
    );
    let strict = ParseOptions {
        reject_duplicate_keys: true,
        maximum_container_depth: Some(2),
    };
    assert!(parse_with_options(r#"{"x":1,"\u0078":2}"#, 100, strict).is_err());
    assert!(parse_with_options(r#"{"a":[1]}"#, 100, strict).is_ok());
    assert!(parse_with_options(r#"{"a":[{}]}"#, 100, strict).is_err());
    assert!(parse(r#"{"x":1,"x":2}"#, 100).is_ok());
}
#[test]
fn byte_limit_error_precedence_matches_typescript() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let output = std::process::Command::new("node")
        .args([
            "--experimental-transform-types",
            "--disable-warning=ExperimentalWarning",
            "tests/fixtures/json-oracle.mjs",
            "limits",
        ])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let cases: Vec<serde_json::Value> = serde_json::from_slice(&output.stdout).unwrap();
    for case in cases {
        let value = parse(case["input"].as_str().unwrap(), 1000).unwrap();
        let maximum = case["maximum"].as_u64().unwrap() as usize;
        for (name, result) in [
            ("canonical", canonical(&value, maximum).map(|_| ())),
            ("validateJson", validate_json(&value, maximum)),
        ] {
            assert_eq!(
                result.err().map(|e| e.code),
                case[name].as_str().map(str::to_owned),
                "{case}"
            );
        }
    }
}
#[test]
fn transport_frame_admission_matches_typescript() {
    use ivy_contracts::validation::*;
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let output = std::process::Command::new("node")
        .args([
            "--experimental-transform-types",
            "--disable-warning=ExperimentalWarning",
            "tests/fixtures/frame-oracle.mjs",
        ])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let cases: Vec<serde_json::Value> = serde_json::from_slice(&output.stdout).unwrap();
    for case in cases {
        let value = parse(case["input"].as_str().unwrap(), 100_000).unwrap();
        let result = match case["kind"].as_str().unwrap() {
            "request" => validate_request(&value),
            "response" => validate_response_frame(&value),
            "service-notification" => validate_service_notification_frame(&value),
            "provider-notification" => validate_provider_notification_frame(&value),
            "event-availability" => validate_event_availability_frame(&value),
            _ => validate_provider_call_frame(&value),
        };
        if case["ok"] == true {
            assert!(result.is_ok(), "{case}: {result:?}");
        } else {
            let error = result.unwrap_err();
            assert_eq!(error.code, case["code"].as_str().unwrap(), "{case}");
            assert_eq!(error.message, case["message"].as_str().unwrap(), "{case}");
        }
    }
}
#[test]
fn utf16_and_javascript_ordering_are_distinct() {
    let value = parse(r#"{"10":1,"2":2,"a":"\ud800","𐀀":3,"":4}"#, 4096).unwrap();
    assert_eq!(
        canonical(&value, 4096).unwrap(),
        r#"{"10":1,"2":2,"a":"\ud800","𐀀":3,"":4}"#
    );
    assert_eq!(
        stringify(&value, 4096).unwrap(),
        r#"{"2":2,"10":1,"a":"\ud800","𐀀":3,"":4}"#
    );
    assert!(value.to_serde().is_err());
    assert_eq!(JsString::from("😀").utf16_len(), 2);
    assert_eq!(JsString::from("😀").code_point_len(), 1);
}
#[test]
fn parser_and_number_boundaries() {
    assert_eq!(
        canonical(&parse(r#"{"x":1,"x":2}"#, 100).unwrap(), 100).unwrap(),
        r#"{"x":2}"#
    );
    for text in ["01", "1.", "+1", "[1,]", "{\"x\":1,}", "NaN", "\"\n\""] {
        assert!(parse(text, 100).is_err(), "{text}");
    }
    for text in ["9007199254740992", "1e400"] {
        assert!(canonical(&parse(text, 100).unwrap(), 100).is_err());
    }
    for (text, want) in [
        ("-0", "0"),
        ("1e-6", "0.000001"),
        ("1e-7", "1e-7"),
        ("9007199254740991", "9007199254740991"),
    ] {
        assert_eq!(canonical(&parse(text, 100).unwrap(), 100).unwrap(), want);
    }
    assert_eq!(
        stringify(&IvyJson::Number(f64::INFINITY), 100).unwrap(),
        "null"
    );
    assert!(canonical(&IvyJson::from("😀"), 5).is_err());
}
#[test]
fn versions_and_operation_id() {
    assert!(
        compare_versions("999999999999999999999.0.0", "10.0.0")
            .unwrap()
            .is_gt()
    );
    assert!(compare_versions("01.0.0", "1.0.0").is_err());
    assert_eq!(
        parse_operation_id("epoch:00001:nonce")
            .unwrap()
            .issued_at_unix_ms,
        1
    );
    assert!(parse_operation_id("epoch:9007199254740992:n").is_err());
    let a = derive_operation_id("e:0:n", &IvyJson::from("😀")).unwrap();
    assert_eq!(a.len(), 20);
    assert_eq!(
        digest(""),
        "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
}

#[test]
fn unchanged_typescript_codec_is_the_oracle() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let output = std::process::Command::new("node")
        .arg("--experimental-transform-types")
        .arg("--disable-warning=ExperimentalWarning")
        .arg("tests/fixtures/json-oracle.mjs")
        .current_dir(root)
        .output()
        .expect("Node is a declared build dependency");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let vectors: Vec<serde_json::Value> = serde_json::from_slice(&output.stdout).unwrap();
    assert!(vectors.len() > 2000);
    for vector in vectors {
        let value = parse(vector["input"].as_str().unwrap(), 1_048_576).unwrap();
        assert_eq!(
            stringify(&value, 1_048_576).unwrap(),
            vector["stringify"].as_str().unwrap()
        );
        if let Some(code) = vector["error"].as_str() {
            assert_eq!(canonical(&value, 1_048_576).unwrap_err().code, code);
            continue;
        }
        let encoded = canonical(&value, 1_048_576).unwrap();
        assert_eq!(
            encoded,
            vector["canonical"].as_str().unwrap(),
            "input: {}",
            vector["input"]
        );
        assert_eq!(hash_json(&value).unwrap(), vector["hash"].as_str().unwrap());
        assert_eq!(
            derive_operation_id("epoch:00123:nonce😀", &value).unwrap(),
            vector["operation"].as_str().unwrap()
        );
        assert!(canonical(&value, encoded.len() - 1).is_err());
    }
}
