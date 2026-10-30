use super::*;
use crate::native_runtime::NativeRuntime;
use std::{
    alloc::{GlobalAlloc, Layout, System},
    path::Path,
    sync::atomic::{AtomicIsize, Ordering as AtomicOrdering},
};

struct Counted;
pub(crate) static LIVE: AtomicIsize = AtomicIsize::new(0);
pub(crate) static PEAK: AtomicIsize = AtomicIsize::new(0);
fn added(bytes: isize) {
    let live = LIVE.fetch_add(bytes, AtomicOrdering::Relaxed) + bytes;
    PEAK.fetch_max(live, AtomicOrdering::Relaxed);
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
        LIVE.fetch_sub(layout.size() as isize, AtomicOrdering::Relaxed);
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

fn fixture() -> (Store, Vec<Contract>) {
    let store = Store::open(Path::new(":memory:")).unwrap();
    store
        .connection()
        .execute("INSERT INTO principals VALUES('fixture')", [])
        .unwrap();
    let contracts = [
        ("fixture/a", "expire"),
        ("fixture/b", "expire"),
        ("fixture/owned", "owned"),
        ("fixture/retain", "retain"),
    ]
    .into_iter()
    .map(|(key, mode)| {
        let policy = IvyJson::from_serde(&serde_json::json!({
            "objects": {"mode": mode, "maximumAgeDays": 1},
            "revisions": {"mode": "all"}
        }))
        .unwrap();
        store
            .connection()
            .execute(
                "INSERT INTO contract_families VALUES(?,'fixture','text/plain',?)",
                params![key, if mode == "owned" { "owned" } else { "retain" }],
            )
            .unwrap();
        store
            .connection()
            .execute(
                "INSERT INTO contracts VALUES(?,'1.0.0',?)",
                params![key, canonical(&policy, MANAGEMENT_FRAME_BYTES).unwrap()],
            )
            .unwrap();
        Contract {
            key: key.into(),
            version: "1.0.0".into(),
            policy,
        }
    })
    .collect();
    (store, contracts)
}

fn insert(store: &Store, id: &str, key: &str, parent: Option<&str>, owner: Option<&str>) {
    store.connection().execute(
        "INSERT INTO objects VALUES(NULL,?,?,?,?,'/'||?,0,NULL,1,?,1,NULL,0,'2026-01-01','2026-01-02')",
        params![id, parent, owner, id, id, key],
    ).unwrap();
    store.connection().execute(
        "INSERT INTO revisions VALUES(?,1,?,'1.0.0','text','x','sha256:fixture',1,'2026-01-02','{}')",
        params![id, key],
    ).unwrap();
}
fn reference(store: &Store, source: &str, name: &str, target: &str) {
    store
        .connection()
        .execute(
            "INSERT INTO revision_references VALUES(?,1,?,?,1)",
            params![source, name, target],
        )
        .unwrap();
}

#[test]
fn graph_keeps_external_requirements_and_rechecks_family_filters() {
    let (mut store, contracts) = fixture();
    store
        .transaction(|store| {
            insert(store, "retained-owner", "fixture/retain", None, None);
            insert(store, "parent", "fixture/a", None, None);
            insert(
                store,
                "eligible-owned",
                "fixture/owned",
                None,
                Some("retained-owner"),
            );
            insert(
                store,
                "eligible-child",
                "fixture/a",
                Some("retained-owner"),
                Some("retained-owner"),
            );
            insert(
                store,
                "retained-child",
                "fixture/retain",
                Some("parent"),
                None,
            );
            insert(store, "keeper", "fixture/retain", None, None);
            for id in [
                "free-cycle-a",
                "free-cycle-b",
                "held-cycle-a",
                "held-cycle-b",
                "self",
                "target",
                "cross-a",
                "outbound-only",
            ] {
                insert(store, id, "fixture/a", None, None);
            }
            insert(store, "cross-b", "fixture/b", None, None);
            for (source, target) in [
                ("free-cycle-a", "free-cycle-b"),
                ("free-cycle-b", "free-cycle-a"),
                ("held-cycle-a", "held-cycle-b"),
                ("held-cycle-b", "held-cycle-a"),
                ("self", "self"),
                ("keeper", "target"),
                ("cross-a", "cross-b"),
                ("cross-b", "cross-a"),
            ] {
                reference(store, source, "edge", target);
            }
            reference(store, "free-cycle-a", "duplicate", "free-cycle-b");
            Ok(())
        })
        .unwrap();
    // Existing legacy edges can name absent endpoints; new public writes still use FKs.
    store
        .connection()
        .execute_batch("PRAGMA foreign_keys=OFF")
        .unwrap();
    reference(&store, "missing-source", "legacy", "held-cycle-a");
    reference(&store, "outbound-only", "legacy", "missing-target");
    store
        .connection()
        .execute_batch("PRAGMA foreign_keys=ON")
        .unwrap();
    let plan = CandidatePlan::new(&store, &contracts, &NativeRuntime, 1_800_000_000_000.0).unwrap();
    let names = |enabled| {
        plan.candidates(enabled)
            .into_iter()
            .map(|(id, _)| id)
            .collect::<Vec<_>>()
    };
    let all = vec![
        "cross-a",
        "cross-b",
        "eligible-child",
        "eligible-owned",
        "free-cycle-a",
        "free-cycle-b",
        "outbound-only",
        "self",
    ];
    assert_eq!(names(None), all);
    let only_a = HashSet::from([JsString::from("fixture/a")]);
    let disabled = HashSet::new();
    assert_eq!(
        names(Some(&only_a)),
        vec![
            "eligible-child",
            "free-cycle-a",
            "free-cycle-b",
            "outbound-only",
            "self"
        ]
    );
    assert!(names(Some(&disabled)).is_empty());
    assert_eq!(names(None), all);
}

#[test]
fn graph_has_a_bounded_allocation_peak_for_shared_identifiers() {
    const CHILD: &str = "IVY_RETENTION_GRAPH_MEMORY_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "retention::graph_tests::graph_has_a_bounded_allocation_peak_for_shared_identifiers", "--nocapture"])
            .env(CHILD, "1")
            .output()
            .unwrap();
        if output.status.success() {
            eprint!("{}", String::from_utf8_lossy(&output.stderr));
        }
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    const COUNT: usize = 2048;
    const FANOUT: usize = 16;
    let (mut store, contracts) = fixture();
    let id = |index| format!("00000000-0000-4000-8000-{index:012}");
    store
        .transaction(|store| {
            for index in 0..COUNT {
                insert(store, &id(index), "fixture/a", None, None);
            }
            for source in 0..COUNT {
                for edge in 1..=FANOUT {
                    reference(
                        store,
                        &id(source),
                        &edge.to_string(),
                        &id((source + edge) % COUNT),
                    );
                }
            }
            Ok(())
        })
        .unwrap();
    let before = LIVE.load(AtomicOrdering::Relaxed);
    PEAK.store(before, AtomicOrdering::Relaxed);
    let plan = CandidatePlan::new(&store, &contracts, &NativeRuntime, 1_800_000_000_000.0).unwrap();
    let candidates = plan.candidates(None);
    let extra = PEAK.load(AtomicOrdering::Relaxed) - before;
    eprintln!(
        "Hive retention graph Rust allocation peak above baseline: {extra} bytes for {COUNT} identifiers and {} edges",
        COUNT * FANOUT
    );
    assert_eq!(candidates.len(), COUNT);
    assert_eq!(candidates[0].0, id(0));
    // Actual Rust allocation requests, excluding SQLite's native heap and RSS.
    assert!(
        extra < 2 * 1024 * 1024,
        "Retention graph duplicated identifiers: {extra} bytes"
    );

    let (mut retained, retained_contracts) = fixture();
    retained
        .transaction(|store| {
            for index in 0..8192 {
                insert(store, &id(index), "fixture/retain", None, None);
            }
            Ok(())
        })
        .unwrap();
    let before = LIVE.load(AtomicOrdering::Relaxed);
    PEAK.store(before, AtomicOrdering::Relaxed);
    let plan = CandidatePlan::new(
        &retained,
        &retained_contracts,
        &NativeRuntime,
        1_800_000_000_000.0,
    )
    .unwrap();
    assert!(plan.candidates(None).is_empty());
    let extra = PEAK.load(AtomicOrdering::Relaxed) - before;
    eprintln!(
        "Hive retention graph Rust allocation peak above baseline: {extra} bytes for 8192 isolated retained objects"
    );
    assert!(
        extra < 128 * 1024,
        "Isolated retained objects entered the graph: {extra} bytes"
    );
}
