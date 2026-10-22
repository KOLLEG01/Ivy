use ivy_contracts::{
    IvyJson,
    json::{parse, stringify},
    schema::{bundle_local_schema, pointer_parts, scalar_types},
};

#[test]
fn compiler_plans_match_original_reference_target_compilation() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let output = std::process::Command::new("node")
        .args([
            "--experimental-transform-types",
            "--disable-warning=ExperimentalWarning",
            "tests/fixtures/schema-compiler-plan-oracle.mjs",
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
    assert!(rows.len() > 1500);
    for row in rows {
        let schema = parse(row["schema"].as_str().unwrap(), 1_000_000).unwrap();
        let admitted = ivy_contracts::schema::admit_schema(&schema).unwrap();
        let plan = admitted.compiler_plan();
        assert_eq!(
            plan.is_ok(),
            row["ok"].as_bool().unwrap(),
            "schema {}",
            row["schema"]
        );
        if let Ok(plan) = plan {
            assert!(!plan.units.is_empty());
            for reference in plan.references {
                assert!(
                    plan.units[reference.from_unit].nodes[reference.from_node]
                        .reference
                        .is_some()
                );
                assert!(reference.to_unit < plan.units.len());
            }
        }
    }
}

#[test]
fn compiler_plans_reuse_consuming_reference_targets_on_a_small_stack() {
    std::thread::Builder::new().stack_size(256 * 1024).spawn(|| {
        let schema = parse(r##"{"$defs":{"node":{"type":"object","properties":{"one":{"$ref":"#/$defs/node"},"two":{"$ref":"#/$defs/node"}}}},"$ref":"#/$defs/node"}"##, 100_000).unwrap();
        let admitted = ivy_contracts::schema::admit_schema(&schema).unwrap();
        let plan = admitted.compiler_plan().unwrap();
        assert_eq!(plan.units.len(), 2);
        assert_eq!(plan.references.len(), 3);
        assert_eq!(plan.references.iter().filter(|r| r.to_unit == 1).count(), 3);
    }).unwrap().join().unwrap();
}

#[test]
fn compiler_resource_resolution_matches_aliases_pointers_and_scopes() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let output = std::process::Command::new("node")
        .args([
            "--experimental-transform-types",
            "--disable-warning=ExperimentalWarning",
            "tests/fixtures/schema-resolution-oracle.mjs",
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
    assert!(rows.len() > 30);
    let mut checked = 0;
    for row in rows {
        let schema = parse(row["schema"].as_str().unwrap(), 1_000_000).unwrap();
        let admitted = ivy_contracts::schema::admit_schema(&schema).unwrap();
        let resources = admitted.compiler_resources();
        assert_eq!(
            resources.is_ok(),
            row["registered"].as_bool().unwrap(),
            "registration {}",
            row["schema"]
        );
        let Ok(resources) = resources else {
            continue;
        };
        for query in row["queries"].as_array().unwrap() {
            let IvyJson::String(ref base) =
                parse(query["base"].as_str().unwrap(), 100_000).unwrap()
            else {
                panic!("base");
            };
            let IvyJson::String(ref reference) =
                parse(query["ref"].as_str().unwrap(), 100_000).unwrap()
            else {
                panic!("reference");
            };
            let actual = match resources.resolve(base, reference) {
                Ok(Some(target)) => {
                    let root = if std::ptr::eq(target.root_schema, admitted.schema()) {
                        "submitted".into()
                    } else {
                        IvyJson::String(
                            target
                                .root_schema
                                .get("$id")
                                .and_then(IvyJson::as_js_string)
                                .unwrap()
                                .clone(),
                        )
                    };
                    IvyJson::object([
                        ("ok", true.into()),
                        ("found", true.into()),
                        (
                            "schema",
                            stringify(target.schema, 1_000_000).unwrap().into(),
                        ),
                        ("base", IvyJson::String(target.base_id)),
                        ("root", root),
                        ("rootBase", IvyJson::String(target.root_base_id)),
                    ])
                }
                Ok(None) => IvyJson::object([("ok", true.into()), ("found", false.into())]),
                Err(error) => {
                    assert_eq!(error.code_js(), "registry_invalid".into());
                    IvyJson::object([("ok", false.into())])
                }
            };
            assert_eq!(
                stringify(&actual, 2_000_000).unwrap(),
                query["result"].as_str().unwrap(),
                "schema {} base {} ref {}",
                row["schema"],
                query["base"],
                query["ref"]
            );
            checked += 1;
        }
    }
    assert!(checked > 4000);
}

#[test]
fn inline_compiler_walk_matches_branch_selection_and_scopes() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let output = std::process::Command::new("node")
        .args([
            "--experimental-transform-types",
            "--disable-warning=ExperimentalWarning",
            "tests/fixtures/schema-compiler-walk-oracle.mjs",
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
    assert!(rows.len() > 3000);
    for row in rows {
        let schema = parse(row["schema"].as_str().unwrap(), 1_000_000).unwrap();
        let admitted = ivy_contracts::schema::admit_schema(&schema).unwrap();
        let actual = match admitted.compiler_walk() {
            Ok(mut walk) => {
                walk.nodes.sort_by(|a, b| a.pointer.0.cmp(&b.pointer.0));
                let nodes = walk
                    .nodes
                    .into_iter()
                    .map(|node| {
                        IvyJson::object([
                            ("pointer", IvyJson::String(node.pointer)),
                            ("base", IvyJson::String(node.base_id)),
                            ("schema", stringify(node.schema, 1_000_000).unwrap().into()),
                            (
                                "ref",
                                node.reference
                                    .map_or(IvyJson::Null, |r| IvyJson::String(r.clone())),
                            ),
                        ])
                    })
                    .collect();
                IvyJson::object([
                    ("ok", true.into()),
                    ("base", IvyJson::String(walk.root_base_id)),
                    ("nodes", IvyJson::Array(nodes)),
                ])
            }
            Err(error) => {
                assert_eq!(error.code_js(), "registry_invalid".into());
                IvyJson::object([("ok", false.into())])
            }
        };
        assert_eq!(
            stringify(&actual, 2_000_000).unwrap(),
            row["result"].as_str().unwrap(),
            "schema {}",
            row["schema"]
        );
    }
}

#[test]
fn compiler_keyword_values_match_pinned_compiler() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let output = std::process::Command::new("node")
        .args([
            "--experimental-transform-types",
            "--disable-warning=ExperimentalWarning",
            "tests/fixtures/schema-keywords-oracle.mjs",
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
    assert!(rows.len() > 3000);
    for row in rows {
        let schema = parse(row["schema"].as_str().unwrap(), 1_000_000).unwrap();
        let result = ivy_contracts::schema::validate_compiler_keyword_values(&schema);
        if let Err(error) = &result {
            assert_eq!(error.code_js(), "registry_invalid".into());
        }
        assert_eq!(
            result.is_ok(),
            row["ok"].as_bool().unwrap(),
            "schema {}",
            row["schema"]
        );
    }
}

#[test]
fn resource_registration_matches_compiler_builtins_and_inherited_names() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let output = std::process::Command::new("node")
        .args([
            "--experimental-transform-types",
            "--disable-warning=ExperimentalWarning",
            "tests/fixtures/schema-registration-oracle.mjs",
        ])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let corpus: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let rows = corpus["rows"].as_array().unwrap();
    assert!(rows.len() > 1500);
    for row in rows {
        let schema = parse(row["schema"].as_str().unwrap(), 1_000_000).unwrap();
        let admitted = ivy_contracts::schema::admit_schema(&schema).unwrap();
        let actual = match admitted.registered_resources() {
            Ok(index) => {
                let mut refs = Vec::new();
                let mut local = Vec::new();
                for reference in index.references.iter().filter(|r| r.alias) {
                    if reference.id.0.first() == Some(&35) {
                        local.push((
                            reference.id.clone(),
                            index.graph.nodes[reference.node].schema.clone(),
                        ));
                    } else {
                        refs.push((
                            reference.id.clone(),
                            IvyJson::String(reference.full_path.clone()),
                        ));
                    }
                }
                IvyJson::object([
                    ("ok", true.into()),
                    ("base", IvyJson::String(index.base_id)),
                    ("registered", index.registered_root.into()),
                    ("refs", IvyJson::Object(refs)),
                    ("local", IvyJson::Object(local)),
                ])
            }
            Err(error) => {
                assert_eq!(error.code_js(), "registry_invalid".into());
                IvyJson::object([("ok", false.into())])
            }
        };
        assert_eq!(
            stringify(&actual, 2_000_000).unwrap(),
            row["result"].as_str().unwrap(),
            "schema {}",
            row["schema"]
        );
    }
}

#[test]
fn resource_ids_match_compiler_normalization_and_local_indexing() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let output = std::process::Command::new("node")
        .args([
            "--experimental-transform-types",
            "--disable-warning=ExperimentalWarning",
            "tests/fixtures/schema-resource-index-oracle.mjs",
        ])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let corpus: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let rows = corpus["fullPaths"].as_array().unwrap();
    assert!(rows.len() > 250);
    for row in rows {
        let IvyJson::String(ref input) = parse(row["input"].as_str().unwrap(), 100_000).unwrap()
        else {
            panic!("Expected ID string");
        };
        let actual = match ivy_contracts::schema::uri::full_path(&input) {
            Ok(path) => IvyJson::object([
                ("ok", true.into()),
                (
                    "id",
                    IvyJson::String(ivy_contracts::schema::uri::normalize_id(&input)),
                ),
                ("path", IvyJson::String(path)),
            ]),
            Err(error) => {
                assert_eq!(error.code_js(), "registry_invalid".into());
                IvyJson::object([("ok", false.into())])
            }
        };
        assert_eq!(
            stringify(&actual, 100_000).unwrap(),
            row["result"].as_str().unwrap(),
            "ID {}",
            row["input"]
        );
    }
    let rows = corpus["indexes"].as_array().unwrap();
    assert!(rows.len() > 2500);
    for row in rows {
        let schema = parse(row["schema"].as_str().unwrap(), 1_000_000).unwrap();
        let admitted = ivy_contracts::schema::admit_schema(&schema).unwrap();
        let actual = match admitted.resource_index() {
            Ok(index) => {
                let mut refs = Vec::new();
                let mut local = Vec::new();
                for reference in index.references.iter().filter(|r| r.alias) {
                    if reference.id.0.first() == Some(&35) {
                        local.push((
                            reference.id.clone(),
                            index.graph.nodes[reference.node].schema.clone(),
                        ));
                    } else {
                        refs.push((
                            reference.id.clone(),
                            IvyJson::String(reference.full_path.clone()),
                        ));
                    }
                }
                IvyJson::object([
                    ("ok", true.into()),
                    ("refs", IvyJson::Object(refs)),
                    ("local", IvyJson::Object(local)),
                ])
            }
            Err(error) => {
                assert_eq!(error.code_js(), "registry_invalid".into());
                IvyJson::object([("ok", false.into())])
            }
        };
        assert_eq!(
            stringify(&actual, 1_000_000).unwrap(),
            row["result"].as_str().unwrap(),
            "schema {}",
            row["schema"]
        );
    }
}

#[test]
fn uri_resolution_matches_compiler_authority_and_scope_rules() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let output = std::process::Command::new("node")
        .arg("tests/fixtures/schema-uri-resolve-oracle.mjs")
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rows: Vec<serde_json::Value> = serde_json::from_slice(&output.stdout).unwrap();
    assert!(rows.len() > 3000);
    for row in rows {
        let IvyJson::String(ref base) = parse(row["base"].as_str().unwrap(), 100_000).unwrap()
        else {
            panic!("Expected base string");
        };
        let IvyJson::String(ref relative) =
            parse(row["relative"].as_str().unwrap(), 100_000).unwrap()
        else {
            panic!("Expected relative string");
        };
        let actual = match ivy_contracts::schema::uri::resolve(&base, &relative) {
            Ok(value) => IvyJson::object([("ok", true.into()), ("value", IvyJson::String(value))]),
            Err(error) => {
                assert_eq!(error.code_js(), "registry_invalid".into());
                IvyJson::object([("ok", false.into())])
            }
        };
        assert_eq!(
            stringify(&actual, 100_000).unwrap(),
            row["result"].as_str().unwrap(),
            "base={} relative={}",
            row["base"],
            row["relative"]
        );
    }
}

#[test]
fn uri_components_match_compiler_encoding_and_dot_segments() {
    use ivy_contracts::schema::uri::{
        Component::{Path, QueryOrFragment, Userinfo},
        normalize_component, normalize_percent, remove_dot_segments, serialize_component,
    };
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let output = std::process::Command::new("node")
        .arg("tests/fixtures/schema-uri-oracle.mjs")
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
        let IvyJson::String(ref input) = parse(row["input"].as_str().unwrap(), 100_000).unwrap()
        else {
            panic!("Expected string input");
        };
        let actual = IvyJson::Array(
            [
                normalize_percent(&input, false),
                normalize_percent(&input, true),
                normalize_component(&input, Path),
                normalize_component(&input, QueryOrFragment),
                serialize_component(&input, Path, false),
                serialize_component(&input, Path, true),
                serialize_component(&input, QueryOrFragment, false),
                serialize_component(&input, QueryOrFragment, false),
                serialize_component(&input, Userinfo, false),
                remove_dot_segments(&input),
            ]
            .into_iter()
            .map(IvyJson::String)
            .collect(),
        );
        assert_eq!(
            stringify(&actual, 100_000).unwrap(),
            row["output"].as_str().unwrap(),
            "{}",
            row["input"]
        );
    }
}

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

#[test]
fn resource_walk_matches_compiler_annotation_and_parent_scope_rules() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let output = std::process::Command::new("node")
        .args([
            "--experimental-transform-types",
            "--disable-warning=ExperimentalWarning",
            "tests/fixtures/schema-resources-oracle.mjs",
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
    assert!(rows.len() > 1000);
    for row in rows {
        let schema = parse(row["schema"].as_str().unwrap(), 1_000_000).unwrap();
        let admitted = ivy_contracts::schema::admit_schema(&schema).unwrap();
        let graph = admitted.resource_graph();
        let nodes = graph
            .nodes
            .iter()
            .map(|node| {
                IvyJson::object([
                    ("pointer", IvyJson::String(node.pointer.clone())),
                    (
                        "parent",
                        node.parent.map_or(IvyJson::Null, |parent| {
                            IvyJson::String(graph.nodes[parent].pointer.clone())
                        }),
                    ),
                    ("schema", stringify(node.schema, 1_000_000).unwrap().into()),
                ])
            })
            .collect();
        let actual = IvyJson::object([
            ("isolated", graph.isolated.into()),
            ("nodes", IvyJson::Array(nodes)),
        ]);
        assert_eq!(
            stringify(&actual, 2_000_000).unwrap(),
            row["result"].as_str().unwrap(),
            "{}",
            row["name"]
        );
    }
}
