use ivy_host_runtime::platform::windows_process::{
    current_process_created_filetime, process_exited, process_identity,
};
use std::{
    io,
    os::windows::{
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
        process::CommandExt,
    },
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

#[test]
fn current_pseudo_handle_creation_time_matches_the_retained_current_identity() {
    let current = process_identity(std::process::id()).unwrap().unwrap();
    let created = current_process_created_filetime().unwrap();
    assert_ne!(created, 0);
    assert_eq!(created, current.created_filetime);
    for _ in 0..16 {
        assert_eq!(current_process_created_filetime().unwrap(), created);
    }
    assert_eq!(
        std::thread::spawn(current_process_created_filetime)
            .join()
            .unwrap()
            .unwrap(),
        created
    );
    assert!(!process_exited(current.pid, created).unwrap());
    if let Some(path) = std::env::var_os("IVY_CURRENT_FILETIME_OBSERVATION") {
        std::fs::write(path, format!("{} {}", current.pid, created)).unwrap();
    }
}

#[test]
fn predecessor_admission_absence_and_identity_mismatch() {
    let current = process_identity(std::process::id()).unwrap().unwrap();
    for (pid, created) in [(0, current.created_filetime), (current.pid, 0), (0, 0)] {
        assert_eq!(
            process_exited(pid, created).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
    }
    assert!(process_exited(u32::MAX, 1).unwrap());
    assert!(!process_exited(current.pid, current.created_filetime).unwrap());
    // A deliberately different FILETIME exercises the PID-reuse branch without
    // trying to force the kernel to recycle an actual process identifier.
    assert!(process_exited(current.pid, current.created_filetime ^ 1).unwrap());
}

#[test]
fn predecessor_system_inspection_preserves_observed_native_outcome() {
    use windows_sys::Win32::{
        Foundation::{ERROR_ACCESS_DENIED, ERROR_INVALID_PARAMETER, FILETIME},
        System::Threading::{
            GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
        },
    };

    // System inspection permissions differ between hosts. Probe the same rights
    // independently so an API that swallows a real denial cannot pass this case.
    let raw = unsafe {
        OpenProcess(
            PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION,
            0,
            4,
        )
    };
    let (opened, native_error, native_created) = if raw.is_null() {
        (false, io::Error::last_os_error().raw_os_error(), None)
    } else {
        let process = unsafe { OwnedHandle::from_raw_handle(raw) };
        let mut created: FILETIME = unsafe { std::mem::zeroed() };
        let mut exited = created;
        let mut kernel = created;
        let mut user = created;
        if unsafe {
            GetProcessTimes(
                process.as_raw_handle(),
                &mut created,
                &mut exited,
                &mut kernel,
                &mut user,
            )
        } == 0
        {
            (true, io::Error::last_os_error().raw_os_error(), None)
        } else {
            (
                true,
                None,
                Some((u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime)),
            )
        }
    };
    if let Some(code) = native_error {
        if !opened && code == ERROR_INVALID_PARAMETER as i32 {
            assert!(process_exited(4, 1).unwrap());
        } else {
            assert_eq!(process_exited(4, 1).unwrap_err().raw_os_error(), Some(code));
        }
    } else {
        assert_ne!(native_created.unwrap(), 1);
        assert!(process_exited(4, 1).unwrap());
    }
    if let Some(path) = std::env::var_os("IVY_PREDECESSOR_PERMISSION_OBSERVATION") {
        let observation = serde_json::json!({
            "pid": 4,
            "recordedFiletime": "1",
            "processOpened": opened,
            "nativeError": native_error,
            "nativeCreatedFiletime": native_created.map(|value| value.to_string()),
            "accessDenialCoverage": if native_error == Some(ERROR_ACCESS_DENIED as i32) {
                "observed"
            } else {
                "unavailable"
            },
        });
        std::fs::write(path, serde_json::to_vec(&observation).unwrap()).unwrap();
    }
}

struct OwnedFixture(Child);
impl Drop for OwnedFixture {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn predecessor_live_and_signaled_same_identity() {
    let mut child = OwnedFixture(
        Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "child_fixture", "--nocapture"])
            .env("IVY_PROCESS_FIXTURE", "input")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW)
            .spawn()
            .unwrap(),
    );
    let identity = process_identity(child.0.id()).unwrap().unwrap();
    assert!(!process_exited(identity.pid, identity.created_filetime).unwrap());

    // EOF makes this existing finite fixture exit naturally. Keep its original
    // Child handle alive so the signaled object and exact PID remain observable.
    drop(child.0.stdin.take());
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        assert!(
            Instant::now() < deadline,
            "Finite predecessor fixture did not exit."
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    // Image queries can fail after exit (ERROR_GEN_FAILURE). Observe only the
    // exact retained creation time, as the new read-only API does.
    let mut created: windows_sys::Win32::Foundation::FILETIME = unsafe { std::mem::zeroed() };
    let mut exited = created;
    let mut kernel = created;
    let mut user = created;
    assert_ne!(
        unsafe {
            windows_sys::Win32::System::Threading::GetProcessTimes(
                child.0.as_raw_handle(),
                &mut created,
                &mut exited,
                &mut kernel,
                &mut user,
            )
        },
        0
    );
    let retained = (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime);
    assert_eq!(retained, identity.created_filetime);
    assert!(process_exited(identity.pid, identity.created_filetime).unwrap());
}
