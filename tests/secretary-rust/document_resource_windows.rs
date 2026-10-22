//! Read-only Windows counters for the resource experiment. This never owns,
//! terminates or limits a process. Its initial Job query closes before stdin.
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    mem::{size_of, zeroed},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

#[repr(C)]
#[derive(Default)]
struct Counters {
    size: u32,
    faults: u32,
    peak_working: usize,
    working: usize,
    peak_paged: usize,
    paged: usize,
    peak_nonpaged: usize,
    nonpaged: usize,
    commit: usize,
    peak_commit: usize,
    private: usize,
}
#[repr(C)]
struct Entry {
    size: u32,
    usage: u32,
    pid: u32,
    heap: usize,
    module: u32,
    threads: u32,
    parent: u32,
    priority: i32,
    flags: u32,
    name: [u16; 260],
}
#[link(name = "kernel32")]
unsafe extern "system" {
    fn OpenProcess(access: u32, inherit: i32, pid: u32) -> isize;
    fn CloseHandle(handle: isize) -> i32;
    fn WaitForSingleObject(handle: isize, milliseconds: u32) -> u32;
    fn CreateToolhelp32Snapshot(flags: u32, pid: u32) -> isize;
    fn Process32FirstW(snapshot: isize, entry: *mut Entry) -> i32;
    fn Process32NextW(snapshot: isize, entry: *mut Entry) -> i32;
    fn K32GetProcessMemoryInfo(handle: isize, counters: *mut Counters, bytes: u32) -> i32;
    fn OpenJobObjectW(access: u32, inherit: i32, name: *const u16) -> isize;
    fn IsProcessInJob(process: isize, job: isize, result: *mut i32) -> i32;
}
struct Handle(isize);
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}
struct Process {
    handle: Handle,
    pid: u32,
    parent: u32,
    name: String,
    reads: u64,
    peak_commit: usize,
    peak_working: usize,
    sampled_private: usize,
    last_read_ms: u64,
    read_failures: u64,
    after_exit_read: bool,
    job_member: Option<bool>,
}
fn snapshot() -> Vec<(u32, u32, String)> {
    let raw = unsafe { CreateToolhelp32Snapshot(2, 0) };
    assert!(raw != -1 && raw != 0, "Process observation unavailable");
    let snapshot = Handle(raw);
    let mut entry: Entry = unsafe { zeroed() };
    entry.size = size_of::<Entry>() as u32;
    let mut result = vec![];
    let mut next = unsafe { Process32FirstW(snapshot.0, &mut entry) };
    while next != 0 {
        let end = entry.name.iter().position(|c| *c == 0).unwrap_or(260);
        result.push((
            entry.pid,
            entry.parent,
            String::from_utf16_lossy(&entry.name[..end]),
        ));
        next = unsafe { Process32NextW(snapshot.0, &mut entry) };
    }
    result
}
fn add(
    processes: &mut BTreeMap<u32, Process>,
    pid: u32,
    parent: u32,
    name: String,
    job: Option<&Handle>,
) {
    if processes.contains_key(&pid) {
        return;
    }
    // Query + synchronize only; no mutation or inheritance rights.
    let handle = unsafe { OpenProcess(0x0010_0400, 0, pid) };
    assert_ne!(handle, 0, "Cannot observe owned PID {pid}");
    let job_member = job.map(|job| {
        let mut member = 0;
        assert_ne!(unsafe { IsProcessInJob(handle, job.0, &mut member) }, 0);
        member != 0
    });
    processes.insert(
        pid,
        Process {
            handle: Handle(handle),
            pid,
            parent,
            name,
            reads: 0,
            peak_commit: 0,
            peak_working: 0,
            sampled_private: 0,
            last_read_ms: 0,
            read_failures: 0,
            after_exit_read: false,
            job_member,
        },
    );
}
fn discover(processes: &mut BTreeMap<u32, Process>, job: Option<&Handle>) {
    let entries = snapshot();
    loop {
        let before = processes.len();
        for (pid, parent, name) in &entries {
            if processes.contains_key(parent) && !processes.contains_key(pid) {
                add(processes, *pid, *parent, name.clone(), job);
            }
        }
        if processes.len() == before {
            break;
        }
    }
}
pub struct Observer {
    stop: Arc<AtomicBool>,
    handle: JoinHandle<Value>,
}
impl Observer {
    pub fn start(root: u32, job_name: &str) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let requested = stop.clone();
        let (ready, seen) = mpsc::channel();
        let job_name: Vec<u16> = job_name.encode_utf16().chain([0]).collect();
        let handle = thread::spawn(move || {
            let start = Instant::now();
            let mut processes = BTreeMap::new();
            let raw_job = unsafe { OpenJobObjectW(4, 0, job_name.as_ptr()) };
            assert_ne!(raw_job, 0, "Initial owned Job query unavailable");
            let mut job = Some(Handle(raw_job));
            add(
                &mut processes,
                root,
                0,
                "qualified-job-helper".into(),
                job.as_ref(),
            );
            let mut last_discovery = Instant::now() - Duration::from_secs(1);
            let mut last_sample = start;
            let mut max_gap_us = 0;
            let mut aggregate_private = 0;
            let mut aggregate_working = 0;
            let mut announced = false;
            loop {
                if last_discovery.elapsed() >= Duration::from_millis(10) {
                    discover(&mut processes, job.as_ref());
                    last_discovery = Instant::now();
                }
                let now = Instant::now();
                max_gap_us = max_gap_us.max(now.duration_since(last_sample).as_micros());
                last_sample = now;
                let mut private = 0;
                let mut working = 0;
                for process in processes.values_mut() {
                    let mut counters = Counters {
                        size: size_of::<Counters>() as u32,
                        ..Default::default()
                    };
                    if unsafe {
                        K32GetProcessMemoryInfo(
                            process.handle.0,
                            &mut counters,
                            size_of::<Counters>() as u32,
                        )
                    } != 0
                    {
                        process.reads += 1;
                        process.peak_commit = process.peak_commit.max(counters.peak_commit);
                        process.peak_working = process.peak_working.max(counters.peak_working);
                        process.sampled_private = process.sampled_private.max(counters.private);
                        process.last_read_ms = start.elapsed().as_millis() as u64;
                        process.after_exit_read |=
                            unsafe { WaitForSingleObject(process.handle.0, 0) } == 0;
                        private += counters.private;
                        working += counters.working;
                    } else {
                        process.read_failures += 1;
                    }
                }
                aggregate_private = aggregate_private.max(private);
                aggregate_working = aggregate_working.max(working);
                if !announced
                    && processes
                        .values()
                        .any(|p| p.job_member == Some(true) && p.reads > 0)
                {
                    // A console host is not evidence that the actual parser was
                    // observed. Release our query handle before input or EOF.
                    drop(job.take());
                    let _ = ready.send(());
                    announced = true;
                }
                if requested.load(Ordering::SeqCst) {
                    break;
                }
                thread::sleep(Duration::from_millis(1));
            }
            let rows: Vec<_> = processes
                .values()
                .map(|p| {
                    json!({
                        "pid":p.pid,"parentPid":p.parent,"name":p.name,"launcher":p.pid==root,
                        "peakPrivateCommitBytes":p.peak_commit,"peakWorkingSetBytes":p.peak_working,
                        "sampledMaxPrivateBytes":p.sampled_private,"successfulReads":p.reads,
                        "lastSuccessfulReadMs":p.last_read_ms,"failedReads":p.read_failures,
                        "successfulReadAfterExit":p.after_exit_read,
                        "jobMemberAtSetup":p.job_member,
                        "exited":unsafe { WaitForSingleObject(p.handle.0,500) }==0
                    })
                })
                .collect();
            json!({"processes":rows,"sampledAggregatePrivateBytes":aggregate_private,
                "sampledAggregateWorkingSetBytes":aggregate_working,"maximumSampleGapUs":max_gap_us,
                "desiredSampleIntervalMs":1,"descendantDiscoveryIntervalMs":10,
                "jobQueryClosedBeforeInput":announced,
                "semantics":"Kernel process peak commit and working-set counters. Initial Job membership is queried before stdin; console hosts are recorded separately and are not presumed Job-owned. Aggregate samples are lower bounds and RSS sums double-count shared pages. Worker threads share their Node process. No OS cap or virtual-address-space measurement."})
        });
        seen.recv_timeout(Duration::from_secs(3))
            .expect("Owned parser PID observation unavailable before stdin");
        Self { stop, handle }
    }
    pub fn finish(self) -> Value {
        self.stop.store(true, Ordering::SeqCst);
        self.handle.join().expect("Process observer failed")
    }
}
