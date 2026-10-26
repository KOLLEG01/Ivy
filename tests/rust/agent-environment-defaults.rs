#[path = "../fixtures/reference-data.rs"]
mod reference_data;
use ivy_agent_manager::environment_defaults::load_environment_defaults;
use ivy_contracts::{
    IvyJson, JsString,
    hash::hash_json,
    json::{canonical, parse},
};
use std::{path::PathBuf, process::Command};

fn projection(value: &IvyJson) -> IvyJson {
    IvyJson::object([
        ("documentHash", hash_json(value).unwrap().into()),
        (
            "urls",
            IvyJson::Array(
                value
                    .required("mcp")
                    .unwrap()
                    .required("servers")
                    .unwrap()
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|server| {
                        IvyJson::object([
                            ("name", server.required("name").unwrap().clone()),
                            ("url", server.required("url").unwrap().clone()),
                        ])
                    })
                    .collect(),
            ),
        ),
    ])
}
#[test]
fn generated_defaults_match_actual_original_loader_and_return_independent_documents() {
    let output = reference_data::read(include_bytes!("../fixtures/agent-environment-defaults-cases.json.gz"));
    let oracle = parse(std::str::from_utf8(&output).unwrap(), usize::MAX).unwrap();
    let rows = oracle.required("rows").unwrap().as_array().unwrap();
    assert!(rows.len() > 700);
    for row in rows {
        let input = parse(
            &row.required("input").unwrap().as_str().unwrap(),
            usize::MAX,
        )
        .unwrap();
        let actual = match load_environment_defaults(
            &"ignored fixture artifact".into(),
            input.as_js_string().unwrap(),
        ) {
            Ok(value) => IvyJson::object([("ok", true.into()), ("value", projection(&value))]),
            Err(error) => IvyJson::object([("ok", false.into()), ("error", error.to_wire())]),
        };
        assert_eq!(
            canonical(&actual, usize::MAX).unwrap(),
            canonical(row.required("result").unwrap(), usize::MAX).unwrap(),
            "{input:?}"
        );
    }
    let input = JsString::from("https://example.invalid/prefix/");
    let mut first = load_environment_defaults(&"".into(), &input).unwrap();
    let expected = projection(&first);
    first.insert("instructions", IvyJson::Null);
    let second = load_environment_defaults(&"different ignored artifact".into(), &input).unwrap();
    assert_eq!(projection(&second), expected);
    assert_eq!(oracle.get("isolated"), Some(&IvyJson::Bool(true)));
    let embedded = parse(
        include_str!("../../services/agent-manager/rust/src/environment-defaults.json"),
        usize::MAX,
    )
    .unwrap();
    assert_eq!(
        oracle.get("sourceHash"),
        Some(&IvyJson::from(hash_json(&embedded).unwrap()))
    );
}
