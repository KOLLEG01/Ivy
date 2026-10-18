use ivy_contracts::{IvyJson, json::parse};
use ivy_dashboards::validation::parse_input;

#[test]
fn original_zod_normalization_and_exact_error_messages() {
    let corpus = parse(
        include_str!("../fixtures/dashboards/validation-runtime.json"),
        usize::MAX,
    )
    .unwrap();
    let mut failures = Vec::new();
    for (index, row) in corpus.as_array().unwrap().iter().enumerate() {
        let name = row.required("schema").unwrap().as_str().unwrap();
        let input = if let Some(raw) = row.get("rawInput") {
            parse(&raw.as_str().unwrap(), usize::MAX).unwrap()
        } else {
            row.required("input").unwrap().clone()
        };
        let actual = parse_input(&name, &input);
        if let Some(expected) = row.get("value") {
            assert_eq!(
                &actual.unwrap_or_else(|e| panic!("case {index} ({name}): {e:?}")),
                expected,
                "case {index} ({name})"
            );
        } else {
            let error = actual.unwrap_err();
            assert_eq!(error.code, "invalid_arguments", "case {index} ({name})");
            if IvyJson::String(error.message_js()) != *row.required("message").unwrap() {
                failures.push(format!(
                    "case {index} ({name}): actual {} expected {}",
                    error.message,
                    row.required("message").unwrap().as_str().unwrap()
                ));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
