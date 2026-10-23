use ivy_contracts::{IvyJson, json::parse};
use ivy_store::receipts::has_unresolved_outcome;

#[test]
fn typed_receipt_markers_and_structured_errors_are_detected_without_coercion() {
    for (text, expected) in [
        (r#"null"#, false),
        (r#""unknown""#, false),
        (r#"{"ordinary":"unknown"}"#, false),
        (
            r#"{"phase":"done","state":null,"outcome":"completed"}"#,
            false,
        ),
        (
            r#"{"error":{"code":-32601,"message":"Unsupported"}}"#,
            false,
        ),
        (
            r#"{"error":{"code":"known","outcome":"not_executed","details":{"value":"unknown"}}}"#,
            false,
        ),
        (
            r#"{"code":0,"errorCode":9007199254740991,"error":null}"#,
            false,
        ),
        (r#"{"nested":[{"phase":"unknown"}]}"#, true),
        (r#"{"owner":{"outcome":"unknown"}}"#, true),
        (r#"{"execution":"outcome_unknown"}"#, true),
        (r#"{"state":"needs_attention"}"#, true),
        (r#"{"error":{"code":"native_epoch_lost"}}"#, true),
        (
            r#"{"error":{"code":-1,"data":{"outcome":"unknown"}}}"#,
            true,
        ),
        (r#"{"errorCode":"prefix_outcome_unknown_suffix"}"#, true),
        (r#"{"code":"\ud800outcome_unknown\udfff"}"#, true),
        (r#"{"\ud800":{"ownerPhase":"unknown"}}"#, true),
    ] {
        assert_eq!(
            has_unresolved_outcome(&parse(text, usize::MAX).unwrap()).unwrap(),
            expected,
            "{text}"
        );
    }
    for phase in ["phase", "state", "ownerPhase", "outcome", "execution"] {
        for marker in ["unknown", "outcome_unknown", "needs_attention"] {
            let value = IvyJson::object([(phase, marker.into())]);
            assert!(has_unresolved_outcome(&value).unwrap());
        }
        assert!(!has_unresolved_outcome(&IvyJson::object([(phase, IvyJson::Null)])).unwrap());
    }
    // Field insertion and numeric-key enumeration order cannot change a boolean guard.
    for (first, second) in [("1", "0"), ("01", "1"), ("4294967295", "4294967294")] {
        for reverse in [false, true] {
            let mut fields = vec![
                (
                    first,
                    parse(r#"{"code":{"unrecognized":"value"}}"#, 1024).unwrap(),
                ),
                (second, parse(r#"{"outcome":"unknown"}"#, 1024).unwrap()),
            ];
            if reverse {
                fields.reverse();
            }
            assert!(has_unresolved_outcome(&IvyJson::object(fields)).unwrap());
        }
    }
}

#[test]
fn malformed_non_null_control_fields_conservatively_protect_receipts() {
    for key in [
        "phase",
        "state",
        "ownerPhase",
        "outcome",
        "execution",
        "error",
        "errorCode",
        "code",
    ] {
        for text in [
            "false",
            "[]",
            r#"["completed"]"#,
            r#"[["unknown"]]"#,
            "{}",
            r#"{"toString":null}"#,
        ] {
            let value = IvyJson::object([(key, parse(text, 1024).unwrap())]);
            assert!(has_unresolved_outcome(&value).unwrap(), "{key}: {text}");
        }
        assert!(!has_unresolved_outcome(&IvyJson::object([(key, IvyJson::Null)])).unwrap());
    }
    for key in [
        "phase",
        "state",
        "ownerPhase",
        "outcome",
        "execution",
        "error",
    ] {
        for number in [0.0, -1.0, 1.5, 9_007_199_254_740_992.0] {
            assert!(
                has_unresolved_outcome(&IvyJson::object([(key, IvyJson::Number(number))])).unwrap()
            );
        }
    }
    for key in ["code", "errorCode"] {
        for number in [
            -9_007_199_254_740_991.0,
            -32601.0,
            0.0,
            9_007_199_254_740_991.0,
        ] {
            assert!(
                !has_unresolved_outcome(&IvyJson::object([(key, IvyJson::Number(number))]))
                    .unwrap()
            );
        }
        for number in [1.5, f64::NAN, f64::INFINITY, 9_007_199_254_740_992.0] {
            assert!(
                has_unresolved_outcome(&IvyJson::object([(key, IvyJson::Number(number))])).unwrap()
            );
        }
    }
}
