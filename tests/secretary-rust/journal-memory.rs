use super::{Limits, memory, value};
use ivy_contracts::IvyJson;
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

#[test]
fn cleanup_drops_record_bodies_between_rows() {
    const CHILD: &str = "IVY_SECRETARY_RETENTION_MEMORY_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "retention_memory::cleanup_drops_record_bodies_between_rows",
                "--nocapture",
            ])
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
    let mut journal = memory(Limits::default());
    let mut document = value(r#"{"phase":"succeeded"}"#);
    document.insert("body", IvyJson::from("x".repeat(128 * 1024)));
    for index in 0..48 {
        let saved = journal
            .create_technical("workflow", &format!("record-{index}"), &document)
            .unwrap();
        journal
            .complete_technical_workflow(&saved.pin.object_id, 0)
            .unwrap();
    }
    drop(document);
    let before = LIVE.load(Ordering::Relaxed);
    PEAK.store(before, Ordering::Relaxed);
    journal.limits.count = 0;
    journal.cleanup(0, 0, None, None).unwrap();
    let extra = PEAK.load(Ordering::Relaxed) - before;
    eprintln!(
        "Secretary cleanup Rust allocation peak above baseline: {extra} bytes for 48 x 128 KiB bodies"
    );
    // Count actual Rust allocator requests, independently of SQLite's native heap or RSS.
    assert!(
        extra < 2 * 1024 * 1024,
        "Cleanup retained the record bodies: {extra} bytes"
    );
    assert_eq!(journal.usage().unwrap().count, 0);
}
