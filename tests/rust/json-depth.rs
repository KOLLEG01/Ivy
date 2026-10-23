use ivy_contracts::{
    IvyJson, JsString,
    builtin_schema::{SchemaBundle, json_equal},
    hash::digest,
    json::*,
};

fn nested(depth: usize, kind: &str, leaf: &str) -> String {
    let mut text = String::new();
    let array = |index| kind == "array" || (kind == "mixed" && index % 2 == 0);
    for index in 0..depth {
        text.push_str(if array(index) { "[" } else { r#"{"next":"# });
    }
    text.push_str(leaf);
    for index in (0..depth).rev() {
        text.push(if array(index) { ']' } else { '}' });
    }
    text
}

#[test]
fn deep_lifecycle_and_error_cleanup_in_subprocess() {
    const CHILD: &str = "IVY_JSON_DEPTH_TEST_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "deep_lifecycle_and_error_cleanup_in_subprocess",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    std::thread::Builder::new().stack_size(256 * 1024).spawn(|| {
        let bundle = SchemaBundle::new(&[r##"{"$id":"https://test.invalid/deep","anyOf":[{"type":"string"},{"type":"array","items":{"$ref":"#"}},{"type":"object","properties":{"next":{"$ref":"#"}},"additionalProperties":false}]}"##]).unwrap();
        for kind in ["array", "object", "mixed"] {
            let text = nested(50_000, kind, r#""\ud800""#);
            let value = parse(&text, text.len()).unwrap();
            let copy = value.clone();
            assert_eq!(value, copy);
            assert!(json_equal(&value, &copy));
            assert_eq!(stringify(&value, text.len()).unwrap(), text);
            assert_eq!(stringify(&value, text.len() - 1).unwrap_err().code, "content_too_large");
            assert!(!format!("{value:?}").is_empty());
            assert!(bundle.matches("https://test.invalid/deep", &value).unwrap());
            let other_text = nested(50_000, kind, "false");
            let other = parse(&other_text, other_text.len()).unwrap();
            assert_ne!(value, other);
            assert!(!json_equal(&value, &other));
            assert!(!bundle.matches("https://test.invalid/deep", &other).unwrap());
            for check in [canonical(&value, usize::MAX).map(|_| ()), validate_json(&value, usize::MAX)] {
                assert_eq!(check.unwrap_err().code, "limit_exceeded");
            }
            for broken in [format!("{text}!"), format!("[{text},]"), text[..text.len()-1].to_owned()] {
                assert_eq!(parse(&broken, broken.len()).unwrap_err().code, "invalid_arguments");
            }
            let duplicate = format!(r#"{{"key":{text},"key":null}}"#);
            assert_eq!(parse(&duplicate, duplicate.len()).unwrap().get("key"), Some(&IvyJson::Null));
            let strict = ParseOptions { reject_duplicate_keys: true, maximum_container_depth: None };
            assert_eq!(parse_with_options(&duplicate, duplicate.len(), strict).unwrap_err().code, "invalid_arguments");
            let bounded = ParseOptions { reject_duplicate_keys: false, maximum_container_depth: Some(49_999) };
            assert_eq!(parse_with_options(&text, text.len(), bounded).unwrap_err().code, "limit_exceeded");
            drop((value, copy, other));
        }
    }).unwrap().join().unwrap();
}

#[test]
fn deep_values_preserve_public_json_and_validation_results() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let output = std::process::Command::new("node")
        .args([
            "--experimental-transform-types",
            "--disable-warning=ExperimentalWarning",
            "tests/fixtures/json-oracle.mjs",
            "deep",
        ])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let oracle: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(oracle["runtime"]["node"], "24.18.0");
    let cases = oracle["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 15);
    let mut compared_hashes = 0;
    for case in cases {
        let text = nested(
            case["depth"].as_u64().unwrap() as usize,
            case["kind"].as_str().unwrap(),
            r#""\ud800""#,
        );
        let value = parse(&text, text.len()).unwrap();
        let encoded = stringify(&value, text.len()).unwrap();
        assert_eq!(encoded, text);
        if let Some(hash) = case["stringifyHash"].as_str() {
            assert_eq!(digest(encoded), format!("sha256:{hash}"));
            compared_hashes += 1;
        }
        for (name, result) in [
            ("canonical", canonical(&value, 4_000_000).map(|_| ())),
            ("validateJson", validate_json(&value, 4_000_000)),
        ] {
            assert_eq!(
                result.err().map(|error| error.code),
                case[name].as_str().map(str::to_owned)
            );
        }
    }
    assert!(compared_hashes > 0);
}

#[test]
fn debug_shape_and_owned_extraction_remain_usable() {
    #[derive(Debug)]
    #[allow(dead_code)]
    enum Prior {
        Null,
        Bool(bool),
        Number(f64),
        String(JsString),
        Array(Vec<Prior>),
        Object(Vec<(JsString, Prior)>),
    }
    let actual = IvyJson::object([(
        "key",
        IvyJson::Array(vec![
            IvyJson::Null,
            true.into(),
            IvyJson::Number(1.25),
            "x".into(),
        ]),
    )]);
    let prior = Prior::Object(vec![(
        "key".into(),
        Prior::Array(vec![
            Prior::Null,
            Prior::Bool(true),
            Prior::Number(1.25),
            Prior::String("x".into()),
        ]),
    )]);
    assert_eq!(format!("{actual:?}"), format!("{prior:?}"));
    assert_eq!(format!("{actual:#?}"), format!("{prior:#?}"));
    let mut fields = actual.into_object().unwrap();
    let mut items = fields.pop().unwrap().1.into_array().unwrap();
    assert_eq!(items.pop().unwrap().into_js_string(), Some("x".into()));
    assert!(IvyJson::Null.into_array().is_none());
    assert!(IvyJson::Null.into_object().is_none());
    assert!(IvyJson::Null.into_js_string().is_none());
    assert_ne!(IvyJson::Number(f64::NAN), IvyJson::Number(f64::NAN));
}
