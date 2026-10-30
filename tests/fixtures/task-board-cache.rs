use super::ProjectionCache;
use ivy_contracts::IvyJson as Json;

#[test]
fn projections_evict_by_retained_bytes_before_reaching_the_entry_limit() {
    let value = Json::Array(vec![Json::Null; 1024]);
    let bytes = value.retained_bytes();
    let mut cache = ProjectionCache::new(128, bytes * 2 + 1024);
    cache.remember("first".into(), value.clone(), bytes);
    cache.remember("second".into(), value.clone(), bytes);
    assert!(cache.get("first").is_some());
    cache.remember("third".into(), value, bytes);
    assert!(cache.get("first").is_none());
    assert!(cache.get("second").is_some());
    assert!(cache.get("third").is_some());
    assert!(cache.bytes <= cache.maximum_bytes);
}

#[test]
fn replacement_oversized_and_disabled_projections_preserve_current_values() {
    let mut cache = ProjectionCache::new(1, 1024);
    cache.remember("first".into(), Json::Null, Json::Null.retained_bytes());
    cache.remember("first".into(), false.into(), Json::Null.retained_bytes());
    cache.remember("oversized".into(), Json::Null, 1025);
    assert_eq!(cache.get("first"), Some(&Json::Bool(false)));
    assert!(cache.get("oversized").is_none());
    cache.remember("second".into(), true.into(), Json::Null.retained_bytes());
    assert!(cache.get("first").is_none());
    assert_eq!(cache.get("second"), Some(&Json::Bool(true)));
    let mut disabled = ProjectionCache::new(0, 1024);
    disabled.remember("key".into(), Json::Null, 0);
    assert!(disabled.get("key").is_none());
}

#[test]
fn action_records_charge_materialized_documents_not_only_wire_payloads() {
    use crate::journal_store::{CachedRecord, remember_record};
    let value = Json::Array(vec![Json::Null; 4096]);
    assert!(
        ivy_contracts::json::stringify(&value, usize::MAX)
            .unwrap()
            .len()
            < 65536
    );
    let mut entries = Vec::new();
    remember_record(
        &mut entries,
        "pin".into(),
        CachedRecord {
            object_id: "id".into(),
            document: None,
            materialized: Some(value),
            retained_bytes: 0,
        },
    );
    assert!(entries.is_empty());
    remember_record(
        &mut entries,
        "pin".into(),
        CachedRecord {
            object_id: "id".into(),
            document: None,
            materialized: Some(Json::Null),
            retained_bytes: 0,
        },
    );
    assert_eq!(entries.len(), 1);
    for index in 0..256 {
        remember_record(
            &mut entries,
            format!("pin-{index}").into(),
            CachedRecord {
                object_id: "id".into(),
                document: None,
                materialized: Some(Json::Array(vec![Json::Null; 1024])),
                retained_bytes: 0,
            },
        );
    }
    assert!(entries.len() < 256);
    assert!(
        entries
            .iter()
            .map(|(_, record)| record.retained_bytes)
            .sum::<usize>()
            <= 4 * 1024 * 1024
    );
}
