use ivy_agent_manager::notifications::NativeNotifications;
use ivy_contracts::{IvyJson, JsString};
use std::{
    alloc::{GlobalAlloc, Layout, System},
    sync::atomic::{AtomicIsize, Ordering},
};

struct Counted;
static LIVE: AtomicIsize = AtomicIsize::new(0);
static PEAK: AtomicIsize = AtomicIsize::new(0);
fn added(bytes: isize) {
    let live = LIVE.fetch_add(bytes, Ordering::Relaxed) + bytes;
    PEAK.fetch_max(live, Ordering::Relaxed);
}
unsafe impl GlobalAlloc for Counted {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            added(layout.size() as isize);
        }
        pointer
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc_zeroed(layout) };
        if !pointer.is_null() {
            added(layout.size() as isize);
        }
        pointer
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) };
        LIVE.fetch_sub(layout.size() as isize, Ordering::Relaxed);
    }
    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        let next = unsafe { System.realloc(pointer, layout, size) };
        if !next.is_null() {
            added(size as isize - layout.size() as isize);
        }
        next
    }
}
#[global_allocator]
static ALLOCATOR: Counted = Counted;

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

#[test]
fn transport_boundary_matches_the_original_utf16_serializer() {
    use ivy_contracts::{json::stringify, limits::MANAGEMENT_FRAME_BYTES};
    let charge = |value: &IvyJson| {
        let encoded = stringify(value, usize::MAX).unwrap();
        encoded.len()
            + stringify(&IvyJson::from(encoded.as_str()), usize::MAX)
                .unwrap()
                .len()
    };
    let value = |params: IvyJson| {
        fixture(64 * 1024 * 1024)
            .observe("epoch", "turn/completed", params, "now")
            .unwrap()
    };
    let base = charge(&value("".into()));
    let maximum = MANAGEMENT_FRAME_BYTES - 1024 * 1024;
    for units in [
        vec![b'\\' as u16],
        vec![
            0,
            8,
            12,
            9,
            10,
            13,
            b'"' as u16,
            b'\\' as u16,
            0xd800,
            b'x' as u16,
            0xdc00,
            0xd83d,
            0xde80,
            0x2028,
            0x2029,
            0xe9,
        ],
    ] {
        let growth = charge(&value(IvyJson::String(JsString(units.clone())))) - base;
        let repeats = (maximum - base) / growth;
        for count in [repeats, repeats + 1] {
            let mut events = fixture(64 * 1024 * 1024);
            let event = events
                .observe(
                    "epoch",
                    "turn/completed",
                    IvyJson::String(JsString(units.repeat(count))),
                    "now",
                )
                .unwrap();
            let expected = charge(&event);
            assert_eq!(expected <= maximum, count == repeats);
            match events.page(0, 100, None) {
                Ok(page) => {
                    assert!(expected <= maximum);
                    assert_eq!(items(&page), 1);
                }
                Err(error) => {
                    assert!(expected > maximum);
                    assert_eq!(error.code, "native_notification_too_large");
                }
            }
        }
    }
}

#[test]
fn native_event_processing_does_not_duplicate_large_payloads() {
    const CHILD: &str = "IVY_AGENT_EVENT_MEMORY_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "native_event_processing_does_not_duplicate_large_payloads",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .output()
            .unwrap();
        eprint!("{}", String::from_utf8_lossy(&output.stderr));
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed;"));
        return;
    }
    use ivy_agent_manager::{
        local_state::{Limits as StateLimits, LocalAgentState},
        native_journal::{Limits, NativeJournal, Owner},
        native_operations::{NativeOperations, SqliteOperationStore},
        notice_policy::NativeNoticePolicy,
        service_events::{ServiceEventSettings, ServiceEvents},
        service_link::ServiceLink,
    };
    use ivy_host_runtime::codex_rpc::NativeNotification;
    use std::sync::{Arc, Mutex, atomic::AtomicBool};
    let root = std::env::temp_dir().join(format!("ivy-event-memory-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let state = Arc::new(Mutex::new(
            LocalAgentState::open(
                &root.join("state.sqlite"),
                "fixture.agent".to_owned(),
                StateLimits {
                    max_operations: 16,
                    max_journal_bytes: 64 * 1024 * 1024,
                },
            )
            .unwrap(),
        ));
        let journal = Arc::new(Mutex::new(
            NativeJournal::new(
                Owner {
                    service_node_id: "fixture.agent".into(),
                    native_version: "0.159.2".into(),
                    native_executable_hash: format!("sha256:{}", "0".repeat(64)).into(),
                },
                Limits {
                    max_operations: 16,
                    max_journal_bytes: 64 * 1024 * 1024,
                    max_pending_inputs: 8,
                },
            )
            .unwrap(),
        ));
        let notifications = Arc::new(Mutex::new(fixture(8 * 1024 * 1024)));
        let policy = Arc::new(Mutex::new(NativeNoticePolicy::default()));
        let events = ServiceEvents::with_clock(
            ServiceEventSettings {
                service_node_id: "fixture.agent".into(),
                service: ServiceLink::default(),
                closing: Arc::new(AtomicBool::new(false)),
                operations: NativeOperations::new(
                    Arc::new(SqliteOperationStore::new(state.clone()).unwrap()),
                    journal,
                ),
                notifications: notifications.clone(),
                policy: policy.clone(),
                query: Arc::new(|_| panic!("An offline observation cannot send a notice")),
            },
            Arc::new(|| "2026-01-01T00:00:00.000Z".into()),
        );
        let callbacks = events.begin_epoch("epoch".into()).unwrap();
        (callbacks.on_notification)(NativeNotification {
            method: "item/started".into(),
            params: IvyJson::Null,
        })
        .await
        .unwrap();
        let input = NativeNotification {
            method: "thread/started".into(),
            params: IvyJson::object([(
                "thread",
                IvyJson::object([
                    ("id", "thread".into()),
                    ("cwd", "/external".into()),
                    ("padding", "x".repeat(1024 * 1024).into()),
                ]),
            )]),
        };
        let before = LIVE.load(Ordering::Relaxed);
        PEAK.store(before, Ordering::Relaxed);
        (callbacks.on_notification)(input).await.unwrap();
        let peak = PEAK.load(Ordering::Relaxed) - before;
        eprintln!(
            "Agent native event Rust allocation peak: {peak} bytes above owned 1 MiB payload"
        );
        assert_eq!(
            policy
                .lock()
                .unwrap()
                .is_internal(&"thread".into())
                .unwrap(),
            Some(false)
        );
        let page = notifications
            .lock()
            .unwrap()
            .page(0, 100, Some("epoch"))
            .unwrap();
        assert_eq!(items(&page), 2);
        assert!(!flag(&page, "gap"));
        assert_eq!(
            page.get("items").unwrap().as_array().unwrap()[1]
                .get("params")
                .unwrap()
                .get("thread")
                .unwrap()
                .get("padding")
                .unwrap()
                .as_js_string()
                .unwrap()
                .0
                .len(),
            1024 * 1024
        );
        state.lock().unwrap().close_shared().unwrap();
        assert!(
            peak < 4 * 1024 * 1024,
            "Native event copied or re-encoded its complete payload: {peak}"
        );
    });
    drop(runtime);
    std::fs::remove_dir_all(&root).unwrap();
}

#[tokio::test]
async fn native_resolution_keeps_original_ids_and_follows_successful_observation() {
    use ivy_agent_manager::{
        local_state::{Limits as StateLimits, LocalAgentState},
        native_dispatch::{Observations, journal_events},
        native_journal::{Limits, NativeJournal, Owner},
        native_operations::{NativeOperations, SqliteOperationStore},
    };
    use ivy_contracts::IvyError;
    use ivy_host_runtime::codex_rpc::NativeNotification;
    use std::sync::{Arc, Mutex, atomic::AtomicBool};
    let state = Arc::new(Mutex::new(
        LocalAgentState::open(
            std::path::Path::new(":memory:"),
            "fixture.agent".to_owned(),
            StateLimits {
                max_operations: 16,
                max_journal_bytes: 128 * 1024 * 1024,
            },
        )
        .unwrap(),
    ));
    let journal = Arc::new(Mutex::new(
        NativeJournal::new(
            Owner {
                service_node_id: "fixture.agent".into(),
                native_version: "0.159.2".into(),
                native_executable_hash: format!("sha256:{}", "0".repeat(64)).into(),
            },
            Limits {
                max_operations: 16,
                max_journal_bytes: 128 * 1024 * 1024,
                max_pending_inputs: 8,
            },
        )
        .unwrap(),
    ));
    journal
        .lock()
        .unwrap()
        .begin_epoch(&"epoch".into())
        .unwrap();
    let operations = NativeOperations::new(
        Arc::new(SqliteOperationStore::new(state.clone()).unwrap()),
        journal.clone(),
    );
    let identities = [IvyJson::Number(-2.0), "-2".into()].map(|request| {
        IvyJson::object([
            ("serviceNodeId", "fixture.agent".into()),
            ("epoch", "epoch".into()),
            ("requestId", request),
        ])
    });
    for identity in &identities {
        journal
            .lock()
            .unwrap()
            .observe_input(
                identity,
                &"currentTime/read".into(),
                &IvyJson::object([("threadId", "thread".into()), ("turnId", "turn".into())]),
            )
            .unwrap();
    }
    let observed = Arc::new(AtomicBool::new(false));
    let callbacks = journal_events(
        "fixture.agent".into(),
        "epoch".into(),
        &operations,
        Observations {
            on_notification: Arc::new({
                let journal = journal.clone();
                let identity = identities[0].clone();
                let observed = observed.clone();
                move |mut notification| {
                    assert_eq!(
                        journal
                            .lock()
                            .unwrap()
                            .input(&identity)
                            .unwrap()
                            .unwrap()
                            .get("state"),
                        Some(&"pending".into())
                    );
                    notification.params.insert("requestId", "-2".into());
                    notification.params.insert("threadId", "other".into());
                    observed.store(true, Ordering::Relaxed);
                    Box::pin(async { Ok(()) })
                }
            }),
            ..Observations::default()
        },
    );
    (callbacks.on_notification)(NativeNotification {
        method: "serverRequest/resolved".into(),
        params: IvyJson::object([
            ("requestId", IvyJson::Number(-2.0)),
            ("threadId", "thread".into()),
        ]),
    })
    .await
    .unwrap();
    assert!(observed.load(Ordering::Relaxed));
    assert_eq!(
        journal
            .lock()
            .unwrap()
            .input(&identities[0])
            .unwrap()
            .unwrap()
            .get("state"),
        Some(&"expired".into())
    );
    assert_eq!(
        journal
            .lock()
            .unwrap()
            .input(&identities[1])
            .unwrap()
            .unwrap()
            .get("state"),
        Some(&"pending".into())
    );
    let rejected = journal_events(
        "fixture.agent".into(),
        "epoch".into(),
        &operations,
        Observations {
            on_notification: Arc::new(|_| {
                Box::pin(async { Err(IvyError::new("fixture_failed", "Observation failed")) })
            }),
            ..Observations::default()
        },
    );
    assert_eq!(
        (rejected.on_notification)(NativeNotification {
            method: "serverRequest/resolved".into(),
            params: IvyJson::object([("requestId", "-2".into()), ("threadId", "thread".into())])
        })
        .await
        .unwrap_err()
        .code,
        "fixture_failed"
    );
    assert_eq!(
        journal
            .lock()
            .unwrap()
            .input(&identities[1])
            .unwrap()
            .unwrap()
            .get("state"),
        Some(&"pending".into())
    );
    observed.store(false, Ordering::Relaxed);
    let null = journal_events(
        "fixture.agent".into(),
        "epoch".into(),
        &operations,
        Observations {
            on_notification: Arc::new({
                let observed = observed.clone();
                move |_| {
                    observed.store(true, Ordering::Relaxed);
                    Box::pin(async { Ok(()) })
                }
            }),
            ..Observations::default()
        },
    );
    assert_eq!(
        (null.on_notification)(NativeNotification {
            method: "serverRequest/resolved".into(),
            params: IvyJson::Null
        })
        .await
        .unwrap_err()
        .code,
        "native_observation_commit_failed"
    );
    assert!(observed.load(Ordering::Relaxed));
    state.lock().unwrap().close_shared().unwrap();
}
