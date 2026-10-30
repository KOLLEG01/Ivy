use ivy_agent_manager::notifications::NativeNotifications;
use ivy_contracts::{IvyJson, JsString};

fn fixture(bytes: usize) -> NativeNotifications {
    NativeNotifications::new(
        "node".into(),
        "0.159.2".into(),
        0,
        9_007_199_254_740_991,
        bytes,
    )
}
fn observe(events: &mut NativeNotifications, params: IvyJson) {
    events
        .observe(
            "epoch",
            "turn/completed",
            params,
            "2026-01-01T00:00:00.000Z",
        )
        .unwrap();
}
fn number(page: &IvyJson, field: &str) -> f64 {
    page.get(field).unwrap().as_f64().unwrap()
}
fn flag(page: &IvyJson, field: &str) -> bool {
    page.get(field) == Some(&IvyJson::Bool(true))
}
fn items(page: &IvyJson) -> usize {
    let IvyJson::Array(items) = page.get("items").unwrap() else {
        panic!()
    };
    items.len()
}

#[test]
fn empty_and_future_cursors_keep_gap_and_through_sequence_semantics() {
    let events = fixture(1024);
    let empty = events.page(0, 100, None).unwrap();
    assert_eq!(number(&empty, "firstAvailableSequence"), 1.0);
    assert_eq!(number(&empty, "throughSequence"), 0.0);
    assert!(!flag(&empty, "gap"));
    assert_eq!(empty.get("epoch"), Some(&IvyJson::Null));
    let future = events.page(10, 100, Some("epoch")).unwrap();
    assert!(flag(&future, "gap"));
    assert_eq!(number(&future, "throughSequence"), 0.0);
}

#[test]
fn paging_is_contiguous_and_retains_original_native_values() {
    let mut events = fixture(4096);
    let original = IvyJson::object([(
        JsString(vec![0xd800]),
        IvyJson::String(JsString(vec![0xdfff])),
    )]);
    for params in [original.clone(), IvyJson::Null, true.into()] {
        observe(&mut events, params);
    }
    let first = events.page(0, 2, Some("epoch")).unwrap();
    assert_eq!(items(&first), 2);
    assert!(flag(&first, "hasMore"));
    assert!(!flag(&first, "gap"));
    assert_eq!(number(&first, "throughSequence"), 2.0);
    assert_eq!(
        first.get("items").unwrap().as_array().unwrap()[0].get("params"),
        Some(&original)
    );
    let last = events.page(2, 2, Some("epoch")).unwrap();
    assert_eq!(items(&last), 1);
    assert!(!flag(&last, "hasMore"));
    assert!(!flag(&last, "gap"));
    assert_eq!(number(&last, "throughSequence"), 3.0);
}

#[test]
fn oversized_dropped_events_leave_recoverable_gaps_between_retained_events() {
    let mut events = fixture(4096);
    observe(&mut events, IvyJson::Null);
    observe(&mut events, "x".repeat(8192).into());
    observe(&mut events, false.into());
    let page = events.page(0, 100, Some("epoch")).unwrap();
    assert_eq!(items(&page), 2);
    assert!(flag(&page, "gap"));
    assert_eq!(number(&page, "throughSequence"), 3.0);
}

#[test]
fn retention_evicts_oldest_by_retained_bytes_and_count() {
    let mut probe = fixture(4096);
    let value = probe
        .observe(
            "epoch",
            "turn/completed",
            IvyJson::Null,
            "2026-01-01T00:00:00.000Z",
        )
        .unwrap();
    let mut events = fixture(
        ivy_contracts::json::stringify(&value, usize::MAX)
            .unwrap()
            .len()
            * 3
            / 2,
    );
    observe(&mut events, IvyJson::Null);
    observe(&mut events, IvyJson::Null);
    let page = events.page(0, 100, None).unwrap();
    assert_eq!(items(&page), 1);
    assert!(flag(&page, "gap"));
    assert_eq!(number(&page, "firstAvailableSequence"), 2.0);
    let mut events = fixture(32 * 1024 * 1024);
    for _ in 0..10_001 {
        observe(&mut events, IvyJson::Null);
    }
    let page = events.page(0, 1, None).unwrap();
    assert!(flag(&page, "gap"));
    assert_eq!(number(&page, "firstAvailableSequence"), 2.0);
}

#[test]
fn dense_json_fits_without_retaining_the_expanded_tree() {
    let params = IvyJson::Array(vec![IvyJson::Null; 4096]);
    let maximum = ivy_contracts::json::stringify(&params, usize::MAX)
        .unwrap()
        .len()
        * 2;
    assert!(params.retained_bytes() > maximum);
    let mut events = fixture(maximum);
    observe(&mut events, params.clone());
    let page = events.page(0, 100, None).unwrap();
    assert_eq!(items(&page), 1);
    assert_eq!(
        page.get("items").unwrap().as_array().unwrap()[0].get("params"),
        Some(&params)
    );
    assert!(!flag(&page, "gap"));
    assert_eq!(number(&page, "throughSequence"), 1.0);
}

#[test]
fn sequence_range_is_exhausted_without_reusing_an_identity() {
    let mut events = NativeNotifications::new("node".into(), "0.159.2".into(), 10, 11, 1024);
    observe(&mut events, IvyJson::Null);
    assert_eq!(
        events
            .observe("epoch", "turn/completed", IvyJson::Null, "now")
            .unwrap_err()
            .code,
        "native_notification_capacity"
    );
    let page = events.page(10, 100, None).unwrap();
    assert_eq!(number(&page, "throughSequence"), 11.0);
    assert!(!flag(&page, "gap"));
}

#[test]
fn transport_charge_includes_json_escaped_result_and_single_large_item_refuses() {
    let mut events = fixture(32 * 1024 * 1024);
    observe(&mut events, "\\".repeat(6 * 1024 * 1024).into());
    assert_eq!(
        events.page(0, 100, None).unwrap_err().code,
        "native_notification_too_large"
    );
}
