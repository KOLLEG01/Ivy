use super::*;
use std::ptr::null;
use windows_sys::Win32::{
    Foundation::{GetHandleInformation, HANDLE_FLAG_INHERIT},
    System::JobObjects::CreateJobObjectW,
};

fn empty_job() -> OwnedHandle {
    let raw = unsafe { CreateJobObjectW(null(), null()) };
    assert!(!raw.is_null());
    unsafe { OwnedHandle::from_raw_handle(raw) }
}

fn running_process() -> OwnedHandle {
    duplicate(
        unsafe { GetCurrentProcess() },
        unsafe { GetCurrentProcess() },
        PROCESS_SYNCHRONIZE,
    )
    .unwrap()
}

#[tokio::test]
async fn empty_job_is_insufficient_until_the_actual_child_signals() {
    let proof = Proof {
        child: running_process(),
        job: empty_job(),
    };
    assert!(!proof.retired().unwrap());
    let error = proof
        .retire(Instant::now() + Duration::from_millis(20))
        .await
        .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
}

#[tokio::test]
async fn access_failures_never_report_retirement_and_copied_handles_are_not_inherited() {
    let job = empty_job();
    let query_only =
        duplicate(unsafe { GetCurrentProcess() }, job.as_raw_handle(), 0x0004).unwrap();
    let proof = Proof {
        child: running_process(),
        job: query_only,
    };
    for handle in [&proof.child, &proof.job] {
        let mut flags = 0;
        checked(unsafe { GetHandleInformation(handle.as_raw_handle(), &mut flags) }).unwrap();
        assert_eq!(flags & HANDLE_FLAG_INHERIT, 0);
    }
    let error = proof
        .retire(Instant::now() + Duration::from_secs(1))
        .await
        .unwrap_err();
    assert_eq!(error.raw_os_error(), Some(5));
    let terminate_only =
        duplicate(unsafe { GetCurrentProcess() }, job.as_raw_handle(), 0x0008).unwrap();
    let proof = Proof {
        child: running_process(),
        job: terminate_only,
    };
    assert_eq!(proof.retired().unwrap_err().raw_os_error(), Some(5));
}
