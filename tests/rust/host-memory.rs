#[cfg(all(target_os = "linux", target_env = "gnu"))]
#[test]
fn released_pages_return_while_the_service_thread_is_blocked() {
    use ivy_host_runtime::memory::{release_unused_memory, start_reclaiming};
    use std::time::{Duration, Instant};
    const CHILD: &str = "IVY_MEMORY_TEST_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "released_pages_return_while_the_service_thread_is_blocked",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    fn resident_bytes() -> usize {
        std::fs::read_to_string("/proc/self/status")
            .unwrap()
            .lines()
            .find(|line| line.starts_with("VmRSS:"))
            .unwrap()
            .split_whitespace()
            .nth(1)
            .unwrap()
            .parse::<usize>()
            .unwrap()
            * 1024
    }
    let mut chunks = Vec::with_capacity(4096);
    let mut anchors = Vec::with_capacity(4096);
    for _ in 0..4096 {
        chunks.push(vec![7u8; 16 * 1024].into_boxed_slice());
        anchors.push(Box::new(1u64));
    }
    let peak = resident_bytes();
    drop(chunks);
    assert!(start_reclaiming());
    assert!(start_reclaiming());
    let deadline = Instant::now() + Duration::from_secs(5);
    while resident_bytes() + 24 * 1024 * 1024 >= peak && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    let released = resident_bytes();
    assert!(
        released + 24 * 1024 * 1024 < peak,
        "before={peak}, after={released}"
    );
    assert!(!release_unused_memory());
    std::hint::black_box(&anchors);
}

#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
#[test]
fn other_allocators_do_not_require_glibc() {
    assert!(!ivy_host_runtime::memory::start_reclaiming());
    assert!(!ivy_host_runtime::memory::release_unused_memory());
}
