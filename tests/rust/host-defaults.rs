use ivy_contracts::{IvyJson, json::parse};
use ivy_host_runtime::default_settings::{default_service_settings, with_defaults};
use std::path::Path;

#[test]
fn configured_null_arrays_false_zero_and_lone_surrogates_are_authoritative() {
    let defaults = parse(
        r#"{"nested":{"a":1,"b":2},"array":[1],"nullable":{"x":1},"flag":true,"number":5}"#,
        10000,
    )
    .unwrap();
    let configured = parse(r#"{"nested":{"a":0},"array":[],"nullable":null,"flag":false,"number":0,"extension":"\ud800","__proto__":{"custom":true}}"#, 10000).unwrap();
    let before = configured.clone();
    let merged = with_defaults(&defaults, &configured).unwrap();
    assert_eq!(
        merged.required("nested").unwrap().required("b").unwrap(),
        &IvyJson::Number(2.0)
    );
    for key in [
        "array",
        "nullable",
        "flag",
        "number",
        "extension",
        "__proto__",
    ] {
        assert_eq!(merged.get(key), configured.get(key));
    }
    assert_eq!(configured, before);
}

#[test]
fn service_policy_preserves_explicit_choices_and_passes_unknown_extensions_through() {
    let settings = parse(r#"{"backup":{"retain":14},"credentials":[]}"#, 10000).unwrap();
    let hive = default_service_settings(
        "hive",
        &settings,
        Path::new("/srv/ivy"),
        Path::new("/srv/ivy/config.json"),
    )
    .unwrap();
    assert_eq!(
        hive.required("backup").unwrap().required("retain").unwrap(),
        &IvyJson::Number(14.0)
    );
    assert_eq!(
        hive.required("backup")
            .unwrap()
            .required("enabled")
            .unwrap(),
        &IvyJson::Bool(true)
    );
    let agent = parse(
        r#"{"capabilities":[],"limits":{"maxPendingInputs":4}}"#,
        10000,
    )
    .unwrap();
    let resolved = default_service_settings(
        "agent-manager",
        &agent,
        Path::new("/srv/ivy"),
        Path::new("/srv/ivy/config.json"),
    )
    .unwrap();
    assert_eq!(resolved.get("capabilities"), agent.get("capabilities"));
    assert_eq!(
        resolved
            .required("limits")
            .unwrap()
            .required("maxPendingInputs")
            .unwrap(),
        &IvyJson::Number(4.0)
    );
    assert_eq!(
        default_service_settings(
            "extension",
            &settings,
            Path::new("/srv/ivy"),
            Path::new("/srv/ivy/config.json")
        )
        .unwrap(),
        settings
    );
}
