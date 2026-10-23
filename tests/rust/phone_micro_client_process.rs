use super::*;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use windows_sys::Win32::{
    Foundation::{WAIT_OBJECT_0, WAIT_TIMEOUT},
    System::Threading::{
        OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE,
        TerminateProcess, WaitForSingleObject,
    },
};

struct Child(OwnedHandle);
impl Child {
    fn open(pid: u32) -> Self {
        let raw = unsafe {
            OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE | PROCESS_TERMINATE,
                0,
                pid,
            )
        };
        assert!(
            !raw.is_null(),
            "Owned fixture handle unavailable: {}",
            io::Error::last_os_error()
        );
        Self(unsafe { OwnedHandle::from_raw_handle(raw) })
    }
    fn exited(&self, milliseconds: u32) -> bool {
        match unsafe { WaitForSingleObject(self.0.as_raw_handle(), milliseconds) } {
            WAIT_OBJECT_0 => true,
            WAIT_TIMEOUT => false,
            _ => panic!("Owned fixture exit observation failed"),
        }
    }
}
impl Drop for Child {
    fn drop(&mut self) {
        if !self.exited(0) {
            unsafe {
                TerminateProcess(self.0.as_raw_handle(), u32::MAX);
            }
            let _ = self.exited(5000);
        }
    }
}
fn emit(name: &str, value: &Value) {
    if let Ok(root) = std::env::var("IVY_PHONE_MICRO_ROOT_TRACE") {
        std::fs::write(
            Path::new(&root).join(name),
            serde_json::to_vec_pretty(value).unwrap(),
        )
        .unwrap();
    }
}
fn row(fixture: &Fixture, mode: &str) -> Value {
    let index = fixture.next.fetch_add(1, Ordering::SeqCst);
    let ready = fixture.root.0.join(format!("root-{index}.json"));
    let release = fixture.root.0.join(format!("release-{index}"));
    let script = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../tests/fixtures/phone-micro-root-child.mjs");
    json!({"mode":mode,"root":fixture.root.0,"ready":ready,"release":release,
        "args":[script,mode,ready,release,"","space value","quote\"value","é𐐷","tail\\"]})
}
async fn ready(path: &Path) -> Value {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Ok(bytes) = std::fs::read(path) {
                if let Ok(value) = serde_json::from_slice(&bytes) {
                    return value;
                }
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("Owned root readiness")
}
fn work(fixture: &Fixture, row: &Value, stop: CancellationToken) -> MicroCommand {
    let client = MicroUsbClient::new(settings(
        &fixture.root.0.join("usbip.exe"),
        &format!("sha256:{}", fixture.pins[0].1),
    ));
    let args = row["args"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_owned())
        .collect();
    client.run_pinned(
        args,
        stop,
        Ok(MicroClientPins::fixture(fixture.pins.clone())),
    )
}
fn normalize(value: &mut Value) {
    if matches!(
        value["mode"].as_str().unwrap(),
        "ok" | "exit7" | "closed-desc"
    ) && value["completed"] == true
    {
        value["output"] = serde_json::from_str(value["output"].as_str().unwrap()).unwrap();
    }
}
async fn scenario(fixture: &Fixture, row: &Value) -> Value {
    let mode = row["mode"].as_str().unwrap();
    let stop = CancellationToken::new();
    let work = work(fixture, row, stop.clone());
    let state = ready(Path::new(row["ready"].as_str().unwrap())).await;
    let mut children = Vec::new();
    for suffix in [".branch", ".leaf"] {
        let path = format!("{}{suffix}", row["ready"].as_str().unwrap());
        if Path::new(&path).exists() {
            let v = ready(Path::new(&path)).await;
            children.push(Child::open(v["pid"].as_u64().unwrap() as u32));
        }
    }
    if matches!(mode, "closed-desc" | "held-desc") {
        let root = Child::open(state["pid"].as_u64().unwrap() as u32);
        std::fs::write(format!("{}.exit", row["ready"].as_str().unwrap()), "").unwrap();
        assert!(root.exited(5000));
        assert!(children.iter().all(|c| !c.exited(0)));
        if mode == "held-desc" {
            fixture.assert_leased(true);
            stop.cancel();
        }
    }
    if mode == "live-tree" {
        assert_eq!(children.len(), 2);
        fixture.assert_leased(true);
        stop.cancel();
    }
    let native = tokio::time::timeout(Duration::from_secs(15), work)
        .await
        .unwrap()
        .unwrap_or_else(|_| panic!("Concrete command unexpectedly faulted"));
    let mut output = native.output;
    if native.completed && matches!(mode, "ok" | "exit7" | "closed-desc") {
        let v: Value = serde_json::from_str(&output).unwrap();
        output=json!({"args":v["args"],"cwdMatches":Path::new(v["cwd"].as_str().unwrap())==fixture.root.0,"inherited":v["inherited"]}).to_string();
    }
    let alive: Vec<_> = children
        .iter()
        .map(|c| !c.exited(if mode == "live-tree" { 2000 } else { 0 }))
        .collect();
    if mode == "closed-desc" || mode == "held-desc" {
        assert_eq!(alive, vec![true]);
    }
    if mode == "live-tree" {
        assert_eq!(alive, vec![false, false]);
    }
    assert_eq!(
        std::fs::read_to_string(format!("{}.starts", row["ready"].as_str().unwrap()))
            .unwrap()
            .lines()
            .count(),
        1
    );
    fixture.assert_leased(false);
    std::fs::write(row["release"].as_str().unwrap(), "").unwrap();
    for child in &children {
        assert!(child.exited(5000));
    }
    let mut value = json!({"mode":mode,"started":native.started,"completed":native.completed,"exitCode":native.exit_code,"output":output,"errorCode":native.error_code,"descendantsAlive":alive});
    normalize(&mut value);
    value
}
fn root_fixture(modes: &[&str]) -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../tests/fixtures/phone-micro-root-cases.json");
    let mut expected: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let rows = expected.as_array_mut().unwrap();
    assert_eq!(rows.len(), modes.len());
    for (row, mode) in rows.iter_mut().zip(modes) {
        assert_eq!(row["mode"], *mode);
        normalize(row);
    }
    expected
}
#[test]
fn concrete_root_exit_pipes_codepage_bounds_and_cleanup_follow_public_cases() {
    let fixture = Fixture::new();
    let modes = [
        "ok",
        "exit7",
        "raw",
        "utf8",
        "utf16",
        "stderr-bound",
        "stderr-over",
        "stdout-over",
        "closed-desc",
        "held-desc",
        "live-tree",
    ];
    let actual = runtime().block_on(async {
        let mut values = Vec::new();
        for mode in modes {
            emit(
                "platform-in-progress.json",
                &json!({"mode":mode,"phase":"starting"}),
            );
            values.push(scenario(&fixture, &row(&fixture, mode)).await);
            emit("platform-rust.json", &Value::Array(values.clone()));
        }
        Value::Array(values)
    });
    emit("platform-rust.json", &actual);
    let expected = root_fixture(&modes);
    let differences: Vec<_> = actual
        .as_array()
        .unwrap()
        .iter()
        .zip(expected.as_array().unwrap())
        .filter(|(a, b)| a != b)
        .map(|(a, b)| json!({"rust":a,"platform":b}))
        .collect();
    emit(
        "platform-comparisons.json",
        &json!({"cases":modes.len(),"differences":differences,"rust":actual,"expected":expected,"scope":"Actual Windows root process with private test pins and public output, exit and cleanup cases. No installed USB/IP client or driver is used."}),
    );
    assert_eq!(actual, expected);
}
#[test]
fn concrete_deadline_and_dropped_observer_retain_leases_and_one_command() {
    runtime().block_on(async {
        // A synchronous kill prefix precedes the original drain clock. This
        // contract control does not simulate native OS kill latency/failure.
        let clock_start=Instant::now();
        let clock_result=run_started_micro_command(tokio::io::empty(),tokio::io::empty(),
            async {Err(io::Error::other("fixture exit failure"))},
            || {std::thread::sleep(Duration::from_millis(150));async {std::future::pending::<()>().await;}},
            &CancellationToken::new(),[437,437]).await;
        let clock_ms=clock_start.elapsed().as_millis();
        assert_eq!(clock_result.error_code.as_deref(),Some("micro_attach_unknown"));
        assert!((2100..5000).contains(&clock_ms));
        let fixture=Fixture::new();let timed=row(&fixture,"live-tree");let stop=CancellationToken::new();let command=work(&fixture,&timed,stop);
        let root=ready(Path::new(timed["ready"].as_str().unwrap())).await;let root=Child::open(root["pid"].as_u64().unwrap() as u32);
        let mut children=Vec::new();for suffix in [".branch",".leaf"]{let v=ready(Path::new(&format!("{}{suffix}",timed["ready"].as_str().unwrap()))).await;children.push(Child::open(v["pid"].as_u64().unwrap() as u32));}
        fixture.assert_leased(true);let start=Instant::now();let timed_result=tokio::time::timeout(Duration::from_secs(15),command).await.unwrap().unwrap_or_else(|_|panic!("Deadline fault"));let elapsed=start.elapsed().as_millis();
        assert!(!timed_result.completed&&timed_result.started);assert_eq!(timed_result.error_code.as_deref(),Some("micro_attach_unknown"));assert!((8000..=12000).contains(&elapsed));assert!(root.exited(2000));assert!(children.iter().all(|c|c.exited(2000)));fixture.assert_leased(false);
        let retained=row(&fixture,"release");let observer=work(&fixture,&retained,CancellationToken::new());let pid=ready(Path::new(retained["ready"].as_str().unwrap())).await["pid"].as_u64().unwrap() as u32;let process=Child::open(pid);drop(observer);
        tokio::time::sleep(Duration::from_millis(40)).await;fixture.assert_leased(true);assert!(!process.exited(0));std::fs::write(retained["release"].as_str().unwrap(),"").unwrap();assert!(process.exited(5000));
        tokio::time::timeout(Duration::from_secs(2),async {while fixture.probe().as_array().unwrap().iter().any(|r|r["write"]!=true){tokio::time::sleep(Duration::from_millis(5)).await;}}).await.unwrap();fixture.assert_leased(false);
        assert_eq!(std::fs::read_to_string(format!("{}.starts",retained["ready"].as_str().unwrap())).unwrap().lines().count(),1);
        emit("concrete-retention.json",&json!({"deadlineResult":result(&timed_result),"elapsedFromReadinessMs":elapsed,"rootAndBothDescendantsRetired":true,"usingVarLeasesReleased":true,"droppedObserverRetainedOneCommandAndLeases":true,"rootWaitClock":{"synchronousPrefixMs":150,"totalPrefixAndDrainMs":clock_ms,"scope":"Injected callback timing contract, not forced native OS kill latency or failure."},"scope":"Actual default launcher with private Node copy and test-only hashes; no installed release client or driver."}));
    });
}
