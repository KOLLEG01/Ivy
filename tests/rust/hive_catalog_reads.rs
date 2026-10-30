use super::*;
use crate::retention::graph_tests::{LIVE, PEAK};
use std::sync::atomic::Ordering;

#[test]
fn catalog_page_moves_selected_records_without_copying_schema_payloads() {
    const CHILD: &str = "IVY_CATALOG_PAGE_MEMORY_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "kernel::catalogs::read_tests::catalog_page_moves_selected_records_without_copying_schema_payloads", "--nocapture"])
            .env(CHILD, "1").output().unwrap();
        eprint!("{}", String::from_utf8_lossy(&output.stderr));
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stdout));
        assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed;"), "Memory child selected no test");
        return;
    }
    let store = Store::open(std::path::Path::new(":memory:")).unwrap();
    let schema_bytes = 64 * 1024;
    let tools = fixture_tools(schema_bytes);
    let payload = |tool: &IvyJson| tool.get("definition").unwrap()
        .get("inputSchema").unwrap().get("description").unwrap()
        .as_js_string().unwrap().0.as_ptr() as usize;
    let pointers: Vec<_> = tools.iter().map(payload).collect();
    let expected = hash_json(&IvyJson::Array(tools.clone())).unwrap();
    let identity = IvyJson::object([("method", "fixture.tools.list".into())]);
    let before = LIVE.load(Ordering::Relaxed);
    PEAK.store(before, Ordering::Relaxed);
    let page = crate::paging::catalog_page(&store, tools, &identity,
        |tool| tool.get("qualifiedName").unwrap().as_js_string().unwrap().clone(), None, None).unwrap();
    let peak = PEAK.load(Ordering::Relaxed) - before;
    let items = page.get("items").unwrap().as_array().unwrap();
    assert_eq!(items.len(), 8);
    assert!(page.get("nextCursor").unwrap().is_null());
    assert_eq!(items.iter().map(payload).collect::<Vec<_>>(), pointers);
    assert_eq!(hash_json(page.get("items").unwrap()).unwrap(), expected);
    eprintln!("Hive catalog page Rust allocation peak: {peak} bytes above owned input for 1 MiB opaque schema text");
    // Response sizing may allocate one encoded record; the 2 MiB UTF-16 payload
    // must move into the page rather than being cloned alongside the input.
    assert!(peak < 1024 * 1024, "Catalog page copied schema payloads: {peak}");
}

fn fixture_tools(schema_bytes: usize) -> Vec<IvyJson> {
    (0..8).map(|index| IvyJson::object([
        ("qualifiedName", format!("fixture.call{index}").into()),
        ("definitionHash", "sha256:fixture".into()),
        ("definition", IvyJson::object([
            ("name", format!("call{index}").into()),
            ("inputSchema", IvyJson::object([("description", "s".repeat(schema_bytes).into())])),
            ("outputSchema", IvyJson::object([("description", "s".repeat(schema_bytes).into())])),
        ])),
    ])).collect()
}

#[test]
fn tool_page_identity_keeps_hashes_filters_and_error_order() {
    let id = JsString::from("fixture");
    let guide = IvyJson::String(JsString(vec![0xd800]));
    let tools = fixture_tools(0);
    let expected = hash_json(&IvyJson::object([
        ("nodeId", id.clone().into()), ("tools", IvyJson::Array(tools.clone())), ("guide", guide.clone()),
    ])).unwrap();
    let params = IvyJson::object([("namePrefix", "call3".into())]);
    let (hash, filtered) = tool_page_records(&id, &guide, tools.clone(), &params).unwrap();
    assert_eq!(hash, expected);
    assert_eq!(filtered, vec![tools[3].clone()]);
    let (hash, all) = tool_page_records(&id, &guide, tools.clone(), &IvyJson::Null).unwrap();
    assert_eq!(hash, expected);
    assert_eq!(all, tools);
    let malformed = vec![IvyJson::object([("definition", IvyJson::Null)])];
    let guide = IvyJson::Number(f64::NAN);
    assert_eq!(tool_page_records(&id, &guide, malformed.clone(), &params).unwrap_err().code, "internal_error");
    assert_eq!(tool_page_records(&id, &guide, malformed, &IvyJson::Null).unwrap_err().code, "invalid_arguments");
}

#[test]
fn tool_page_identity_does_not_clone_the_full_tool_vector() {
    const CHILD: &str = "IVY_TOOL_PAGE_MEMORY_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "kernel::catalogs::read_tests::tool_page_identity_does_not_clone_the_full_tool_vector", "--nocapture"])
            .env(CHILD, "1").output().unwrap();
        eprint!("{}", String::from_utf8_lossy(&output.stderr));
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stdout));
        assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed;"), "Memory child selected no test");
        return;
    }
    let id = JsString::from("fixture");
    let guide = IvyJson::from("Fixture guide.");
    let schema_bytes = 64 * 1024;
    let tools = fixture_tools(schema_bytes);
    // Vec::clone copies initialized UTF-16 units, not the input's spare capacity.
    let copied_schema_bytes = 8 * 2 * schema_bytes * std::mem::size_of::<u16>();
    let before = LIVE.load(Ordering::Relaxed);
    PEAK.store(before, Ordering::Relaxed);
    let identity = IvyJson::object([
        ("nodeId", id.clone().into()), ("tools", IvyJson::Array(tools.clone())), ("guide", guide.clone()),
    ]);
    let expected = hash_json(&identity).unwrap();
    let cloned_peak = PEAK.load(Ordering::Relaxed) - before;
    drop(identity);
    let before = LIVE.load(Ordering::Relaxed);
    PEAK.store(before, Ordering::Relaxed);
    let (hash, returned) = tool_page_records(&id, &guide, tools, &IvyJson::Null).unwrap();
    let moved_peak = PEAK.load(Ordering::Relaxed) - before;
    assert_eq!(hash, expected);
    assert_eq!(returned.len(), 8);
    eprintln!("Hive tool-page identity Rust allocation peak: cloned identity {cloned_peak} bytes, moved identity {moved_peak} bytes for 1 MiB opaque schema text");
    // Compare allocator requests above the already-owned input, excluding RSS.
    assert!(cloned_peak > moved_peak + isize::try_from(copied_schema_bytes * 3 / 4).unwrap());
}
