use super::*;
use crate::retention::graph_tests::{LIVE, PEAK};
use ivy_contracts::json::stringify;
use std::sync::atomic::Ordering;

fn reference_embed(bundle: &mut McpSchemaBundle, value: &IvyJson, path: &JsString) -> IvyResult<IvyJson> {
    if value.as_object().is_none() { return Ok(value.clone()); }
    let source = value;
    let mut references = HashMap::new();
    let mut added = vec![];
    if let Some(definitions) = source.get("$defs").and_then(IvyJson::as_object) {
        for (key, definition) in definitions {
            let mut deps = empty();
            let mut outside = false;
            dependencies(definition, source, &mut deps, &mut outside)?;
            let mut identity = IvyJson::object([("definition", definition.clone()), ("dependencies", deps)]);
            if outside { identity.insert("path", IvyJson::String(path.clone())); }
            let identity = ivy_contracts::hash::hash_json(&identity)?;
            let name = if let Some(name) = bundle.names.get(&identity) { name.clone() } else {
                let mut name = key.clone();
                let mut suffix = 2;
                while bundle.definitions.get_key(&name).is_some() {
                    name = cat(&[key, &format!("_{suffix}").as_str().into()]);
                    suffix += 1;
                }
                bundle.names.insert(identity, name.clone());
                bundle.definitions.insert(name.clone(), true.into());
                added.push((name.clone(), definition.clone()));
                name
            };
            references.insert(pointer_token(key), pointer_token(&name));
        }
    }
    for (name, mut definition) in added {
        rewrite(&mut definition, path, &references)?;
        bundle.definitions.insert(name, definition);
    }
    let mut root = source.clone();
    root.remove("$defs");
    rewrite(&mut root, path, &references)?;
    Ok(root)
}

fn reference_finish(bundle: &McpSchemaBundle, mut schema: IvyJson) -> IvyJson {
    if bundle.definitions.as_object().is_some_and(|v| !v.is_empty()) {
        schema.insert("$defs", bundle.definitions.clone());
    }
    schema
}

#[test]
fn finish_preserves_opaque_definitions_existing_fields_and_bytes() {
    let definitions = [empty(), IvyJson::object([
        (JsString(vec![0xd800]), IvyJson::object([
            ("description", IvyJson::String(JsString(vec![0xd801]))),
            ("const", IvyJson::Number(-0.0)),
        ])),
    ])];
    let mut duplicates = vec![("$defs".into(), true.into()), ("$defs".into(), false.into())];
    duplicates.push(("type".into(), "object".into()));
    for definitions in definitions { for schema in [empty(), IvyJson::object([
        ("$defs", "replaced".into()), ("properties", empty()),
    ]), IvyJson::Object(duplicates.clone())] {
        let bundle = McpSchemaBundle { definitions: definitions.clone(), names: HashMap::new() };
        let expected = reference_finish(&bundle, schema.clone());
        let actual = bundle.finish(schema);
        assert_eq!(stringify(&actual, usize::MAX).unwrap(), stringify(&expected, usize::MAX).unwrap());
    } }
}

#[test]
fn finish_transfers_definition_buffers_without_a_large_copy() {
    const CHILD: &str = "IVY_MCP_SCHEMA_FINISH_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "mcp_schema::ownership_tests::finish_transfers_definition_buffers_without_a_large_copy", "--nocapture"])
            .env(CHILD, "1").output().unwrap();
        eprint!("{}", String::from_utf8_lossy(&output.stderr));
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stdout));
        return;
    }
    for original in [true, false] {
        let value = IvyJson::from("S".repeat(1024 * 1024));
        let pointer = value.as_js_string().unwrap().0.as_ptr();
        let bundle = McpSchemaBundle { definitions: IvyJson::object([("Shared", value)]), names: HashMap::new() };
        let root = IvyJson::object([("type", "object".into())]);
        let before = LIVE.load(Ordering::Relaxed); PEAK.store(before, Ordering::Relaxed);
        let result = if original { reference_finish(&bundle, root) } else { bundle.finish(root) };
        let peak = PEAK.load(Ordering::Relaxed) - before;
        eprintln!("Hive MCP schema finish Rust allocation peak: {peak} bytes; original={original}; one 1 MiB definition text");
        if !original {
            assert_eq!(result.required("$defs").unwrap().required("Shared").unwrap().as_js_string().unwrap().0.as_ptr(), pointer);
            assert!(peak < 8 * 1024, "Schema finish copied definitions: {peak}");
        }
        drop(result);
    }
}

#[test]
fn structured_output_classification_accepts_borrowed_catalog_entries() {
    const CHILD: &str = "IVY_MCP_SCHEMA_CHECK_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "mcp_schema::ownership_tests::structured_output_classification_accepts_borrowed_catalog_entries", "--nocapture"])
            .env(CHILD, "1").output().unwrap();
        eprint!("{}", String::from_utf8_lossy(&output.stderr));
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stdout));
        return;
    }
    let bindings: Vec<_> = (0..4).map(|_| IvyJson::object([("definition", IvyJson::object([
        ("inputSchema", "I".repeat(1024 * 1024).into()),
        ("outputSchema", IvyJson::object([("type", "object".into()), ("description", "O".repeat(1024 * 1024).into())])),
    ]))])).collect();
    let before = LIVE.load(Ordering::Relaxed); PEAK.store(before, Ordering::Relaxed);
    assert!(crate::mcp::has_structured_output(bindings.iter()).unwrap());
    let peak = PEAK.load(Ordering::Relaxed) - before;
    eprintln!("Hive MCP borrowed output classification Rust allocation peak: {peak} bytes; four bindings with 1 MiB input/output text each");
    assert!(peak < 8 * 1024, "Output classification copied a catalog: {peak}");
}

#[test]
fn embed_keeps_hashes_duplicate_fields_graph_identity_and_errors() {
    let schema = |definition| IvyJson::object([
        ("$ref", "#/$defs/Shared".into()), ("$defs", IvyJson::object([("Shared", definition)])),
    ]);
    let mut deep = IvyJson::Null;
    for _ in 0..128 { deep = IvyJson::Array(vec![deep]); }
    let definitions = vec![
        IvyJson::object([("type", "string".into()), ("const", IvyJson::Number(-0.0))]),
        IvyJson::object([(JsString(vec![0xd800]), IvyJson::String(JsString(vec![0xdc00])))]),
        IvyJson::object([("$ref", "#/$defs/Shared".into())]),
        IvyJson::object([("$ref", "#/properties/target".into())]),
        IvyJson::object([("$ref", "#/$defs/missing".into()), ("const", IvyJson::Number(f64::NAN))]),
        IvyJson::object([("$ref", "#/required/00".into())]),
        IvyJson::object([("const", IvyJson::Number(f64::NAN))]),
        IvyJson::object([("const", deep)]),
    ];
    let duplicate = IvyJson::Object(vec![
        ("$defs".into(), IvyJson::object([("First", IvyJson::Bool(true))])),
        ("$id".into(), "removed-first".into()),
        ("$defs".into(), IvyJson::object([("Later", IvyJson::object([("type", "number".into())]))])),
        ("$id".into(), "preserved-second".into()),
        ("x".into(), 1.0.into()), ("x".into(), 2.0.into()),
    ]);
    let mut schemas = vec![IvyJson::Null, IvyJson::Bool(true), empty(), duplicate];
    for definition in definitions {
        let mut value = schema(definition);
        value.insert("properties", IvyJson::object([("target", IvyJson::object([("type", "string".into())]))]));
        value.insert("required", IvyJson::Array(vec!["target".into()]));
        schemas.push(value);
    }
    let mut expected = McpSchemaBundle::new();
    let mut actual = McpSchemaBundle::new();
    for path in [JsString::from("/left"), JsString(vec![47, 0xd800]), JsString::from("/right")] {
        for schema in &schemas {
            let bytes = |result: IvyResult<IvyJson>| result
                .and_then(|value| stringify(&value, usize::MAX)).map_err(|e| (e.code, e.message));
            assert_eq!(bytes(actual.embed(schema, &path)), bytes(reference_embed(&mut expected, schema, &path)));
            assert_eq!(actual.names, expected.names);
            assert_eq!(stringify(&actual.definitions, usize::MAX).unwrap(), stringify(&expected.definitions, usize::MAX).unwrap());
        }
    }
    assert_eq!(stringify(&actual.finish(empty()), usize::MAX).unwrap(), stringify(&expected.finish(empty()), usize::MAX).unwrap());
}

#[test]
fn embed_avoids_hash_and_discarded_root_definition_copies() {
    const CHILD: &str = "IVY_MCP_SCHEMA_EMBED_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "mcp_schema::ownership_tests::embed_avoids_hash_and_discarded_root_definition_copies", "--nocapture"])
            .env(CHILD, "1").output().unwrap();
        eprint!("{}", String::from_utf8_lossy(&output.stderr));
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stdout));
        return;
    }
    let mut original_peak = 0;
    let mut original_hash = String::new();
    for original in [true, false] {
        let source = IvyJson::object([
            ("$ref", "#/$defs/Shared".into()),
            ("$defs", IvyJson::object([("Shared", IvyJson::object([
                ("type", "string".into()), ("description", "S".repeat(1024 * 1024).into()),
            ]))])),
        ]);
        let path = JsString::from("/input");
        let mut bundle = McpSchemaBundle::new();
        let before = LIVE.load(Ordering::Relaxed); PEAK.store(before, Ordering::Relaxed);
        let root = if original { reference_embed(&mut bundle, &source, &path) } else { bundle.embed(&source, &path) }.unwrap();
        let result = bundle.finish(root);
        let peak = PEAK.load(Ordering::Relaxed) - before;
        let hash = ivy_contracts::hash::digest(stringify(&result, usize::MAX).unwrap());
        eprintln!("Hive MCP schema embed Rust allocation peak: {peak} bytes; original={original}; one 1 MiB definition text");
        if original { original_peak = peak; original_hash = hash; } else {
            assert_eq!(hash, original_hash);
            assert!(original_peak >= peak + 1024 * 1024, "Schema embedding retained a discarded definition copy: {peak} vs {original_peak}");
        }
    }
}
