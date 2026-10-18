use ivy_contracts::{IvyJson, json::{parse, stringify, canonical}};
use ivy_data_collector::schema::{parse_schema, registry};
use std::process::Command;

#[test]
fn schema_parsing_and_registry_match_the_original_zod_source() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let output = Command::new("node")
        .args(["--disable-warning=ExperimentalWarning", "tests/fixtures/data-collector-schema-oracle.mjs", "cases"])
        .current_dir(&root).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let oracle = parse(std::str::from_utf8(&output.stdout).unwrap(), usize::MAX).unwrap();
    let assets = oracle.get("assets").unwrap();
    assert_eq!(canonical(&registry(), usize::MAX).unwrap(), canonical(assets.get("registry").unwrap(), usize::MAX).unwrap());
    let committed = parse(include_str!("../../services/data-collector/rust/assets/schemas.json"), usize::MAX).unwrap();
    assert_eq!(canonical(&committed, usize::MAX).unwrap(), canonical(assets.get("documents").unwrap(), usize::MAX).unwrap());
    let rows = oracle.get("rows").unwrap().as_array().unwrap();
    assert!(rows.len() > 1000);
    for (index, row) in rows.iter().enumerate() {
        let name = row.get("name").unwrap().as_str().unwrap();
        let input = row.get("input").unwrap();
        let actual = parse_schema(&name, input);
        let expected = row.get("accepted") == Some(&IvyJson::Bool(true));
        assert_eq!(actual.is_ok(), expected, "case {index} {name}: {}", stringify(input, usize::MAX).unwrap());
        if expected {
            let actual = actual.unwrap();
            assert!(ivy_contracts::builtin_schema::json_equal(&actual, row.get("value").unwrap()), "case {index} {name}: {}", stringify(&actual, usize::MAX).unwrap());
        } else {
            let error = actual.unwrap_err();
            assert_eq!(error.code, "internal_error");
            assert_eq!(error.outcome, ivy_contracts::Outcome::Unknown);
        }
    }
    for number in [f64::INFINITY, f64::NEG_INFINITY, f64::NAN] {
        assert!(parse_schema("outputSchema", &IvyJson::object([("data", IvyJson::Number(number))])).is_err());
    }
}
