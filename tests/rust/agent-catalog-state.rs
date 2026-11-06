use ivy_agent_manager::inventory::NativeInventory;
use ivy_agent_manager::{
    notice_policy::NativeNoticePolicy,
    project_locations::ProjectLocations,
    project_model::{NativeProjects, ProjectSnapshot},
    project_policy::ProjectPolicy,
    project_repository::WorkspaceGit,
};
use ivy_contracts::{IvyJson, JsString};
use ivy_host_runtime::node_path;
use std::{
    alloc::{GlobalAlloc, Layout, System},
    sync::{
        Arc, Mutex,
        atomic::{AtomicIsize, AtomicUsize, Ordering},
    },
};

struct Counted;
static LIVE_BYTES: AtomicIsize = AtomicIsize::new(0);
static LARGEST_ALLOCATION: AtomicUsize = AtomicUsize::new(0);
unsafe impl GlobalAlloc for Counted {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            LIVE_BYTES.fetch_add(layout.size() as isize, Ordering::Relaxed);
            LARGEST_ALLOCATION.fetch_max(layout.size(), Ordering::Relaxed);
        }
        pointer
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) };
        LIVE_BYTES.fetch_sub(layout.size() as isize, Ordering::Relaxed);
    }
    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        let next = unsafe { System.realloc(pointer, layout, size) };
        if !next.is_null() {
            LIVE_BYTES.fetch_add(size as isize - layout.size() as isize, Ordering::Relaxed);
            LARGEST_ALLOCATION.fetch_max(size, Ordering::Relaxed);
        }
        next
    }
}
#[global_allocator]
static ALLOCATOR: Counted = Counted;

fn catalog(projects: bool) -> IvyJson {
    let mut requests = vec![IvyJson::object([
        ("method", "thread/list".into()),
        (
            "inputSchema",
            IvyJson::object([
                (
                    "properties",
                    IvyJson::object([(
                        "sourceKinds",
                        IvyJson::object([(
                            "items",
                            IvyJson::object([(
                                "enum",
                                IvyJson::Array(vec![
                                    "cli".into(),
                                    IvyJson::String(JsString(vec![0xd800])),
                                ]),
                            )]),
                        )]),
                    )]),
                ),
                (
                    "$defs",
                    IvyJson::object([(
                        "large",
                        IvyJson::object([(
                            "enum",
                            IvyJson::Array(vec![IvyJson::Number(0.0); 256 * 1024]),
                        )]),
                    )]),
                ),
            ]),
        ),
    ])];
    if projects {
        requests.push(IvyJson::object([("method", "project/list".into())]));
    }
    IvyJson::object([("clientRequests", IvyJson::Array(requests))])
}

#[test]
fn inventory_retains_only_small_projections_and_keeps_lossless_sources_and_project_support() {
    if std::env::var_os("IVY_TEST_CATALOG_MEMORY").is_none() {
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "inventory_retains_only_small_projections_and_keeps_lossless_sources_and_project_support", "--test-threads=1"])
            .env("IVY_TEST_CATALOG_MEMORY", "1")
            .output().unwrap();
        assert!(
            result.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
        return;
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let read = Arc::new({
        let calls = calls.clone();
        move |method: JsString, params: IvyJson, _| {
            calls.lock().unwrap().push((method, params));
            Box::pin(async {
                Ok(IvyJson::object([(
                    "result",
                    IvyJson::object([
                        ("data", IvyJson::Array(vec![])),
                        ("nextCursor", IvyJson::Null),
                    ]),
                )]))
            }) as ivy_sdk::service::ServiceFuture<IvyJson>
        }
    });
    let baseline = LIVE_BYTES.load(Ordering::Relaxed);
    let source = catalog(true);
    let before_clone = LIVE_BYTES.load(Ordering::Relaxed);
    let redundant = source.clone();
    let clone_bytes = LIVE_BYTES.load(Ordering::Relaxed) - before_clone;
    assert!(
        clone_bytes >= 8 * 1024 * 1024,
        "Large catalog clone: {clone_bytes}"
    );
    drop(redundant);
    let inventory = NativeInventory::with_reader(
        &source,
        "epoch".into(),
        read.clone(),
        Arc::new(|| 0.0),
        Arc::new(|| "2026-01-01T00:00:00.000Z".into()),
    )
    .unwrap();
    drop(source);
    let retained = LIVE_BYTES.load(Ordering::Relaxed) - baseline;
    assert!(
        retained < 64 * 1024,
        "Inventory retained {retained} bytes after dropping the catalog"
    );
    runtime.block_on(async {
        inventory.collect_threads(None).await.unwrap();
        inventory.collect_projects(None).await.unwrap();
    });
    let recorded = calls.lock().unwrap();
    let thread_pages: Vec<_> = recorded
        .iter()
        .filter(|(method, _)| method == &JsString::from("thread/list"))
        .collect();
    assert_eq!(thread_pages.len(), 2);
    let expected = IvyJson::Array(vec!["cli".into(), IvyJson::String(JsString(vec![0xd800]))]);
    for (_, params) in thread_pages {
        assert_eq!(params.get("sourceKinds"), Some(&expected));
    }
    assert_eq!(
        recorded
            .iter()
            .filter(|(method, _)| method == &JsString::from("project/list"))
            .count(),
        1
    );
    let count_before = recorded.len();
    drop(recorded);
    let unsupported = NativeInventory::with_reader(
        &catalog(false),
        "epoch".into(),
        read,
        Arc::new(|| 0.0),
        Arc::new(|| "2026-01-01T00:00:00.000Z".into()),
    )
    .unwrap();
    assert_eq!(
        runtime
            .block_on(unsupported.collect_projects(None))
            .unwrap_err()
            .code,
        "native_inventory_unavailable"
    );
    assert_eq!(calls.lock().unwrap().len(), count_before);
}

#[test]
fn project_consumers_share_payloads_without_repeating_the_json_heap() {
    if std::env::var_os("IVY_TEST_PROJECT_MODEL_MEMORY").is_none() {
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "project_consumers_share_payloads_without_repeating_the_json_heap",
                "--test-threads=1",
            ])
            .env("IVY_TEST_PROJECT_MODEL_MEMORY", "1")
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
        print!("{}", String::from_utf8_lossy(&result.stdout));
        return;
    }
    let values: Vec<_> = (0..64)
        .map(|i| {
            IvyJson::object([
                ("nativeId", format!("p{i}").into()),
                ("source", "native".into()),
                ("name", "Project".into()),
                (
                    "paths",
                    IvyJson::Array(vec![IvyJson::from("/".to_owned() + &"a".repeat(1023)); 16]),
                ),
                ("position", IvyJson::Number(i as f64)),
                ("recencyAt", IvyJson::Null),
            ])
        })
        .collect();
    let before_copies = LIVE_BYTES.load(Ordering::Relaxed);
    let copies = [values.clone(), values.clone(), values.clone()];
    let copied_bytes = LIVE_BYTES.load(Ordering::Relaxed) - before_copies;
    std::hint::black_box(&copies);
    assert!(
        copied_bytes > 6 * 1024 * 1024,
        "Copied payload bytes: {copied_bytes}"
    );
    drop(copies);
    let model = NativeProjects::from_summaries(&values).unwrap();
    let root = std::env::temp_dir();
    let locations = ProjectLocations::new(
        ProjectPolicy::new(
            node_path::from_native(&root),
            &IvyJson::object([] as [(&str, IvyJson); 0]),
            &[],
            node_path::Context::current().unwrap(),
            &node_path::from_native(&root),
        )
        .unwrap(),
        None,
        None,
        Arc::new(WorkspaceGit::new(root, None)),
    );
    let mut policy = NativeNoticePolicy::default();
    let before_share = LIVE_BYTES.load(Ordering::Relaxed);
    policy.set_projects(model.clone());
    locations.set_native_projects(model.clone()).unwrap();
    let snapshot = ProjectSnapshot {
        observed_at: "2026-01-01T00:00:00.000Z".into(),
        projects: model.clone(),
    };
    let shared_bytes = LIVE_BYTES.load(Ordering::Relaxed) - before_share;
    std::hint::black_box((&policy, &locations, &snapshot));
    assert!(
        shared_bytes.abs() < 4096,
        "Additional shared metadata bytes: {shared_bytes}"
    );
    assert_eq!(Arc::strong_count(&model), 4);
    println!(
        "project-model-allocation: three_json_copies={copied_bytes}, three_shared_consumers={shared_bytes}, model_retained={}",
        model.retained_bytes()
    );
}

#[test]
fn notice_membership_budget_covers_allocations_and_rejects_large_borrowed_paths_before_clone() {
    if std::env::var_os("IVY_TEST_NOTICE_POLICY_MEMORY").is_none() {
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "notice_membership_budget_covers_allocations_and_rejects_large_borrowed_paths_before_clone",
                "--test-threads=1",
            ])
            .env("IVY_TEST_NOTICE_POLICY_MEMORY", "1")
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
        print!("{}", String::from_utf8_lossy(&result.stdout));
        return;
    }
    let large = IvyJson::object([("cwd", "p".repeat(32 * 1024).into())]);
    let oversized = IvyJson::object([("cwd", "p".repeat(4 * 1024 * 1024).into())]);
    let mut policy = NativeNoticePolicy::default();
    let baseline = LIVE_BYTES.load(Ordering::Relaxed);
    for i in 0..5000 {
        policy.set_thread(format!("t{i}").into(), &large, None);
    }
    let actual = LIVE_BYTES.load(Ordering::Relaxed) - baseline;
    let charged = policy.retained_bytes();
    assert!(actual > 0 && actual as usize <= charged);
    assert!(charged <= 2 * 1024 * 1024);
    LARGEST_ALLOCATION.store(0, Ordering::Relaxed);
    policy.set_thread("large".into(), &oversized, None);
    let largest = LARGEST_ALLOCATION.load(Ordering::Relaxed);
    assert!(
        largest < 256 * 1024,
        "Oversized borrowed path was copied: {largest}"
    );
    assert_eq!(policy.is_internal(&"large".into()).unwrap(), None);
    let push = IvyJson::object([
        ("threadId", "large".into()),
        ("projectId", oversized.required("cwd").unwrap().clone()),
    ]);
    let method = "thread/project/updated".into();
    LARGEST_ALLOCATION.store(0, Ordering::Relaxed);
    policy.observe(&method, &push);
    let project_largest = LARGEST_ALLOCATION.load(Ordering::Relaxed);
    assert!(
        project_largest < 256 * 1024,
        "Oversized project ID was copied: {project_largest}"
    );
    drop((push, method));
    assert_eq!(policy.is_internal(&"large".into()).unwrap(), None);
    policy.set_threads(&[], policy.revision()).unwrap();
    let after_clear = LIVE_BYTES.load(Ordering::Relaxed) - baseline;
    assert_eq!(after_clear, 0, "Empty cache retains allocations");
    println!(
        "notice-policy-allocation: actual={actual}, charged={charged}, oversized_max_allocation={largest}, oversized_project_max_allocation={project_largest}, after_clear={after_clear}"
    );
}
