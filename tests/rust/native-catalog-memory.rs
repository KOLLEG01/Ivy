use ivy_contracts::IvyJson;
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
