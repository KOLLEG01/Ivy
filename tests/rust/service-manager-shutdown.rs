use ivy_contracts::{IvyError, IvyResult};
use ivy_service_manager::shutdown::{Cleanup, CleanupAction, Shutdown};
use std::sync::{Arc, Mutex};
use tokio::sync::Notify;

#[tokio::test]
async fn shutdown_joins_admitted_work_after_its_transport_waiter_disappears() {
    use ivy_service_manager::connection::Admission;
    let admission = Admission::default();
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let written = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let work = {
        let (entered, release, written) = (entered.clone(), release.clone(), written.clone());
        Box::pin(async move {
            entered.notify_one();
            release.notified().await;
            written.store(true, std::sync::atomic::Ordering::Release);
            Ok(())
        })
    };
    let waiter = tokio::spawn({
        let admission = admission.clone();
        async move { admission.run(work).await }
    });
    entered.notified().await;
    waiter.abort();
    let _ = waiter.await;
    admission.stop();
    assert_eq!(
        admission
            .run(Box::pin(async { Ok(()) }))
            .await
            .unwrap_err()
            .code,
        "service_unavailable"
    );
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(20), admission.drain())
            .await
            .is_err()
    );
    release.notify_one();
    tokio::time::timeout(std::time::Duration::from_secs(2), admission.drain())
        .await
        .unwrap();
    assert!(written.load(std::sync::atomic::Ordering::Acquire));
}

fn action(log: &Arc<Mutex<Vec<&'static str>>>, name: &'static str, fail: bool) -> CleanupAction {
    let log = log.clone();
    Box::pin(async move {
        log.lock().unwrap().push(name);
        if fail {
            Err(IvyError::new(name, "Injected cleanup failure."))
        } else {
            Ok(())
        }
    })
}

#[tokio::test]
async fn each_failure_still_attempts_every_later_resource_in_order() {
    let names = [
        "owner-c", "owner-b", "owner-a", "service", "drain", "health", "journal",
    ];
    // Includes success, individual failures and multiple simultaneous failures.
    for failures in [0u8, 1, 2, 4, 8, 16, 32, 64, 127] {
        let log = Arc::new(Mutex::new(Vec::new()));
        let make = |n: usize| action(&log, names[n], failures & (1 << n) != 0);
        let cleanup = Cleanup {
            owners: vec![make(2), make(1), make(0)],
            stop_service: make(3),
            drain_health: make(4),
            final_health: make(5),
            close_journal: make(6),
        };
        let admission_log = log.clone();
        let shutdown = Shutdown::new(move || admission_log.lock().unwrap().push("fence"), cleanup);
        let first = shutdown.close().await;
        assert_eq!(first, shutdown.close().await);
        assert_eq!(
            *log.lock().unwrap(),
            [vec!["fence"], names.to_vec()].concat()
        );
        match names
            .iter()
            .enumerate()
            .find(|(n, _)| failures & (1 << n) != 0)
        {
            Some((_, name)) => assert_eq!(first.unwrap_err().code, *name),
            None => assert_eq!(first, Ok(())),
        }
    }
}

#[tokio::test]
async fn concurrent_callers_wait_and_cancelled_caller_does_not_cancel_cleanup() {
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let (enter, wait) = (entered.clone(), release.clone());
    let log = Arc::new(Mutex::new(Vec::new()));
    let shutdown = Arc::new(Shutdown::new(
        || {},
        Cleanup {
            owners: vec![Box::pin(async move {
                enter.notify_one();
                wait.notified().await;
                Ok(())
            })],
            stop_service: action(&log, "service", true),
            drain_health: action(&log, "drain", false),
            final_health: action(&log, "health", false),
            close_journal: action(&log, "journal", false),
        },
    ));
    let first = shutdown.clone();
    let first = tokio::spawn(async move { first.close().await });
    entered.notified().await;
    first.abort();
    let mut waiters = Vec::new();
    for _ in 0..8 {
        let shutdown = shutdown.clone();
        waiters.push(tokio::spawn(async move { shutdown.close().await }));
    }
    tokio::task::yield_now().await;
    assert!(waiters.iter().all(|waiter| !waiter.is_finished()));
    assert!(log.lock().unwrap().is_empty());
    release.notify_one();
    for waiter in waiters {
        let result: IvyResult<()> = waiter.await.unwrap();
        assert_eq!(result.unwrap_err().code, "service");
    }
    assert_eq!(
        *log.lock().unwrap(),
        ["service", "drain", "health", "journal"]
    );
}
