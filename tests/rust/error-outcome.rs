use ivy_contracts::{
    ErrorOutcome, IvyError, IvyJson, JsString, Outcome,
    json::{parse, stringify},
};

fn json(text: &str) -> IvyJson {
    parse(text, 100_000).unwrap()
}
fn literals() -> [Outcome; 3] {
    [Outcome::NotExecuted, Outcome::Unknown, Outcome::Completed]
}

#[test]
fn enum_constructors_and_both_literal_comparison_directions_remain_exact() {
    assert_eq!(
        IvyError::new("remote", "message").outcome,
        Outcome::NotExecuted
    );
    assert_eq!(
        IvyError::new_js("remote".into(), "message".into()).outcome,
        Outcome::NotExecuted
    );
    assert_eq!(IvyError::internal().outcome, Outcome::Unknown);
    for literal in literals() {
        let outcome: ErrorOutcome = literal.into();
        assert_eq!(outcome.literal(), Some(literal));
        assert_eq!(outcome.to_json(), IvyJson::from(literal.as_str()));
        assert_eq!(
            serde_json::to_string(&literal).unwrap(),
            format!("\"{}\"", literal.as_str())
        );
        for other in literals() {
            assert_eq!(outcome == other, literal == other);
            assert_eq!(other == outcome, other == literal);
        }
    }
}

#[test]
fn provider_values_are_retained_without_coercion_validation_or_literal_fallback() {
    for value in [
        json(r#"["unknown"]"#),
        json(r#"["completed"]"#),
        json(r#"[["not_executed"]]"#),
        json(r#"["not_executed"]"#),
        json("null"),
        json("false"),
        json("0"),
        json(r#""UNKNOWN""#),
        json(r#"{"toString":null,"value":"not_executed"}"#),
        IvyJson::String(JsString(vec![0xd800])),
    ] {
        let error = IvyError::new("remote", "message").with_provider_outcome(value.clone());
        assert_eq!(error.outcome_json(), value);
        assert_eq!(error.outcome.to_json(), value);
        assert_eq!(error.outcome.literal(), None);
        for literal in literals() {
            assert_ne!(error.outcome, literal);
            assert_ne!(literal, error.outcome);
        }
        for literal in literals() {
            let replaced = error.clone().with_outcome(literal);
            assert_eq!(replaced.outcome_json(), IvyJson::from(literal.as_str()));
            assert_eq!(replaced.outcome.literal(), Some(literal));
            assert_eq!(
                replaced.with_provider_outcome(value.clone()).outcome_json(),
                value
            );
        }
    }
}

#[test]
fn three_retained_array_failures_reach_the_actual_wire_serializer_unchanged() {
    for value in [
        r#"["unknown"]"#,
        r#"["completed"]"#,
        r#"[["not_executed"]]"#,
    ] {
        let mut error = IvyError::new("remote", "Retained.").with_provider_outcome(json(value));
        error.details = Some(IvyJson::Null);
        let expected = json(&format!(
            r#"{{"code":-32000,"message":"Retained.","data":{{"code":"remote","outcome":{value},"details":null}}}}"#
        ));
        let wire = error.to_wire();
        assert_eq!(wire, expected);
        assert_eq!(json(&stringify(&wire, 100_000).unwrap()), expected);
        assert_eq!(error.clone().to_wire(), expected);
    }
}

#[test]
fn utf16_clipping_and_absent_null_details_survive_the_raw_carrier() {
    let code = JsString(vec![0xdfff, 65]);
    let mut units = vec![120; 2047];
    units.extend([0xd800, 0xdc00, 66]);
    let message = JsString(units.clone());
    let outcome = IvyJson::Array(vec![IvyJson::String(JsString(vec![0xd800]))]);
    let mut error =
        IvyError::new_js(code.clone(), message.clone()).with_provider_outcome(outcome.clone());
    let absent = error.to_wire();
    assert_eq!(error.code_js(), code);
    assert_eq!(error.message_js(), message);
    assert!(absent.get("data").unwrap().get("details").is_none());
    assert_eq!(
        absent.get("data").unwrap().get("code"),
        Some(&IvyJson::String(code))
    );
    assert_eq!(absent.get("data").unwrap().get("outcome"), Some(&outcome));
    assert_eq!(
        absent.get("message"),
        Some(&IvyJson::String(JsString(units[..2048].to_vec())))
    );
    assert!(stringify(&absent, 100_000).unwrap().contains(r#"\ud800"#));
    for details in [
        IvyJson::Null,
        IvyJson::String(JsString(vec![0xdfff])),
        json(r#"{"unknown":[null,false]}"#),
    ] {
        error.details = Some(details.clone());
        assert_eq!(
            error.to_wire().get("data").unwrap().get("details"),
            Some(&details)
        );
        assert_eq!(error.outcome_json(), outcome);
        assert_eq!(
            error.clone().with_outcome(Outcome::Completed).details,
            Some(details)
        );
    }
    for (code, expected) in [
        ("invalid_frame", -32600.0),
        ("invalid_arguments", -32602.0),
        ("remote", -32000.0),
    ] {
        assert_eq!(
            IvyError::new(code, "message")
                .with_provider_outcome(outcome.clone())
                .to_wire()
                .get("code"),
            Some(&IvyJson::Number(expected))
        );
    }
}
