use ivy_agent_manager::inventory::NativeInventory;
use ivy_contracts::{IvyJson, JsString};
use std::{
    alloc::{GlobalAlloc, Layout, System},
    sync::{
        Arc, Mutex,
        atomic::{AtomicIsize, Ordering},
    },
};

struct Counted;
static LIVE_BYTES: AtomicIsize = AtomicIsize::new(0);
unsafe impl GlobalAlloc for Counted {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            LIVE_BYTES.fetch_add(layout.size() as isize, Ordering::Relaxed);
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
