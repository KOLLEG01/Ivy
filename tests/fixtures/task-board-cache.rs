use super::ProjectionCache;
use ivy_contracts::IvyJson as Json;

#[test]
fn independent_action_caches_share_bytes_and_keep_spare_storage_charged() {
    use crate::{
        journal_store::{CacheRecords, CachedValue, remember_record},
        operations::{ObjectPin, StoredRecord},
        work_budget::{MAX_RETAINED_BYTES, WorkBudget},
    };
    let budget = WorkBudget::default();
    let record = StoredRecord {
        key: "task-board/task".into(),
        pin: ObjectPin {
            object_id: "immutable".into(),
            revision: 1.0,
        },
        value: Json::Array(vec![Json::Null; 512]),
        metadata: Json::Null,
        revision: Json::Null,
    };
    let mut contexts = Vec::new();
    for scope in 0..20 {
        let mut entries = CacheRecords::new(budget.clone());
        for index in 0..256 {
            let _ = remember_record(
                &mut entries,
                format!("{scope}-{index}").into(),
                record.retained_bytes(),
                || CachedValue::Materialized(record.clone()),
            );
        }
        assert!(entries.iter().map(|(_, r)| r.retained_bytes).sum::<usize>() <= 4 * 1024 * 1024);
        contexts.push(entries);
    }
    assert!(contexts[0].len() > 1);
    assert!(
        contexts.iter().any(|entries| entries.is_empty()),
        "The common budget stops independent caches before their local limits."
    );
    let held = budget.retained_usage();
    assert_eq!(held.0, 0);
    assert!(held.1 > MAX_RETAINED_BYTES / 2 && held.1 <= MAX_RETAINED_BYTES);
    contexts[0].clear();
    let empty_buffer_bytes = contexts[0].values.capacity()
        * std::mem::size_of::<(Json, crate::journal_store::CachedRecord)>();
    assert!(empty_buffer_bytes > 0);
    let before_drop = budget.retained_usage().1;
    drop(contexts.remove(0));
    assert_eq!(budget.retained_usage().1, before_drop - empty_buffer_bytes);
    let mut replacement = CacheRecords::new(budget.clone());
    let _ = remember_record(
        &mut replacement,
        "new".into(),
        record.retained_bytes(),
        || CachedValue::Materialized(record.clone()),
    );
    assert_eq!(replacement.len(), 1);
    drop(contexts);
    drop(replacement);
    assert_eq!(budget.retained_usage(), (0, 0));
}

#[test]
fn work_pressure_refuses_cache_copies_and_preserves_the_existing_immutable_value() {
    use crate::{
        journal_store::{CacheRecords, CachedValue, remember_record},
        operations::{ObjectPin, StoredRecord},
        work_budget::{MAX_RETAINED_BYTES, WorkBudget},
    };
    let budget = WorkBudget::default();
    let mut entries = CacheRecords::new(budget.clone());
    let record = StoredRecord {
        key: "task-board/task".into(),
        pin: ObjectPin {
            object_id: "original".into(),
            revision: 1.0,
        },
        value: Json::Bool(true),
        metadata: Json::Null,
        revision: Json::Null,
    };
    let _ = remember_record(&mut entries, "pin".into(), record.retained_bytes(), || {
        CachedValue::Materialized(record.clone())
    });
    let work = budget
        .call(MAX_RETAINED_BYTES - budget.retained_usage().1)
        .unwrap();
    let _ = remember_record(&mut entries, "pin".into(), record.retained_bytes(), || {
        panic!("No replacement may be copied under shared work pressure.")
    });
    assert!(
        matches!(&entries[0].1.value, CachedValue::Materialized(r) if r.value == Json::Bool(true))
    );
    drop(work);
    let _ = remember_record(&mut entries, "next".into(), record.retained_bytes(), || {
        CachedValue::Materialized(record.clone())
    });
    assert_eq!(entries.len(), 2);
    drop(entries);
    assert_eq!(budget.retained_usage(), (0, 0));
}

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
    use crate::journal_store::{CacheRecords, CachedValue, remember_record};
    use crate::operations::{ObjectPin, StoredRecord};
    let record = |value| StoredRecord {
        key: "task-board/task".into(),
        pin: ObjectPin {
            object_id: "id".into(),
            revision: 1.0,
        },
        value,
        metadata: Json::Null,
        revision: Json::Null,
    };
    let value = Json::Array(vec![Json::Null; 4096]);
    assert!(
        ivy_contracts::json::stringify(&value, usize::MAX)
            .unwrap()
            .len()
            < 65536
    );
    let mut entries = CacheRecords::default();
    let expanded = record(value);
    let _ = remember_record(
        &mut entries,
        "pin".into(),
        expanded.retained_bytes(),
        || panic!("oversized records must be rejected before copying"),
    );
    assert!(entries.is_empty());
    let small = record(Json::Null);
    let _ = remember_record(&mut entries, "pin".into(), small.retained_bytes(), || {
        CachedValue::Materialized(small.clone())
    });
    assert_eq!(entries.len(), 1);
    let page = record(Json::Array(vec![Json::Null; 1024]));
    for index in 0..256 {
        let _ = remember_record(
            &mut entries,
            format!("pin-{index}").into(),
            page.retained_bytes(),
            || CachedValue::Materialized(page.clone()),
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
