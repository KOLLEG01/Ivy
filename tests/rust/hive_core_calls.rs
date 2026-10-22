use ivy_contracts::{
    IvyJson,
    json::{parse, stringify},
};
use ivy_hive::discovery::CoreCalls;
use std::{fs, path::Path, process::Command};

fn save(methods: &IvyJson, rows: &[IvyJson]) {
    if let Some(path) = std::env::var_os("IVY_HIVE_CORE_CALLS_CORPUS") {
        fs::write(
            path,
            stringify(
                &IvyJson::object([
                    ("methods", methods.clone()),
                    ("rows", IvyJson::Array(rows.to_vec())),
                ]),
                usize::MAX,
            )
            .unwrap(),
        )
        .unwrap();
    }
}

#[test]
fn core_calls_original_definitions_hashes_and_resolution_order() {
    let output = Command::new(std::env::var_os("IVY_TEST_NODE").unwrap_or_else(|| "node".into()))
        .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."))
        .arg("tests/fixtures/hive-core-calls-oracle.mjs")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let original = parse(std::str::from_utf8(&output.stdout).unwrap(), usize::MAX).unwrap();
    let mut owner = CoreCalls::default();
    let rows = original.required("rows").unwrap().as_array().unwrap();
    let mut observed = vec![];
    for row in rows {
        let kind = row.required("kind").unwrap();
        let input = row.required("input").unwrap();
        let result = if kind == &"binding".into() {
            owner.binding(input.as_js_string().unwrap())
        } else {
            owner.resolve(input)
        };
        let actual = match result {
            Ok(value) => IvyJson::object([("value", value)]),
            Err(error) => IvyJson::object([("error", error.to_wire())]),
        };
        let expected = if let Some(value) = row.get("value") {
            IvyJson::object([("value", value.clone())])
        } else {
            IvyJson::object([("error", row.required("error").unwrap().clone())])
        };
        observed.push(IvyJson::object([
            ("input", row.clone()),
            ("native", actual.clone()),
        ]));
        if actual != expected {
            save(original.required("methods").unwrap(), &observed);
        }
        assert_eq!(actual, expected, "{}", stringify(row, usize::MAX).unwrap());
    }
    save(original.required("methods").unwrap(), &observed);
    assert!(!rows.is_empty());
}
