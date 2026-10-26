use ivy_contracts::{
    IvyJson,
    json::{parse, parse_utf16},
};
use std::{path::Path, process::Command};

#[test]
fn raw_utf16_json_matches_javascript_without_accepting_invalid_escapes() {
    let output = Command::new("node")
        .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."))
        .args([
            "--experimental-transform-types",
            "--disable-warning=ExperimentalWarning",
            "tests/fixtures/json-oracle.mjs",
            "utf16",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let cases = parse(std::str::from_utf8(&output.stdout).unwrap(), usize::MAX).unwrap();
    assert_eq!(cases.as_array().unwrap().len(), 68);
    for (index, case) in cases.as_array().unwrap().iter().enumerate() {
        let input = case.required("input").unwrap().as_js_string().unwrap();
        let expected = case.required("expected").unwrap();
        let actual = parse_utf16(&input.0, usize::MAX);
        if expected.get("error") == Some(&IvyJson::Bool(true)) {
            assert_eq!(
                actual.unwrap_err().code,
                "invalid_arguments",
                "case {index}"
            );
        } else {
            assert_eq!(
                actual.unwrap(),
                expected.required("value").unwrap().clone(),
                "case {index}"
            );
            assert_eq!(
                parse_utf16(&input.0, 0).unwrap_err().code,
                "content_too_large"
            );
        }
    }
}
