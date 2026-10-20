use ivy_host_runtime::platform::windows_process::{process_exited, process_identity};
use std::{
    io,
    os::windows::{io::AsRawHandle, process::CommandExt},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

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
fn predecessor_access_denial_is_not_retirement() {
    // The Windows System process is protected even from ordinary elevated callers.
    let error = process_exited(4, 1).unwrap_err();
    assert_eq!(
        error.raw_os_error(),
        Some(windows_sys::Win32::Foundation::ERROR_ACCESS_DENIED as i32)
    );
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
