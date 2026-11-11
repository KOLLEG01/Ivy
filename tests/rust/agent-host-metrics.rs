use ivy_agent_manager::host_metrics::{
    CPU_AVERAGE, CPU_GAP, CpuAverage, CpuTimes, HostMetrics, Volume, parse_meminfo,
    parse_proc_stat, project_volumes,
};
use ivy_contracts::{IvyJson, JsString, agent_schema::validate_agent};
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

fn times(busy: u64, total: u64) -> CpuTimes {
    CpuTimes { busy, total }
}

#[test]
fn cpu_average_warms_up_weights_by_time_and_restarts_after_gaps() {
    let start = Instant::now();
    let mut cpu = CpuAverage::default();
    cpu.observe(start, times(0, 0));
    assert_eq!(
        cpu.percent(),
        None,
        "one counter reading is not an interval"
    );
    // System counters cover all logical CPUs: 3 of 8 busy is 37.5%, not 300%.
    let minute = Duration::from_secs(60);
    cpu.observe(start + minute, times(3 * 60, 8 * 60));
    assert_eq!(cpu.percent(), Some(37.5));
    assert_eq!(cpu.covered(), minute);
    // A fully busy minute moves the average by 1 - exp(-60/300), not halfway.
    cpu.observe(start + 2 * minute, times(3 * 60 + 8 * 60, 16 * 60));
    let alpha = 1.0 - (-60.0 / CPU_AVERAGE.as_secs_f64()).exp();
    let expected = ((0.375 + alpha * (1.0 - 0.375)) * 1000.0_f64).round() / 10.0;
    assert_eq!(cpu.percent(), Some(expected));
    // Repeated identical counters carry no interval and keep the average.
    cpu.observe(start + 2 * minute, times(3 * 60 + 8 * 60, 16 * 60));
    assert_eq!(cpu.percent(), Some(expected));
    // A long unobserved interval no longer describes the last minutes.
    let late = start + 2 * minute + CPU_GAP + Duration::from_secs(1);
    cpu.observe(late, times(20_000, 40_000));
    assert_eq!(cpu.percent(), None);
    assert_eq!(cpu.covered(), Duration::ZERO);
    cpu.observe(late + minute, times(20_000 + 30, 40_000 + 60));
    assert_eq!(cpu.percent(), Some(50.0));
    // Restarted counters restart the average instead of producing a negative sample.
    cpu.observe(late + 2 * minute, times(5, 10));
    assert_eq!(cpu.percent(), None);
}

#[test]
fn linux_counters_and_memory_are_parsed_as_documented() {
    let stat = "cpu  100 20 30 800 50 0 0 0 7 0\ncpu0 1 2 3 4 5 6 7 8 9 10\n";
    // iowait is idle; guest is already contained in user.
    assert_eq!(parse_proc_stat(stat), Some(times(150, 1000)));
    assert_eq!(parse_proc_stat("cpu0 1 2 3 4\n"), None);
    assert_eq!(parse_proc_stat("cpu  1 x 3 4\n"), None);
    let meminfo = "MemTotal:       16000 kB\nMemFree:         1000 kB\nMemAvailable:    4000 kB\n";
    assert_eq!(parse_meminfo(meminfo), Some((16000 * 1024, 12000 * 1024)));
    assert_eq!(parse_meminfo("MemTotal: 16000 kB\n"), None);
}

#[test]
fn each_project_volume_is_reported_once_with_used_bytes() {
    let root = std::env::temp_dir();
    let missing = root.join("not-yet-created").join("projects");
    let roots: Vec<JsString> = [&root, &missing, &root.join("other")]
        .iter()
        .map(|path| path.to_str().unwrap().into())
        .collect();
    let calls = AtomicUsize::new(0);
    let volumes = project_volumes(&roots, |path| {
        calls.fetch_add(1, Ordering::Relaxed);
        // A missing project root is measured at its existing parent.
        assert!(path.exists());
        Some(Volume {
            key: if path == root { "a" } else { "b" }.into(),
            label: Some("V:\\".into()),
            total: 1000,
            free: 250,
        })
    });
    assert_eq!(calls.load(Ordering::Relaxed), 3);
    assert_eq!(volumes.len(), 1, "the same filesystem is not counted twice");
    let volume = &volumes[0];
    assert_eq!(volume.get("path"), Some(&IvyJson::String(roots[0].clone())));
    assert_eq!(volume.get("usedBytes"), Some(&IvyJson::Number(750.0)));
    assert_eq!(volume.get("totalBytes"), Some(&IvyJson::Number(1000.0)));
    assert!(project_volumes(&roots, |_| None).is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn readers_share_one_snapshot_of_the_actual_host() {
    let metrics = Arc::new(HostMetrics::default());
    // Configured Windows roots may use either separator.
    let root = std::env::temp_dir().to_str().unwrap().replace('\\', "/");
    let reads = Arc::new(AtomicUsize::new(0));
    let counted = reads.clone();
    metrics.set_project_roots(Arc::new(move || {
        counted.fetch_add(1, Ordering::Relaxed);
        vec![JsString::from(root.as_str()), JsString::from(root.as_str())]
    }));
    let concurrent = (0..16).map(|_| {
        let metrics = metrics.clone();
        tokio::spawn(async move { metrics.read().await.unwrap() })
    });
    let values = futures_util::future::join_all(concurrent).await;
    let first = values[0].as_ref().unwrap();
    for value in &values {
        assert_eq!(value.as_ref().unwrap(), first);
    }
    assert_eq!(
        reads.load(Ordering::Relaxed),
        1,
        "one sample serves all readers"
    );
    validate_agent("HostMetrics", first).unwrap();
    let cpu = first.get("cpu").unwrap();
    if cfg!(any(windows, target_os = "linux")) {
        let memory = first.get("memory").unwrap();
        let total = memory.get("totalBytes").unwrap().as_f64().unwrap();
        let used = memory.get("usedBytes").unwrap().as_f64().unwrap();
        assert!(total > 0.0 && used > 0.0 && used <= total);
        // The first reading starts the interval; no percentage is invented.
        assert_eq!(cpu.get("usedPercent"), Some(&IvyJson::Null));
        assert_eq!(cpu.get("coveredSeconds"), Some(&IvyJson::Number(0.0)));
    }
    let volumes = first.get("volumes").unwrap().as_array().unwrap();
    assert_eq!(volumes.len(), 1);
    let used = volumes[0].get("usedBytes").unwrap().as_f64().unwrap();
    assert!(used > 0.0 && used <= volumes[0].get("totalBytes").unwrap().as_f64().unwrap());
    assert_eq!(metrics.read().await.unwrap(), *first);
    assert_eq!(reads.load(Ordering::Relaxed), 1);
}
