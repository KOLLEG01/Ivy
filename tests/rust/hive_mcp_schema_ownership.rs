use super::*;
use crate::retention::graph_tests::{LIVE, PEAK};
use ivy_contracts::json::stringify;
use std::sync::atomic::Ordering;

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
