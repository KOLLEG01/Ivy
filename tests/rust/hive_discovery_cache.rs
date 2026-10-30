use super::*;

fn discovery() -> Discovery {
    Discovery {
        indexes: vec![],
        index_bytes: 0,
        core_index: vec![],
        core_bindings: CoreCalls::default(),
    }
}
fn index(identity: String) -> Index {
    Index {
        identity,
        tools: vec![],
        groups: vec![],
        description: JsString::default(),
        bytes: 0,
    }
}

#[test]
fn oversized_allocation_does_not_flush_other_providers() {
    let mut discovery = discovery();
    discovery.retain_index(&"stable".into(), index(String::new()));
    let retained = discovery.retain_index(
        &"large".into(),
        index(String::with_capacity(INDEX_CACHE_ENTRY_BYTES + 1)),
    );
    assert!(retained.identity.is_empty());
    assert_eq!(discovery.indexes.len(), 1);
    assert_eq!(discovery.indexes[0].0, JsString::from("stable"));
}

#[test]
fn utf16_capacity_counts_when_serialized_content_is_empty() {
    let mut discovery = discovery();
    let mut value = index(String::new());
    value.tools.push(IndexedTool {
        qualified_name: JsString::default(),
        namespace: JsString::default(),
        group: JsString::default(),
        group_description: JsString::default(),
        description: JsString(Vec::with_capacity(INDEX_CACHE_ENTRY_BYTES)),
        effect: "read",
        words: vec![],
        mcp: None,
    });
    let retained = discovery.retain_index(&"large".into(), value);
    assert!(retained.tools[0].description.0.is_empty());
    assert!(discovery.indexes.is_empty());
}

#[test]
fn total_retained_budget_evicts_oldest_provider() {
    let mut discovery = discovery();
    for id in 0..6 {
        discovery.retain_index(
            &id.to_string().into(),
            index(String::with_capacity(3 * 1024 * 1024)),
        );
        assert!(discovery.index_bytes <= INDEX_CACHE_BYTES);
    }
    assert_eq!(discovery.indexes.len(), 5);
    assert_eq!(discovery.indexes[0].0, JsString::from("1"));
}
