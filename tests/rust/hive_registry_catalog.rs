use super::*;

#[test]
fn binding_wire_keeps_field_order_and_opaque_schema_values() {
    for source in [
        r#"{"before":"\ud800","inputSchema":{"description":"\udfff"},"extra":[1,true],"outputSchema":false,"after":17}"#,
        r#"{"outputSchema":true,"inputSchema":false}"#,
        r#"{"inputSchema":null,"outputSchema":null}"#,
        r#"{"inputSchema":true,"extension":{"unknown":true}}"#,
        r#"{"outputSchema":{"type":"string"}}"#,
        r#"{"description":"No schemas"}"#,
    ] {
        let definition = ivy_contracts::json::parse(source, usize::MAX).unwrap();
        let binding = ToolBinding::new("fixture.call".into(), "definition-hash".into(), definition.clone());
        assert_eq!(binding.definition(), definition);
        assert_eq!(binding.to_wire().get("definition"), Some(&definition));
        assert_eq!(binding.to_wire().get("definitionHash"), Some(&"definition-hash".into()));
        assert_eq!(ivy_contracts::hash::hash_json(&binding.definition()).unwrap(),
            ivy_contracts::hash::hash_json(&definition).unwrap());
    }
}

#[test]
fn native_notification_fields_preserve_utf16_and_opaque_payload_schemas() {
    for schema in ["true", "false", r#"{"description":"\ud802","unknown":[9007199254740992,{"x":null}]}"#] {
        let registry = ivy_contracts::json::parse(&format!(
            r#"{{"namespaces":[{{"namespace":"fixture","notifications":[{{"name":"\ud800","version":"1.0.0","description":"\udfff","payloadSchema":{schema}}}]}}]}}"#
        ), usize::MAX).unwrap();
        let catalog = Catalog::new(&registry, HashMap::new()).unwrap();
        let notification = &catalog.notifications[0];
        assert_eq!(notification.namespace, "fixture".into());
        assert_eq!(notification.name.0, [0xd800]);
        assert_eq!(notification.version, "1.0.0".into());
        assert_eq!(notification.description.0, [0xdfff]);
        let expected = ivy_contracts::json::parse(schema, usize::MAX).unwrap();
        assert_eq!(notification.payload_schema.as_ref(), &expected);
        assert_eq!(ivy_contracts::hash::hash_json(notification.payload_schema.as_ref()).map_err(|e| e.to_wire()),
            ivy_contracts::hash::hash_json(&expected).map_err(|e| e.to_wire()));
    }
}

fn catalog(reserved_bytes: usize) -> Catalog {
    // A short serialized value with a large retained capacity distinguishes heap
    // accounting from a serialized-size or entry-count limit.
    let mut text = Vec::with_capacity(reserved_bytes / 2);
    text.push(b'x' as u16);
    Catalog {
        namespaces: vec![],
        tools: vec![],
        notifications: vec![NotificationDefinition {
            namespace: "fixture".into(), name: "changed".into(), version: "1.0.0".into(),
            description: "Fixture notification.".into(),
            payload_schema: Arc::new(IvyJson::String(JsString(text))),
        }],
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
    assert_eq!(recent.notifications[0].payload_schema.as_ref(), &"x".into());
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
    assert_eq!(replacement.notifications[0].payload_schema.as_ref(), &"x".into());
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
