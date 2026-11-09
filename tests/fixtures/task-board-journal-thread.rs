use super::*;
use ivy_store::rusqlite::Connection;
use std::{
    cell::Cell,
    rc::Rc,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

#[tokio::test(flavor = "current_thread")]
async fn sqlite_busy_owner_keeps_io_timers_and_messages_responsive() {
    let path =
        std::env::temp_dir().join(format!("task-board-thread-{}.sqlite", uuid::Uuid::new_v4()));
    let blocker = Connection::open(&path).unwrap();
    blocker
        .execute_batch(
            "CREATE TABLE marker(value INTEGER); INSERT INTO marker VALUES(0); BEGIN IMMEDIATE;",
        )
        .unwrap();
    let worker_path = path.clone();
    let main_thread = std::thread::current().id();
    let (entered, started) = oneshot::channel();
    let finished = Arc::new(AtomicBool::new(false));
    let worker_finished = finished.clone();
    let work = run(move || async move {
        // This graph is deliberately non-Send and constructed on its owner.
        let local = Rc::new(Cell::new(0));
        let database = Connection::open(&worker_path).map_err(ivy_store::sqlite::error)?;
        database
            .busy_timeout(Duration::from_secs(2))
            .map_err(ivy_store::sqlite::error)?;
        entered.send(std::thread::current().id()).unwrap();
        database
            .execute("UPDATE marker SET value=1", [])
            .map_err(ivy_store::sqlite::error)?;
        local.set(1);
        assert_eq!(local.get(), 1);
        worker_finished.store(true, Ordering::Release);
        Ok(())
    });
    let io = async {
        assert_ne!(started.await.unwrap(), main_thread);
        let (ping, pong) = oneshot::channel();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(25)).await;
            ping.send(()).unwrap();
        });
        tokio::time::timeout(Duration::from_secs(1), pong)
            .await
            .unwrap()
            .unwrap();
        assert!(
            !finished.load(Ordering::Acquire),
            "SQLite must still wait for its original lock"
        );
        blocker.execute_batch("COMMIT;").unwrap();
    };
    let (result, ()) = tokio::join!(work, io);
    result.unwrap();
    assert!(finished.load(Ordering::Acquire));
    blocker.execute_batch("BEGIN EXCLUSIVE; ROLLBACK;").unwrap();
    assert_eq!(
        blocker
            .query_row("SELECT value FROM marker", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        1
    );
    drop(blocker);
    std::fs::remove_file(path).unwrap();
}

struct Dropped {
    alive: Arc<AtomicBool>,
    thread: Arc<Mutex<Option<std::thread::ThreadId>>>,
}

#[tokio::test(flavor = "current_thread")]
async fn lost_journal_observer_keeps_the_original_completion_owned() {
    let (entered, started) = oneshot::channel();
    let (release, released) = oneshot::channel();
    let alive = Arc::new(AtomicBool::new(true));
    let finished = Arc::new(AtomicBool::new(false));
    let committed = finished.clone();
    let dropped = Dropped {
        alive: alive.clone(),
        thread: Arc::new(Mutex::new(None)),
    };
    let observer = tokio::spawn(run(move || async move {
        let _dropped = dropped;
        let _local = Rc::new(Cell::new(1));
        entered.send(()).unwrap();
        released.await.unwrap();
        committed.store(true, Ordering::Release);
        Ok(())
    }));
    started.await.unwrap();
    observer.abort();
    assert!(observer.await.unwrap_err().is_cancelled());
    assert!(alive.load(Ordering::Acquire));
    release.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while alive.load(Ordering::Acquire) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(finished.load(Ordering::Acquire));
}
impl Drop for Dropped {
    fn drop(&mut self) {
        *self.thread.lock().unwrap() = Some(std::thread::current().id());
        self.alive.store(false, Ordering::Release);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn journal_owner_drops_resources_and_joins_before_error_or_panic_is_observed() {
    for panic in [false, true] {
        let alive = Arc::new(AtomicBool::new(true));
        let thread = Arc::new(Mutex::new(None));
        let observed = thread.clone();
        let dropped = Dropped {
            alive: alive.clone(),
            thread,
        };
        let expected = IvyError::new("fixture_failure", "original failure");
        let failure = expected.clone();
        let result = run(move || async move {
            let _dropped = dropped;
            let _local = Rc::new(Cell::new(1));
            assert_eq!(std::thread::current().name(), Some("task-board-journal"));
            if panic {
                panic!("fixture owner panic")
            }
            Err(failure)
        })
        .await;
        assert!(!alive.load(Ordering::Acquire));
        assert_ne!(*observed.lock().unwrap(), Some(std::thread::current().id()));
        if panic {
            let failure = result.unwrap_err();
            assert_eq!(failure.code, "service_unavailable");
            assert_eq!(failure.outcome_json(), "unknown".into());
        } else {
            assert_eq!(result.unwrap_err(), expected);
        }
    }
}
