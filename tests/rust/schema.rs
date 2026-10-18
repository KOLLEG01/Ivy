use ivy_contracts::{
    IvyJson,
    json::{parse, stringify},
    schema::{bundle_local_schema, pointer_parts, scalar_types},
};

#[test]
fn dynamic_patterns_match_pinned_re2js() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let output = std::process::Command::new("node")
        .args([
            "--experimental-transform-types",
            "--disable-warning=ExperimentalWarning",
            "tests/fixtures/re2-oracle.mjs",
        ])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rows: Vec<serde_json::Value> = serde_json::from_slice(&output.stdout).unwrap();
    assert!(rows.len() > 2000);
    for row in rows {
        let pattern = parse(row["pattern"].as_str().unwrap(), 100_000).unwrap();
        let result = ivy_contracts::re2::Re2Pattern::compile(pattern.as_js_string().unwrap());
        assert_eq!(
            result.is_ok(),
            row["ok"].as_bool().unwrap(),
            "admission {}",
            row["pattern"]
        );
        match result {
            Err(error) => {
                assert_eq!(
                    error.code,
                    row["code"].as_str().unwrap(),
                    "{}",
                    row["pattern"]
                );
                assert_eq!(
                    error.message,
                    row["message"].as_str().unwrap(),
                    "{}",
                    row["pattern"]
                );
            }
            Ok(expression) => {
                for sample in row["matches"].as_array().unwrap() {
                    let input = parse(sample["input"].as_str().unwrap(), 100_000).unwrap();
                    assert_eq!(
                        expression.is_match(input.as_js_string().unwrap()),
                        sample["valid"].as_bool().unwrap(),
                        "pattern {}, input {}",
                        row["pattern"],
                        sample["input"]
                    );
                }
            }
        }
    }
}

#[test]
fn pattern_lifecycle_uses_bounded_native_stack() {
    std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(|| {
            let pattern = format!("{}a{}", "(".repeat(500), ")".repeat(500));
            let expression = ivy_contracts::re2::Re2Pattern::compile(&pattern.into()).unwrap();
            assert!(expression.is_match(&"a".into()));
            let nested = format!("{}a{}", "(".repeat(300), ")+".repeat(300));
            let expression = ivy_contracts::re2::Re2Pattern::compile(&nested.into()).unwrap();
            assert!(expression.is_match(&"a".repeat(20000).into()));
            assert!(!expression.is_match(&"b".into()));
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn dynamic_admission_matches_typescript_graph_and_error_order() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let output = std::process::Command::new("node")
        .args([
            "--experimental-transform-types",
            "--disable-warning=ExperimentalWarning",
            "tests/fixtures/schema-admission-oracle.mjs",
        ])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rows: Vec<serde_json::Value> = serde_json::from_slice(&output.stdout).unwrap();
    assert!(rows.len() > 150);
    for row in rows {
        let schema = parse(row["schema"].as_str().unwrap(), 1_000_000).unwrap();
        let result = match ivy_contracts::schema::admit_schema(&schema) {
            Ok(admitted) => {
                assert_eq!(admitted.schema(), &schema);
                IvyJson::object([("ok", true.into())])
            }
            Err(error) => IvyJson::object([
                ("ok", false.into()),
                ("code", IvyJson::String(error.code_js())),
                ("message", IvyJson::String(error.message_js())),
            ]),
        };
        assert_eq!(
            stringify(&result, 1_000_000).unwrap(),
            row["result"].as_str().unwrap(),
            "{}",
            row["name"]
        );
    }
}

#[test]
fn schema_graph_helpers_match_typescript() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let output = std::process::Command::new("node")
        .args([
            "--experimental-transform-types",
            "--disable-warning=ExperimentalWarning",
            "tests/fixtures/schema-oracle.mjs",
        ])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rows: Vec<serde_json::Value> = serde_json::from_slice(&output.stdout).unwrap();
    assert!(rows.len() > 250);
    for row in rows {
        let input = parse(row["input"].as_str().unwrap(), 2_000_000).unwrap();
        let before = stringify(&input, 2_000_000).unwrap();
        let result = match row["operation"].as_str().unwrap() {
            "pointer" => pointer_parts(input.as_js_string().unwrap())
                .map(|parts| IvyJson::Array(parts.into_iter().map(IvyJson::String).collect())),
            "scalar" => scalar_types(
                input.get("schema").unwrap(),
                input.get("pointer").unwrap().as_js_string().unwrap(),
            )
            .map(|types| IvyJson::Array(types.into_iter().map(IvyJson::from).collect())),
            "bundle" => bundle_local_schema(
                input.get("document").unwrap(),
                input.get("name").unwrap().as_js_string().unwrap(),
            ),
            operation => panic!("Unknown oracle operation {operation}"),
        };
        let actual = match result {
            Ok(value) => IvyJson::object([("ok", true.into()), ("value", value)]),
            Err(error) => IvyJson::object([
                ("ok", false.into()),
                ("code", IvyJson::String(error.code_js())),
                ("message", IvyJson::String(error.message_js())),
            ]),
        };
        assert_eq!(
            stringify(&actual, 2_000_000).unwrap(),
            row["result"].as_str().unwrap(),
            "{}",
            row["name"]
        );
        assert_eq!(
            stringify(&input, 2_000_000).unwrap(),
            before,
            "Input mutated: {}",
            row["name"]
        );
    }
}
#[test]
fn dynamic_meta_schema_matches_pinned_compiler() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let output = std::process::Command::new("node")
        .args([
            "--experimental-transform-types",
            "--disable-warning=ExperimentalWarning",
            "tests/fixtures/schema-meta-oracle.mjs",
        ])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rows: Vec<serde_json::Value> = serde_json::from_slice(&output.stdout).unwrap();
    assert!(rows.len() > 6000);
    for row in rows {
        let schema = parse(row["schema"].as_str().unwrap(), 1_000_000).unwrap();
        let result = match ivy_contracts::schema::admit_schema(&schema)
            .and_then(|s| s.validate_meta_schema())
        {
            Ok(()) => IvyJson::object([("ok", true.into())]),
            Err(error) => IvyJson::object([
                ("ok", false.into()),
                ("code", IvyJson::String(error.code_js())),
                ("message", IvyJson::String(error.message_js())),
            ]),
        };
        assert_eq!(
            stringify(&result, 1_000_000).unwrap(),
            row["result"].as_str().unwrap(),
            "{}",
            row["name"]
        );
    }
}

#[test]
fn meta_schema_validation_uses_bounded_native_stack() {
    std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(|| {
            let mut schema = IvyJson::object([("type", "integer".into())]);
            for _ in 0..64 {
                schema = IvyJson::object([("items", schema)]);
            }
            ivy_contracts::schema::admit_schema(&schema)
                .unwrap()
                .validate_meta_schema()
                .unwrap();
        })
        .unwrap()
        .join()
        .unwrap();
}
