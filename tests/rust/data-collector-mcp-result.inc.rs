use ivy_contracts::{IvyJson, JsString, Outcome, json::parse};
use ivy_data_collector::mcp_result::tool_result;

fn json(text: &str) -> IvyJson {
    parse(text, usize::MAX).unwrap()
}
#[test]
fn mcp_results_prefer_structured_payloads_and_parse_standard_json_text() {
    let structured =
        json(r#"{"structuredContent":{"value":1},"content":[{"type":"text","text":"2"}]}"#);
    assert_eq!(tool_result(&structured).unwrap(), json(r#"{"value":1}"#));
    for value in ["false", "0", "[]"] {
        let input = IvyJson::object([("structuredContent", json(value))]);
        assert_eq!(tool_result(&input).unwrap(), json(value));
    }
    let input = json(
        r#"{"structuredContent":null,"content":[{"type":"image","text":"4"},{"type":"text","text":"invalid JSON"},{"type":"text","text":"{\"value\":\"\\ud800\",\"__proto__\":7}"}]}"#,
    );
    assert_eq!(
        tool_result(&input).unwrap(),
        json(r#"{"value":"\ud800","__proto__":7}"#)
    );
    for input in [
        json("{}"),
        json(r#"{"content":[{"type":"text","text":"unparsed"}]}"#),
    ] {
        assert_eq!(tool_result(&input).unwrap(), input);
    }
}
#[test]
fn mcp_results_do_not_rewrite_invalid_unicode_into_json() {
    let input = IvyJson::object([(
        "content",
        IvyJson::Array(vec![IvyJson::object([
            ("type", "text".into()),
            ("text", IvyJson::String(JsString(vec![34, 0xd800, 34]))),
        ])]),
    )]);
    assert_eq!(tool_result(&input).unwrap(), input);
}
#[test]
fn mcp_failures_keep_actionable_codes_and_outcomes_without_provider_details() {
    for outcome in ["not_executed", "unknown", "completed"] {
        let payload = json(&format!(
            r#"{{"error":{{"code":-32000,"message":"private provider detail","data":{{"code":"authentication_required","outcome":"{outcome}","details":{{"secret":"private"}}}}}}}}"#
        ));
        for envelope in [
            IvyJson::object([
                ("isError", true.into()),
                ("structuredContent", payload.clone()),
            ]),
            IvyJson::object([
                ("isError", true.into()),
                (
                    "content",
                    IvyJson::Array(vec![IvyJson::object([
                        ("type", "text".into()),
                        (
                            "text",
                            ivy_contracts::json::stringify(&payload, 1000)
                                .unwrap()
                                .into(),
                        ),
                    ])]),
                ),
            ]),
        ] {
            let error = tool_result(&envelope).unwrap_err();
            assert_eq!(error.code, "authentication_required");
            assert_eq!(error.outcome.to_json(), outcome.into());
            assert_eq!(error.message, "The MCP tool returned an error.");
        }
    }
    for input in [
        json(r#"{"isError":true}"#),
        json(r#"{"isError":true,"structuredContent":{"error":{"code":4}}}"#),
        json(
            r#"{"isError":true,"structuredContent":{"error":{"code":-32000,"message":"private","data":{"code":"BAD-CODE","outcome":"unknown","details":null}}}}"#,
        ),
    ] {
        let error = tool_result(&input).unwrap_err();
        assert_eq!(error.code, "provider_unavailable");
        assert_eq!(error.outcome, Outcome::Unknown);
        assert!(!error.message.contains("private"));
    }
}
