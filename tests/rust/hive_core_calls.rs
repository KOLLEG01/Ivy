use super::construction_reference;
use ivy_contracts::{IvyJson, json::stringify};
use ivy_hive::discovery::CoreCalls;

#[test]
fn core_calls_original_definitions_hashes_and_resolution_order() {
    let original = construction_reference("coreCalls");
    let owner = CoreCalls;
    let rows = original.required("rows").unwrap().as_array().unwrap();
    assert_eq!(
        original
            .required("methods")
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        60
    );
    assert_eq!(rows.len(), 1278);
    for row in rows {
        let kind = row.required("kind").unwrap();
        let input = row.required("input").unwrap();
        let result = if kind == &"binding".into() {
            owner.binding(input.as_js_string().unwrap())
        } else {
            owner.resolve(input).map(|call| IvyJson::object([
                ("method", call.method.into()), ("params", call.params.clone()),
            ]))
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
        assert!(
            actual == expected,
            "Input: {}\nNative: {}\nOriginal: {}",
            stringify(row, usize::MAX).unwrap(),
            stringify(&actual, usize::MAX).unwrap(),
            stringify(&expected, usize::MAX).unwrap()
        );
    }
}
