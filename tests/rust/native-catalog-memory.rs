use ivy_contracts::{IvyJson, agent_schema::validate_agent, json::parse};
use ivy_host_runtime::codex_rpc::{Events, NativeRpc};
use std::{
    alloc::{GlobalAlloc, Layout, System},
    sync::{
        Arc,
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

#[test]
fn compiled_properties_retain_one_copy_of_declared_names() {
    let name = "n".repeat(96 * 1024);
    let schema = IvyJson::object([
        (
            "properties",
            IvyJson::object([(name.clone(), IvyJson::Bool(true))]),
        ),
        ("additionalProperties", IvyJson::Bool(false)),
    ]);
    let validators = ivy_contracts::schema::SchemaValidators::new();
    let compiled = validators.compile(&schema).unwrap();
    // The UTF-16 name needs 192 KiB once. A second lookup-name list would
    // duplicate it even though the graph already owns the same name.
    assert!(
        compiled.retained_bytes() < 256 * 1024,
        "Compiled state retains {} bytes for a {}-byte ASCII property name",
        compiled.retained_bytes(),
        name.len()
    );
    assert!(
        compiled
            .matches(&IvyJson::object([(name, true.into())]))
            .unwrap()
    );
    assert!(
        !compiled
            .matches(&IvyJson::object([("other", true.into())]))
            .unwrap()
    );
}

#[test]
fn schema_cache_budgets_owned_allocations_and_preserves_caller_handles() {
    use ivy_contracts::schema::SchemaValidators;
    if std::env::var_os("IVY_TEST_SCHEMA_CACHE_MEMORY").is_none() {
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "schema_cache_budgets_owned_allocations_and_preserves_caller_handles",
                "--test-threads=1",
            ])
            .env("IVY_TEST_SCHEMA_CACHE_MEMORY", "1")
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
        return;
    }
    let mut schema = IvyJson::object([
        ("title", "0".into()),
        ("const", "x".repeat(96 * 1024).into()),
    ]);
    // Tiny serialized patterns expand into much larger instruction arrays.
    // Each repeated Unicode class shares one Arc allocation within its program.
    let oversized = IvyJson::object([(
        "properties",
        IvyJson::object((0..70).map(|i| {
            (
                format!("field{i}"),
                IvyJson::object([("pattern", format!(r"\p{{L}}{{1000}}x{{{i}}}").into())]),
            )
        })),
    )]);
    {
        let warm = SchemaValidators::new();
        warm.compile(&schema).unwrap();
        warm.compile(&oversized).unwrap();
    }
    let baseline = LIVE_BYTES.load(Ordering::Relaxed);
    let validators = SchemaValidators::new();
    let first = validators.compile(&schema).unwrap();
    let first_bytes = first.retained_bytes();
    let weak = Arc::downgrade(&first);
    for i in 1..65 {
        schema.insert("title", i.to_string().into());
        drop(validators.compile(&schema).unwrap());
        let retained = LIVE_BYTES.load(Ordering::Relaxed) - baseline;
        assert!(
            retained <= (8 * 1024 * 1024 + first_bytes + 1024) as isize,
            "Cache plus the separately held caller handle retained {retained} bytes"
        );
    }
    assert_eq!(
        Arc::strong_count(&first),
        1,
        "Large entries must evict before 256 compiles"
    );
    assert!(first.matches(schema.get("const").unwrap()).unwrap());
    assert!(!first.matches(&"different".into()).unwrap());

    let small = validators.compile(&IvyJson::Bool(true)).unwrap();
    let before = LIVE_BYTES.load(Ordering::Relaxed);
    let large = validators.compile(&oversized).unwrap();
    let allocated = LIVE_BYTES.load(Ordering::Relaxed) - before;
    assert!(large.retained_bytes() > 2 * 1024 * 1024);
    assert_eq!(
        allocated as usize,
        large.retained_bytes() + 2 * std::mem::size_of::<usize>(),
        "Compiled accounting must match requested allocator bytes, including shared classes once"
    );
    let uncached = Arc::downgrade(&large);
    drop(large);
    assert!(
        uncached.upgrade().is_none(),
        "Oversized validator must remain caller-owned"
    );
    drop(uncached);
    assert!(Arc::ptr_eq(
        &small,
        &validators.compile(&IvyJson::Bool(true)).unwrap()
    ));
    drop(small);
    drop(first);
    assert!(
        weak.upgrade().is_none(),
        "Eviction must not leave a hidden strong owner"
    );
    drop(weak);
    drop(validators);
    let remaining = LIVE_BYTES.load(Ordering::Relaxed) - baseline;
    assert!(
        remaining.abs() < 256,
        "Dropped owners retained {remaining} bytes"
    );
}

#[test]
fn transport_retains_one_catalog_and_small_envelope_tables() {
    // Keep unrelated test/runtime allocations out of the retained-byte comparison.
    if std::env::var_os("IVY_TEST_NATIVE_CATALOG_MEMORY").is_none() {
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "transport_retains_one_catalog_and_small_envelope_tables",
                "--test-threads=1",
            ])
            .env("IVY_TEST_NATIVE_CATALOG_MEMORY", "1")
            .output()
            .unwrap();
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
    runtime.block_on(async {
        let mut catalog = parse(
            include_str!("../../specs/native/codex-0.154.0/catalog.json"),
            32 * 1024 * 1024,
        )
        .unwrap();
        let IvyJson::Object(fields) = &mut catalog else {
            unreachable!()
        };
        let (_, IvyJson::Array(requests)) = fields
            .iter_mut()
            .find(|(name, _)| name == &"clientRequests".into())
            .unwrap()
        else {
            unreachable!()
        };
        requests[0].insert(
            "inputSchema",
            IvyJson::object([(
                "enum",
                IvyJson::Array(vec![IvyJson::Number(0.0); 256 * 1024]),
            )]),
        );
        let catalog = Arc::new(catalog);
        // Initialize the common catalog validator before measuring the transport.
        validate_agent("Catalog", &catalog).unwrap();
        let before_clone = LIVE_BYTES.load(Ordering::Relaxed);
        let redundant = catalog.as_ref().clone();
        let clone_bytes = LIVE_BYTES.load(Ordering::Relaxed) - before_clone;
        assert!(
            clone_bytes >= 8 * 1024 * 1024,
            "Large catalog clone: {clone_bytes}"
        );
        drop(redundant);
        let weak = Arc::downgrade(&catalog);
        let (output, _peer) = tokio::io::duplex(64);
        let baseline = LIVE_BYTES.load(Ordering::Relaxed);
        let rpc = NativeRpc::start(
            catalog.clone(),
            tokio::io::sink(),
            output,
            Events::default(),
        )
        .unwrap();
        tokio::task::yield_now().await;
        let retained = LIVE_BYTES.load(Ordering::Relaxed) - baseline;
        assert!(
            retained < 512 * 1024,
            "Transport retained {retained} extra bytes beside the shared catalog"
        );
        assert!(std::ptr::eq(catalog.as_ref(), rpc.catalog()));
        drop(catalog);
        assert!(weak.upgrade().is_some());
        rpc.close("fixture_closed").await;
        drop(rpc);
        assert!(
            weak.upgrade().is_none(),
            "Closed transport retained the catalog"
        );
    });
}
