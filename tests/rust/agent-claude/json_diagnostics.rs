use ivy_contracts::{
    IvyJson, JsString, Outcome,
    json::{parse, stringify},
};

#[test]
fn native_json_retains_explicit_utf16_keys_values_and_duplicate_updates() {
    let value = parse(
        r#"{"\ud800":"\udc00","x":1,"x":2,"items":[true,null,"😀"]}"#,
        1024,
    )
    .unwrap();
    assert_eq!(
        value.get_key(&JsString(vec![0xd800])),
        Some(&IvyJson::String(JsString(vec![0xdc00])))
    );
    assert_eq!(value.get("x"), Some(&IvyJson::Number(2.0)));
    let encoded = stringify(&value, 1024).unwrap();
    assert_eq!(parse(&encoded, 1024).unwrap(), value);
}

#[test]
fn native_json_bounds_bytes_and_reports_typed_failures_without_runtime_diagnostics() {
    for input in ["{", "[invalid]", "true false", "\"unterminated", "\"😀\""] {
        assert_eq!(
            parse(input, input.len() - 1).unwrap_err().code,
            "content_too_large"
        );
    }
    for input in [
        "{",
        "[invalid]",
        "true false",
        "\"unterminated",
        "\"\\uX000\"",
    ] {
        let error = parse(input, 1024).unwrap_err();
        assert_eq!(error.code, "invalid_arguments");
        assert_eq!(error.outcome, Outcome::NotExecuted);
        assert!(!error.message.is_empty());
        assert_eq!(error.wire_message, None);
    }
    assert!(parse("\"😀\"", "\"😀\"".len()).is_ok());
}
