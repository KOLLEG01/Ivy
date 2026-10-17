use ivy_contracts::{IvyJson, JsString, hash::*, json::*, operation::*};
#[test]
fn transport_frame_admission_matches_typescript() {
    use ivy_contracts::validation::*;
    let root=std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let output=std::process::Command::new("node").args(["--experimental-transform-types","--disable-warning=ExperimentalWarning","tests/fixtures/frame-oracle.mjs"]).current_dir(root).output().unwrap();
    assert!(output.status.success(),"{}",String::from_utf8_lossy(&output.stderr));
    let cases:Vec<serde_json::Value>=serde_json::from_slice(&output.stdout).unwrap();
    for case in cases {
        let value=parse(case["input"].as_str().unwrap(),100_000).unwrap();
        let result=match case["kind"].as_str().unwrap(){"request"=>validate_request(&value),"response"=>validate_response_frame(&value),_=>validate_provider_call_frame(&value)};
        if case["ok"]==true{assert!(result.is_ok(),"{case}: {result:?}");}
        else{let error=result.unwrap_err();assert_eq!(error.code,case["code"].as_str().unwrap(),"{case}");assert_eq!(error.message,case["message"].as_str().unwrap(),"{case}");}
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
