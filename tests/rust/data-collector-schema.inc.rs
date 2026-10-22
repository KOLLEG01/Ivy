use ivy_contracts::{IvyJson, Outcome, json::parse};
use ivy_data_collector::schema::{parse_schema, registry};

fn json(text: &str) -> IvyJson {
    parse(text, usize::MAX).unwrap()
}
fn task() -> IvyJson {
    json(
        r#"{"id":"fixture","name":"Fixture","enabled":false,"intervalSeconds":60,"timeoutSeconds":5,"memoryMb":128,"script":"export default()=>({data:null})","dependencies":{},"config":{},"secretNames":[],"retention":{"maximumCount":1000,"maximumAgeDays":7,"maximumBytes":104857600}}"#,
    )
}

#[test]
fn schema_applies_documented_defaults_and_keeps_public_registration() {
    let settings = parse_schema("settingsSchema", &json(r#"{"ingress":{"port":8080}}"#)).unwrap();
    assert_eq!(settings.get("rootName"), Some(&"DataCollector".into()));
    assert_eq!(settings.get("concurrency"), Some(&IvyJson::Number(2.0)));
    assert_eq!(settings.get("maximumTasks"), Some(&IvyJson::Number(100.0)));
    assert_eq!(settings.get("retention"), task().get("retention"));
    assert_eq!(settings.get("secrets"), Some(&json("{}")));
    assert_eq!(
        settings.required("ingress").unwrap().get("host"),
        Some(&"127.0.0.1".into())
    );
    assert_eq!(registry().get("mcpPrefix"), Some(&"data_collector".into()));
    for input in [
        r#"{"view":"tasks"}"#,
        r#"{"view":"handoff","jobId":"job","expectedRevision":1}"#,
    ] {
        assert_eq!(
            parse_schema("readSchema", &json(input)).unwrap(),
            json(input)
        );
    }
}

#[test]
fn schema_preserves_json_members_and_validates_dictionary_keys() {
    let data =
        json(r#"{"__proto__":1,"constructor":2,"nested":{"__proto__":3,"items":[null,false]}}"#);
    let output = IvyJson::object([("data", data.clone()), ("state", IvyJson::Null)]);
    assert_eq!(parse_schema("outputSchema", &output).unwrap(), output);
    let mut value = task();
    value.insert("config", data.clone());
    let parsed = parse_schema("taskSchema", &value).unwrap();
    assert_eq!(parsed.get("config"), Some(&data));
    // Runtime-looking member names are data; dictionary constraints still apply.
    value.insert("dependencies", json(r#"{"__proto__":"1.2.3"}"#));
    assert!(parse_schema("taskSchema", &value).is_err());
    assert!(
        parse_schema(
            "settingsSchema",
            &json(r#"{"secrets":{"__proto__":"private"}}"#)
        )
        .is_err()
    );
}

#[test]
fn schema_rejects_invalid_shapes_and_domain_conflicts_without_exposing_values() {
    let mut conflicting = task();
    conflicting.insert("allowUnauthenticatedInput", true.into());
    conflicting.insert("inputSecretName", "TOKEN".into());
    let cases = [
        ("taskSchema", conflicting),
        ("settingsSchema", json(r#"{"concurrency":0}"#)),
        ("settingsSchema", json(r#"{"unknown":"private"}"#)),
        (
            "mcpSchema",
            json(
                r#"{"serviceNodeId":"host.agent","cwd":"relative","server":"fixture","tools":["one","one"]}"#,
            ),
        ),
        ("outputSchema", json(r#"{"state":null}"#)),
        (
            "outputSchema",
            json(r#"{"data":null,"events":[{"name":"INVALID","payload":null}]}"#),
        ),
        (
            "readSchema",
            json(r#"{"view":"result","id":"fixture","revision":0}"#),
        ),
    ];
    for (name, input) in cases {
        let error = parse_schema(name, &input).unwrap_err();
        assert_eq!(error.code, "internal_error");
        assert_eq!(error.outcome, Outcome::Unknown);
        assert!(!error.message.contains("private"));
    }
    for number in [f64::INFINITY, f64::NEG_INFINITY, f64::NAN] {
        let input = IvyJson::object([("data", IvyJson::Array(vec![IvyJson::Number(number)]))]);
        assert!(parse_schema("outputSchema", &input).is_err());
    }
}
