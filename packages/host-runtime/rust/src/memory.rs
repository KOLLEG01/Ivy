pub fn start_reclaiming() -> bool {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        use std::{sync::OnceLock, thread, time::Duration};
        static STARTED: OnceLock<bool> = OnceLock::new();
        *STARTED.get_or_init(|| {
            thread::Builder::new()
                .name("ivy-memory".into())
                .stack_size(64 * 1024)
                .spawn(|| {
                    loop {
                        release_unused_memory();
                        thread::sleep(Duration::from_secs(5));
                    }
                })
                .is_ok()
        })
    }
    #[cfg(not(all(target_os = "linux", target_env = "gnu")))]
    false
}

pub fn release_unused_memory() -> bool {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        use std::{
            sync::Mutex,
            time::{Duration, Instant},
        };
        static LAST: Mutex<Option<Instant>> = Mutex::new(None);
        let now = Instant::now();
        let mut last = LAST.lock().unwrap_or_else(|error| error.into_inner());
        if last.is_some_and(|previous| now.duration_since(previous) < Duration::from_secs(30)) {
            return false;
        }
        *last = Some(now);
        drop(last);
        unsafe { libc::malloc_trim(0) != 0 }
    }
    #[cfg(not(all(target_os = "linux", target_env = "gnu")))]
    false
}
