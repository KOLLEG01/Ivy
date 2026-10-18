//! Parent-approved difference: a missing name is not recovered tree retirement.
use super::*;
use ivy_data_collector::{cache::ExecutionCache, engine::Runner, runner::TaskRunner};

fn cached_run(fixture: &Fixture) -> IvyJson {
    let db = sqlite::open(
        &database(fixture),
        sqlite::OpenOptions {
            busy_timeout: Duration::from_secs(1),
            read_only: true,
        },
    )
    .unwrap();
    let document: String = db
        .query_row(
            "SELECT document FROM runs WHERE id='crashed-fixture'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    parse(&document, usize::MAX).unwrap()
}

async fn request_run(fixture: &Fixture) -> IvyJson {
    let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", fixture.port))
        .await
        .unwrap();
    stream.write_all(b"POST /tasks/fixture/run HTTP/1.1\r\nHost: localhost\r\nIdempotency-Key: replacement\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}").await.unwrap();
    crate::http::read_reply(stream).await
}

async fn scenario(original: bool) {
    let fixture = Fixture::new(true).await;
    stage(&fixture).await;
    if original {
        std::fs::copy(
            std::env::var_os("IVY_TEST_JOB_LAUNCHER").unwrap(),
            fixture.root.join("artifact/dist/native/ivy-job.exe"),
        )
        .unwrap();
    }
    let runner = TaskRunner::new(
        fixture.root.join("work/tasks"),
        fixture.root.join("artifact"),
        Some(&node()),
        fixture.config.required("settings").unwrap().clone(),
        Some(payload(&fixture).join("ivy-job.exe")),
        None,
    )
    .await
    .unwrap();
    let name = runner.job_name("fixture").unwrap();
    assert_eq!(runner.job_name("fixture").unwrap(), name);
    assert!(runner.job_name("../outside").is_err());
    assert!(ivy_host_runtime::platform::windows_job::job_stopped(&name).unwrap());
    assert!(ivy_host_runtime::platform::windows_job::job_stopped("\0").is_err());
    for (task, job) in [
        ("fixture", name.clone()),
        ("fixture", "Global\\Ivy.Collector.foreign".into()),
        ("fixture", "\0".into()),
        ("../outside", name.clone()),
    ] {
        assert!(!runner.stopped(task.into(), job).await.unwrap());
    }
    {
        // Seed a real running-cache snapshot, not an actual escaped process.
        // The process startup itself must mark it interrupted/outcome_unknown.
        let store = CollectorStore::open(
            &database(&fixture),
            Limits::default(),
            "2026-01-01T00:00:00.000Z",
        )
        .unwrap();
        let mut row = store.get("fixture", false).unwrap().unwrap();
        row.insert(
            "state",
            IvyJson::object([("cursor", IvyJson::Number(42.0))]),
        );
        store.save(&row).unwrap();
        let cache = ExecutionCache { store: &store };
        cache
            .enqueue(row, "crashed-fixture", IvyJson::Null, 0.0)
            .unwrap();
        let mut run = cache
            .mark_running("crashed-fixture", "2026-01-01T00:00:00.000Z")
            .unwrap();
        cache.register_job(&mut run, &name).unwrap();
    }
    let child = launch_service(&fixture, original).await;
    ready(&fixture, &child, None).await;
    let marker = fixture.root.join("work/tasks/fixture/marker");
    if original {
        tokio::time::timeout(Duration::from_secs(12), async {
            while cached_run(&fixture).get("error") == Some(&"outcome_unknown".into()) {
                tokio::time::sleep(Duration::from_millis(30)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            cached_run(&fixture).get("error"),
            Some(&"Worker termination verified; execution outcome remains unknown.".into())
        );
    } else {
        // Cross two scheduled recovery observations, with a due cached task and
        // an exact absent Job name, before attempting manual replacement.
        tokio::time::sleep(Duration::from_millis(2400)).await;
        let run = cached_run(&fixture);
        assert_eq!(run.get("status"), Some(&"interrupted".into()));
        assert_eq!(run.get("error"), Some(&"outcome_unknown".into()));
        assert_eq!(run.get("workerJobName"), Some(&name.into()));
    }
    assert!(!marker.exists());
    let response = request_run(&fixture).await;
    if original {
        assert_eq!(response.get("status"), Some(&IvyJson::Number(202.0)));
        tokio::time::timeout(Duration::from_secs(10), async {
            while !marker.exists() {
                tokio::time::sleep(Duration::from_millis(30)).await;
            }
        })
        .await
        .unwrap();
    } else {
        assert_eq!(response.get("status"), Some(&IvyJson::Number(400.0)));
        assert_eq!(
            response.required("body").unwrap().get("error"),
            Some(&"outcome_unknown".into())
        );
        assert!(!marker.exists());
        let db = sqlite::open(
            &database(&fixture),
            sqlite::OpenOptions {
                busy_timeout: Duration::from_secs(1),
                read_only: true,
            },
        )
        .unwrap();
        let count: u32 = db
            .query_row("SELECT count(*) FROM runs", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
        let row: String = db
            .query_row("SELECT document FROM tasks WHERE id='fixture'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(
            parse(&row, usize::MAX).unwrap().get("state"),
            Some(&IvyJson::object([("cursor", IvyJson::Number(42.0))]))
        );
    }
    stop(&fixture, &child).await;
    if !original {
        assert!(!marker.exists());
    }
}

#[tokio::test]
async fn restarted_cache_keeps_unknown_fence_despite_missing_job_name() {
    tokio::time::timeout(Duration::from_secs(90), async {
        scenario(true).await;
        scenario(false).await;
    })
    .await
    .unwrap();
}
