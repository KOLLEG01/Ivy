use ivy_contracts::json::{number_from_string, parse};
#[test]
fn number_strings_match_node_including_rounding_and_every_utf16_boundary() {
    let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let result = std::process::Command::new("node")
        .arg("tests/fixtures/number-string-oracle.mjs")
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let rows: Vec<serde_json::Value> = serde_json::from_slice(&result.stdout).unwrap();
    assert!(rows.len() > 69000);
    for row in rows {
        let text = row[0].as_str().unwrap();
        let value = parse(text, usize::MAX).unwrap();
        let actual = number_from_string(value.as_js_string().unwrap());
        let expected = &row[1];
        match expected.as_str() {
            Some("nan") => assert!(actual.is_nan(), "{text}: {actual}"),
            Some(bits) => assert_eq!(
                actual.to_bits(),
                u64::from_str_radix(bits, 16).unwrap(),
                "{text}"
            ),
            _ => panic!("Invalid oracle result"),
        }
    }
}
