use super::*;

fn catalog(reserved_bytes: usize) -> Catalog {
    // A short serialized value with a large retained capacity distinguishes heap
    // accounting from a serialized-size or entry-count limit.
    let mut text = Vec::with_capacity(reserved_bytes / 2);
    text.push(b'x' as u16);
    Catalog {
        namespaces: vec![],
        tools: vec![],
        notifications: vec![IvyJson::String(JsString(text))],
    }
}

#[test]
fn heap_budget_evicts_the_least_recent_catalog_without_invalidating_active_owners() {
    let mut cache = CatalogCache::default();
    let oldest = cache.insert("oldest".into(), catalog(7 * 1024 * 1024));
    let recent = cache.insert("recent".into(), catalog(7 * 1024 * 1024));
    cache.insert("third".into(), catalog(7 * 1024 * 1024));
    cache.insert("fourth".into(), catalog(7 * 1024 * 1024));
    assert!(Arc::ptr_eq(&oldest, &cache.get(&"oldest".into()).unwrap()));
    cache.insert("fifth".into(), catalog(7 * 1024 * 1024));
    assert!(cache.get(&"recent".into()).is_none());
    assert!(Arc::ptr_eq(&oldest, &cache.get(&"oldest".into()).unwrap()));
    assert!(cache.retained_bytes() <= CACHE_BYTES);
    assert_eq!(recent.notifications[0], "x".into());
    assert_eq!(Arc::strong_count(&recent), 1);
}

#[test]
fn an_oversized_replacement_is_available_to_its_caller_without_retaining_stale_state() {
    let mut cache = CatalogCache::default();
    let previous = cache.insert("node".into(), catalog(0));
    let replacement = cache.insert("node".into(), catalog(9 * 1024 * 1024));
    assert!(cache.get(&"node".into()).is_none());
    assert_eq!(Arc::strong_count(&previous), 1);
    assert_eq!(Arc::strong_count(&replacement), 1);
    assert_eq!(replacement.notifications[0], "x".into());
    assert!(cache.retained_bytes() <= CACHE_BYTES);
}

#[test]
fn small_catalogs_also_obey_the_entry_limit_and_share_the_same_owner() {
    let mut cache = CatalogCache::default();
    let first = cache.insert("0".into(), catalog(0));
    for id in 1..=CACHE_ENTRIES {
        let id: JsString = id.to_string().into();
        let admitted = cache.insert(id.clone(), catalog(0));
        assert!(Arc::ptr_eq(&admitted, &cache.get(&id).unwrap()));
    }
    assert!(cache.get(&"0".into()).is_none());
    assert_eq!(Arc::strong_count(&first), 1);
    assert_eq!(cache.entries.len(), CACHE_ENTRIES);
}
