use ivy_host_runtime::platform::windows_process::process_parent_id;
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Write},
    os::windows::process::CommandExt,
    process::{Child, Command, Stdio},
};

struct Original {
    child: Child,
    output: BufReader<std::process::ChildStdout>,
}
impl Original {
    fn read(&mut self) -> Value {
        let mut line = String::new();
        assert_ne!(
            self.output.read_line(&mut line).unwrap(),
            0,
            "Original fixture closed."
        );
        serde_json::from_str(&line).unwrap()
    }
    fn parent(&mut self, pid: u32) -> Value {
        writeln!(self.child.stdin.as_mut().unwrap(), "{}", json!({"pid":pid})).unwrap();
        self.child.stdin.as_mut().unwrap().flush().unwrap();
        self.read()
    }
}
impl Drop for Original {
    fn drop(&mut self) {
        drop(self.child.stdin.take());
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}

#[test]
fn parent_snapshot_matches_original_bounds_and_closes_transient_handles() {
    let _serial = super::serial_fixture();
    let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../tests/fixtures/host-parent-pid/observe.ps1");
    let mut child = Command::new(
        std::env::var_os("IVY_PARENT_PID_POWERSHELL").unwrap_or_else(|| "pwsh.exe".into()),
    )
    .args(["-NoLogo", "-NoProfile", "-NonInteractive", "-File"])
    .arg(script)
    .creation_flags(0x08000000)
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::inherit())
    .spawn()
    .unwrap();
    let output = BufReader::new(child.stdout.take().unwrap());
    let mut original = Original { child, output };
    let hello = original.read();
    assert_eq!(hello["entrySize"], 568);
    let child_pid = u32::try_from(hello["pid"].as_u64().unwrap()).unwrap();
    let mut rows = Vec::new();
    let mut own_parent = None;
    for (label, pid) in [
        ("current", std::process::id()),
        ("owned-fixture", child_pid),
        ("idle", 0),
        ("absent", i32::MAX as u32),
    ] {
        let expected = original.parent(pid);
        let actual = process_parent_id(pid, 65_536).unwrap();
        if label == "absent" {
            assert_eq!(expected["error"], "InvalidOperationException");
            assert_eq!(actual, None);
        } else {
            let parent = u32::try_from(expected["parent"].as_u64().unwrap()).unwrap();
            assert_eq!(actual, Some(parent), "{label}");
        }
        if label == "current" {
            own_parent = actual;
        }
        if label == "owned-fixture" {
            assert_eq!(actual, Some(std::process::id()));
        }
        rows.push(json!({"label":label,"pid":pid,"original":expected,"actual":actual}));
    }
    for limit in [0, 1] {
        let error = process_parent_id(i32::MAX as u32, limit).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        rows.push(json!({"limit":limit,"error":"InvalidData"}));
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetCurrentProcess() -> *mut std::ffi::c_void;
        fn GetProcessHandleCount(process: *mut std::ffi::c_void, count: *mut u32) -> i32;
    }
    let handles = || {
        let mut count = 0;
        assert_ne!(
            unsafe { GetProcessHandleCount(GetCurrentProcess(), &mut count) },
            0
        );
        count
    };
    let before = handles();
    for _ in 0..128 {
        assert_eq!(
            process_parent_id(std::process::id(), 65_536).unwrap(),
            own_parent
        );
        assert_eq!(
            process_parent_id(i32::MAX as u32, 1).unwrap_err().kind(),
            std::io::ErrorKind::InvalidData
        );
    }
    let after = handles();
    assert_eq!(
        after, before,
        "Transient success and bound-failure snapshots must close."
    );
    writeln!(original.child.stdin.as_mut().unwrap(), "done").unwrap();
    original.child.stdin.as_mut().unwrap().flush().unwrap();
    assert!(original.child.wait().unwrap().success());
    if let Some(path) = std::env::var_os("IVY_PARENT_PID_OBSERVATION") {
        std::fs::write(
            path,
            serde_json::to_vec_pretty(&json!({
                "hello":hello,"comparisons":rows,"successLifetimes":128,
                "boundFailureLifetimes":128,"handlesBefore":before,"handlesAfter":after
            }))
            .unwrap(),
        )
        .unwrap();
    }
}
