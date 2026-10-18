use ivy_contracts::{
    IvyJson,
    json::{canonical, parse},
};
use ivy_data_collector::{model, policy};

#[path = "data-collector-mcp-result.inc.rs"]
mod mcp_result;
#[path = "data-collector-schema.inc.rs"]
mod schema;

fn json(text: &str) -> IvyJson {
    parse(text, usize::MAX).unwrap()
}
fn retention() -> IvyJson {
    json(r#"{"maximumCount":1000,"maximumAgeDays":7,"maximumBytes":104857600}"#)
}
fn run() -> IvyJson {
    json(
        r#"{"id":"run","taskId":"task","finishedAt":"2026-01-01T00:00:00.000Z","task":{"retention":{"maximumCount":1000,"maximumAgeDays":7,"maximumBytes":104857600}},"output":{"data":null,"state":null,"events":[]}}"#,
    )
}
#[test]
fn cursor_commit_preserves_disable_and_distinguishes_missing_null_and_false() {
    for state in [
        None,
        Some(IvyJson::Null),
        Some(false.into()),
        Some(IvyJson::Number(0.0)),
    ] {
        let mut row = json(
            r#"{"task":{"enabled":false,"intervalSeconds":10},"revision":7,"state":{"cursor":5},"nextAt":0}"#,
        );
        let previous = row.get("state").unwrap().clone();
        let mut r = run();
        let mut out = IvyJson::object([("data", IvyJson::Null)]);
        if let Some(value) = &state {
            out.insert("state", value.clone());
        }
        r.insert("output", out);
        assert!(policy::commit_execution(&mut row, &mut r, 1000.0).unwrap());
        assert_eq!(row.get("state"), Some(state.as_ref().unwrap_or(&previous)));
        assert_eq!(model::number(&row, "nextAt").unwrap(), 11000.0);
        assert_eq!(model::number(&row, "revision").unwrap(), 7.0);
        assert!(!model::truth(
            model::field(&row, "task").unwrap(),
            "enabled"
        ));
        assert!(!policy::commit_execution(&mut row, &mut r, 5000.0).unwrap());
        assert_eq!(model::number(&row, "nextAt").unwrap(), 11000.0);
    }
}
#[test]
fn event_limit_includes_envelope_and_retention_limit_includes_result_envelope() {
    let mut r = run();
    let mut output = r.get("output").unwrap().clone();
    output.insert(
        "events",
        IvyJson::Array(vec![IvyJson::object([
            ("name", "event".into()),
            ("payload", "a".repeat(4090).into()),
        ])]),
    );
    r.insert("output", output);
    assert_eq!(
        policy::validate_publication(&r, &retention())
            .unwrap_err()
            .code,
        "content_too_large"
    );
    let r = run();
    let content = policy::result_content(&r).unwrap();
    let size = canonical(&content, usize::MAX).unwrap().len();
    let mut maximum = retention();
    maximum.insert("maximumBytes", IvyJson::Number(size as f64));
    policy::validate_publication(&r, &maximum).unwrap();
    maximum.insert("maximumBytes", IvyJson::Number(size as f64 - 1.0));
    assert_eq!(
        policy::validate_publication(&r, &maximum).unwrap_err().code,
        "limit_exceeded"
    );
}
#[test]
fn dc02_keys_and_values_reject_configured_secrets_without_false_empty_match() {
    for source in [
        r#"{"secret-token":0}"#,
        r#"[{"key":"prefix-secret-token-suffix"}]"#,
        r#"{"outer":{"secret-token":false}}"#,
    ] {
        assert!(policy::contains_configured_secret(
            &json(source),
            "secret-token"
        ));
    }
    assert!(!policy::contains_configured_secret(
        &json(r#"{"TOKEN":"configured by name"}"#),
        "secret-token"
    ));
    assert!(!policy::contains_configured_secret(
        &json(r#"{"anything":0}"#),
        ""
    ));
    assert!(policy::contains_configured_secret_json(
        &json(r#"{"prefix-\ud800-suffix":0}"#),
        &ivy_contracts::JsString(vec![0xd800])
    ));
}
