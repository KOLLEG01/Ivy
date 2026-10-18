use ivy_contracts::{json::parse, limits::MANAGEMENT_FRAME_BYTES};
use ivy_dashboards::document::dashboard_document;

#[test]
fn original_typescript_document_bytes_and_utf16_channels() {
    let fixtures = parse(
        include_str!("../fixtures/dashboards/document.json"),
        MANAGEMENT_FRAME_BYTES,
    )
    .unwrap();
    for case in fixtures.as_array().unwrap() {
        let html = case.required("html").unwrap().as_js_string().unwrap();
        let channel = case.required("channel").unwrap().as_js_string().unwrap();
        let document = case.required("document").unwrap().as_js_string().unwrap();
        assert_eq!(&dashboard_document(html, channel).unwrap(), document);
    }
}

#[test]
fn registry_keeps_seven_rpc_five_mcp_and_both_storage_versions() {
    let registry = ivy_dashboards::registry();
    let ns = &registry.required("namespaces").unwrap().as_array().unwrap()[0];
    let tools = ns.required("tools").unwrap().as_array().unwrap();
    assert_eq!(tools.len(), 7);
    assert_eq!(
        tools
            .iter()
            .filter(|t| t.get("discovery").is_some())
            .count(),
        5
    );
    let contracts = registry.required("contracts").unwrap().as_array().unwrap();
    assert_eq!(
        contracts
            .iter()
            .map(|c| c.required("version").unwrap().as_str().unwrap())
            .collect::<Vec<_>>(),
        ["1.0.0", "1.1.0"]
    );
}
