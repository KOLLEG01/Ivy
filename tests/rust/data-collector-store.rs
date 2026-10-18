use ivy_contracts::{
    IvyError, IvyJson,
    json::{parse, stringify},
};
use ivy_data_collector::{
    model,
    store::{CollectorStore, Limits},
};
use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

#[path = "data-collector-runtime.inc.rs"]
mod runtime;

fn json(s: &str) -> IvyJson {
    parse(s, usize::MAX).unwrap()
}
struct Database(PathBuf);
impl Database {
    fn new() -> Self {
        static SEQUENCE: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "ivy-collector-test-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn path(&self) -> PathBuf {
        self.0.join("collector.sqlite")
    }
    fn open(&self, limits: Limits) -> CollectorStore {
        CollectorStore::open(&self.path(), limits, "2026-01-01T00:00:00.000Z").unwrap()
    }
}
impl Drop for Database {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn task(id: &str, last_run: Option<&str>) -> IvyJson {
    let mut row = json(
        r#"{"task":{"id":"task","enabled":true,"script":"secret script","config":{"secret":"value"},"dependencies":{"pkg":"1.0.0"}},"revision":3,"resultObjectId":null,"nextAt":1000,"state":{"cursor":0},"deleted":false,"lastRunId":null,"retentionWarning":null,"future":{"value":"\ud800"}}"#,
    );
    let mut task = model::field(&row, "task").unwrap().clone();
    task.insert("id", id.into());
    row.insert("task", task);
    if let Some(id) = last_run {
        row.insert("lastRunId", id.into());
    }
    row
}
fn run(id: &str, task_id: &str, status: &str) -> IvyJson {
    let mut run = json(
        r#"{"id":"run","taskId":"task","task":{},"status":"running","input":{"secret":"input"},"output":{"data":7},"startedAt":"2025-01-01T00:00:00.000Z","finishedAt":null,"eventIndex":0,"error":null}"#,
    );
    run.insert("id", id.into());
    run.insert("taskId", task_id.into());
    run.insert("status", status.into());
    run.insert(
        "task",
        model::field(&task(task_id, None), "task").unwrap().clone(),
    );
    run
}

#[test]
fn reopening_fences_interrupted_work_preserves_named_job_schedule_and_unknown_fields() {
    let db = Database::new();
    {
        let s = db.open(Limits::default());
        s.save(&task("plain", Some("plain-run"))).unwrap();
        s.save(&task("named", Some("named-run"))).unwrap();
        s.save_run(&run("plain-run", "plain", "running")).unwrap();
        let mut named = run("named-run", "named", "running");
        named.insert("workerJobName", "Global\\Ivy.Collector.fixture".into());
        s.save_run(&named).unwrap();
    }
    let s = db.open(Limits::default());
    for id in ["plain-run", "named-run"] {
        let r = s.run(id).unwrap().unwrap();
        assert_eq!(model::text(&r, "status").unwrap(), "interrupted");
        assert_eq!(model::text(&r, "error").unwrap(), "outcome_unknown");
        assert_eq!(
            model::text(model::field(&r, "task").unwrap(), "script").unwrap(),
            "secret script"
        );
    }
    let plain = s.get("plain", false).unwrap().unwrap();
    let named = s.get("named", false).unwrap().unwrap();
    assert!(!model::truth(
        model::field(&plain, "task").unwrap(),
        "enabled"
    ));
    assert_eq!(model::number(&plain, "revision").unwrap(), 4.0);
    assert!(model::truth(
        model::field(&named, "task").unwrap(),
        "enabled"
    ));
    assert_eq!(model::number(&named, "revision").unwrap(), 3.0);
    assert_eq!(model::number(&named, "nextAt").unwrap(), 1000.0);
    assert!(stringify(&named, usize::MAX).unwrap().contains(r"\ud800"));
    assert_eq!(
        s.connection()
            .query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
    let tables:i64=s.connection().query_row("SELECT count(*) FROM pragma_table_list WHERE schema='main' AND name NOT LIKE 'sqlite_%' AND strict=0",[],|r|r.get(0)).unwrap();
    assert_eq!(tables, 6);
}

#[test]
fn mutation_replay_conflict_capacity_and_rollback_preserve_original_effect() {
    let db = Database::new();
    let s = db.open(Limits {
        operations: 1,
        ..Limits::default()
    });
    let first = s
        .mutate("first", "request", 1, |s| {
            s.set_metadata("effect", "one")?;
            Ok(json(r#"{"runId":"live"}"#))
        })
        .unwrap();
    assert_eq!(
        s.mutate("first", "request", 2, |_| panic!("replay performed work"))
            .unwrap(),
        first
    );
    assert_eq!(
        s.mutate("first", "different", 2, |_| panic!())
            .unwrap_err()
            .code,
        "operation_conflict"
    );
    let failure = s
        .mutate("second", "request2", 2, |s| {
            s.set_metadata("effect", "two")?;
            Ok(IvyJson::Null)
        })
        .unwrap_err();
    assert_eq!(failure.code, "capacity_exceeded");
    assert_eq!(s.metadata("effect").unwrap().as_deref(), Some("one"));
    assert!(s.operation("second").unwrap().is_none());
    s.save_run(&run("live", "task", "succeeded")).unwrap();
    s.mutate("second", "request2", 3, |_| Ok(IvyJson::Null))
        .unwrap();
    assert_eq!(s.operation("first").unwrap_err().code, "operation_expired");
    assert_eq!(
        s.mutate("first", "request", 4, |_| panic!())
            .unwrap_err()
            .code,
        "operation_expired"
    );
    assert_eq!(
        s.mutate("first", "different", 4, |_| panic!())
            .unwrap_err()
            .code,
        "operation_conflict"
    );
    assert!(
        s.connection()
            .execute(
                "INSERT INTO operations VALUES('first','request','null',1)",
                []
            )
            .is_err()
    );
    let err = s.transaction(|s| {
        s.set_metadata("rollback", "bad")?;
        Err::<(), _>(IvyError::new("test_failure", "test"))
    });
    assert!(err.is_err());
    assert!(s.metadata("rollback").unwrap().is_none());
}

#[test]
fn ordered_outbox_is_not_an_execution_lock_and_terminal_compaction_keeps_unknown_evidence() {
    let db = Database::new();
    let s = db.open(Limits {
        runs: 1,
        ..Limits::default()
    });
    s.save_run(&run("z", "task", "publishing")).unwrap();
    s.save_run(&run("a", "task", "publishing")).unwrap();
    assert!(s.busy("task").unwrap());
    assert!(!s.executing("task").unwrap());
    assert_eq!(
        s.runs("publishing")
            .unwrap()
            .iter()
            .map(|r| model::text(r, "id").unwrap())
            .collect::<Vec<_>>(),
        ["z", "a"]
    );
    s.save_run(&run("latest", "task", "succeeded")).unwrap();
    s.save(&task("task", Some("latest"))).unwrap();
    let mut unknown = run("unknown", "other", "failed");
    unknown.insert("error", "outcome_UNKNOWN".into());
    s.save_run(&unknown).unwrap();
    s.save_run(&run("discard", "other", "failed")).unwrap();
    s.cleanup(100).unwrap();
    assert!(s.run("discard").unwrap().is_none());
    assert!(s.used_run_id("discard").unwrap());
    assert!(s.run("latest").unwrap().is_some());
    assert_eq!(s.runs("publishing").unwrap().len(), 2);
    let unknown = s.run("unknown").unwrap().unwrap();
    assert_eq!(
        model::text(model::field(&unknown, "task").unwrap(), "script").unwrap(),
        "secret script"
    );
    let latest = s.run("latest").unwrap().unwrap();
    assert_eq!(
        model::text(model::field(&latest, "task").unwrap(), "script").unwrap(),
        ""
    );
    assert_eq!(latest.get("output"), Some(&IvyJson::Null));
}

#[test]
fn definition_metadata_counts_once_and_expiry_cannot_drop_unresolved_write() {
    let db = Database::new();
    let s = db.open(Limits {
        operations: 1,
        operation_bytes: 1024,
        ..Limits::default()
    });
    s.set_metadata(
        "definition-write:pending:script",
        r#"{"immutable":"request"}"#,
    )
    .unwrap();
    s.set_metadata("definition-expiry:pending", "1").unwrap();
    s.cleanup(10).unwrap();
    assert!(
        s.metadata("definition-write:pending:script")
            .unwrap()
            .is_some()
    );
    s.set_metadata("definition-request:first", "request")
        .unwrap();
    s.set_metadata("definition-write:first:script", "retained write")
        .unwrap();
    s.mutate("first", "request", 1, |_| Ok(IvyJson::Null))
        .unwrap();
    s.mutate("second", "request2", 2, |_| Ok(IvyJson::Null))
        .unwrap();
    assert!(
        s.metadata("definition-write:first:script")
            .unwrap()
            .is_none()
    );
    let guard = parse(
        &s.metadata("definition-request:first").unwrap().unwrap(),
        1024,
    )
    .unwrap();
    assert!(model::truth(&guard, "retired"));
    assert!(
        s.metadata("definition-write:pending:script")
            .unwrap()
            .is_some()
    );
}

#[test]
fn a_second_connection_cannot_steal_an_immediate_transaction() {
    let db = Database::new();
    let s = db.open(Limits::default());
    let second = db.open(Limits::default());
    s.transaction(|s| {
        s.set_metadata("writer", "first")?;
        assert!(
            second
                .transaction(|s| s.set_metadata("writer", "second"))
                .is_err()
        );
        Ok(())
    })
    .unwrap();
    assert_eq!(second.metadata("writer").unwrap().as_deref(), Some("first"));
}

#[test]
fn sqlite_key_coercion_does_not_change_lossless_receipt_request_identity() {
    let db = Database::new();
    let s = db.open(Limits::default());
    let first = json(r#""\ud800""#);
    let second = json(r#""\ud801""#);
    let first_key = model::sqlite_key(&first).unwrap();
    let second_key = model::sqlite_key(&second).unwrap();
    assert_eq!(first_key, "\u{fffd}");
    assert_eq!(first_key, second_key);
    let first_request = stringify(&first, 1024).unwrap();
    let second_request = stringify(&second, 1024).unwrap();
    s.mutate(&first_key, &first_request, 1, |_| Ok(IvyJson::Null))
        .unwrap();
    assert_eq!(
        s.mutate(&second_key, &second_request, 2, |_| panic!())
            .unwrap_err()
            .code,
        "operation_conflict"
    );
}

#[test]
fn node_to_rust_to_node_to_rust_preserves_journal_and_lossless_documents() {
    let db = Database::new();
    let oracle = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../tests/rust/data-collector-store-oracle.mjs");
    let invoke = |phase: &str| {
        let result = std::process::Command::new("node")
            .arg("--disable-warning=ExperimentalWarning")
            .arg(&oracle)
            .arg(phase)
            .arg(db.path())
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
    };
    invoke("seed");
    let opaque;
    {
        let s = db.open(Limits::default());
        let mut row = s.get("oracle", false).unwrap().unwrap();
        opaque = model::field(&row, "future").unwrap().clone();
        assert_eq!(
            model::field(&opaque, "wide")
                .unwrap()
                .as_array()
                .unwrap()
                .len(),
            400001
        );
        assert_eq!(
            stringify(model::field(&opaque, "deep").unwrap(), usize::MAX).unwrap(),
            format!("{}false{}", "[".repeat(300), "]".repeat(300))
        );
        assert_eq!(model::number(&row, "revision").unwrap(), 4.0);
        row.insert("state", IvyJson::Null);
        s.save(&row).unwrap();
        let mut run = s.run("pending").unwrap().unwrap();
        run.insert("executionCommitted", true.into());
        s.save_run(&run).unwrap();
        assert_eq!(
            s.mutate("receipt", "request", 1, |_| panic!("cross-runtime replay"))
                .unwrap(),
            json(r#"{"runId":"pending","value":0}"#)
        );
        s.mutate("rust-receipt", "rust-request", 2, |_| {
            Ok(IvyJson::object([
                ("saved", true.into()),
                ("future", opaque.clone()),
            ]))
        })
        .unwrap();
        assert!(s.used_run_id("retired-run").unwrap());
        assert_eq!(
            s.connection()
                .query_row("PRAGMA foreign_keys", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            s.connection()
                .query_row("PRAGMA busy_timeout", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
    invoke("verify");
    let s = db.open(Limits::default());
    assert_eq!(
        s.operation("node-after-rust").unwrap().unwrap(),
        IvyJson::object([("saved", true.into()), ("future", opaque)])
    );
}

#[test]
fn offline_execution_commits_cursor_before_publication_and_replay_does_not_enqueue_twice() {
    use ivy_data_collector::cache::ExecutionCache;
    let db = Database::new();
    let maximum = json(r#"{"maximumCount":1000,"maximumAgeDays":7,"maximumBytes":104857600}"#);
    {
        let s = db.open(Limits::default());
        let cache = ExecutionCache { store: &s };
        let mut row = task("task", None);
        let mut definition = row.get("task").unwrap().clone();
        definition.insert("intervalSeconds", IvyJson::Number(10.0));
        definition.insert("retention", maximum.clone());
        row.insert("task", definition);
        s.save(&row).unwrap();
        cache
            .admit("task", "one", "request-one", false.into(), 1000)
            .unwrap();
        assert_eq!(
            cache
                .admit("task", "busy", "busy-request", IvyJson::Null, 1100)
                .unwrap_err()
                .code,
            "task_busy"
        );
        let mut first = cache
            .mark_running("one", "2026-01-01T00:00:00.000Z")
            .unwrap();
        cache
            .commit_output(
                &mut first,
                json(r#"{"data":1,"state":{"cursor":1}}"#),
                "2026-01-01T00:00:01.000Z",
                2000.0,
                &maximum,
            )
            .unwrap();
        cache
            .admit("task", "two", "request-two", IvyJson::Null, 3000)
            .unwrap();
        assert_eq!(
            cache.task("task").unwrap().get("state"),
            Some(&json(r#"{"cursor":1}"#))
        );
        let mut second = cache
            .mark_running("two", "2026-01-01T00:00:02.000Z")
            .unwrap();
        // A concurrent disable must survive completion of the earlier snapshot.
        let mut latest = cache.task("task").unwrap();
        let mut t = latest.get("task").unwrap().clone();
        t.insert("enabled", false.into());
        latest.insert("task", t);
        latest.insert("revision", IvyJson::Number(8.0));
        s.save(&latest).unwrap();
        cache
            .commit_output(
                &mut second,
                json(r#"{"data":2,"state":null}"#),
                "2026-01-01T00:00:03.000Z",
                4000.0,
                &maximum,
            )
            .unwrap();
        assert_eq!(s.runs("publishing").unwrap().len(), 2);
        cache
            .admit("task", "one", "request-one", false.into(), 5000)
            .unwrap();
        assert!(s.runs("queued").unwrap().is_empty());
    }
    let s = db.open(Limits::default());
    let cache = ExecutionCache { store: &s };
    let row = cache.task("task").unwrap();
    assert_eq!(row.get("state"), Some(&IvyJson::Null));
    assert_eq!(model::number(&row, "revision").unwrap(), 8.0);
    assert!(!model::truth(row.get("task").unwrap(), "enabled"));
    assert_eq!(model::number(&row, "nextAt").unwrap(), 14000.0);
    assert_eq!(
        s.runs("publishing")
            .unwrap()
            .iter()
            .map(|r| model::text(r, "id").unwrap())
            .collect::<Vec<_>>(),
        ["one", "two"]
    );
    assert_eq!(cache.schedule(30000.0, || "unexpected".into()).unwrap(), 0);
}

#[test]
fn stale_job_observation_does_not_clear_the_current_unknown_execution() {
    use ivy_data_collector::cache::ExecutionCache;
    let db = Database::new();
    let s = db.open(Limits::default());
    let cache = ExecutionCache { store: &s };
    let mut row = task("task", Some("current"));
    let mut t = row.get("task").unwrap().clone();
    t.insert("intervalSeconds", IvyJson::Number(2.0));
    row.insert("task", t);
    s.save(&row).unwrap();
    let mut r = run("current", "task", "failed");
    r.insert("error", "outcome_unknown".into());
    r.insert("workerJobName", "exact-job".into());
    s.save_run(&r).unwrap();
    assert!(
        !cache
            .verified_stopped("task", "older", "exact-job", 1000.0)
            .unwrap()
    );
    assert!(
        !cache
            .verified_stopped("task", "current", "different-job", 1000.0)
            .unwrap()
    );
    assert!(cache.unconfirmed(&cache.task("task").unwrap()).unwrap());
    assert!(
        cache
            .verified_stopped("task", "current", "exact-job", 1000.0)
            .unwrap()
    );
    assert!(!cache.unconfirmed(&cache.task("task").unwrap()).unwrap());
    assert_eq!(
        model::number(&cache.task("task").unwrap(), "nextAt").unwrap(),
        3000.0
    );
    assert!(s.runs("queued").unwrap().is_empty());
    assert!(
        model::unresolved(&s.run("current").unwrap().unwrap()),
        "retain the original unknown outcome evidence"
    );
}

#[test]
fn dc01_deleted_id_adoption_cannot_bypass_capacity_and_transfer_is_consumed_once() {
    use ivy_data_collector::adoption::{Adoption, adopt};
    let db = Database::new();
    let s = db.open(Limits::default());
    let mut old = task("old", None);
    old.insert("deleted", true.into());
    s.save(&old).unwrap();
    s.save(&task("active", None)).unwrap();
    let task = json(r#"{"id":"old","enabled":false,"intervalSeconds":5,"script":"new"}"#);
    let desired = json(
        r#"{"serviceNodeId":"fixture.collector","jobId":"job","enabled":false,"deleted":false,"definitionRevision":1,"rootObjectId":"root","resultObjectId":null}"#,
    );
    let settings = json(r#"{"maximumTasks":1}"#);
    let attempt = || {
        adopt(
            &s,
            Adoption {
                task: &task,
                desired: &desired,
                assignment_id: "assignment",
                assignment_revision: 2.0,
                service_node_id: "fixture.collector",
                settings: &settings,
                confirmed_stopped: false,
                ever_accepted: false,
                active: false,
                now_ms: 1000.0,
            },
        )
    };
    assert_eq!(attempt().unwrap_err().code, "limit_exceeded");
    assert!(model::truth(
        &s.get("old", true).unwrap().unwrap(),
        "deleted"
    ));
    assert_eq!(s.list(false).unwrap().len(), 1);
    let mut active = s.get("active", false).unwrap().unwrap();
    active.insert("deleted", true.into());
    s.save(&active).unwrap();
    s.set_metadata(
        "handoff-state:job",
        r#"{"assignmentId":"assignment","assignmentRevision":2,"state":false,"nextAt":8888}"#,
    )
    .unwrap();
    s.set_metadata("handoff-lock:job", "1").unwrap();
    assert!(attempt().unwrap());
    let mut adopted = s.get("old", false).unwrap().unwrap();
    assert_eq!(adopted.get("state"), Some(&IvyJson::Bool(false)));
    assert_eq!(model::number(&adopted, "nextAt").unwrap(), 8888.0);
    assert_eq!(
        s.metadata("handoff-consumed:job").unwrap().as_deref(),
        Some("true")
    );
    assert_eq!(
        s.metadata("handoff-lock:job").unwrap().as_deref(),
        Some("0")
    );
    adopted.insert("state", IvyJson::Number(42.0));
    adopted.insert("nextAt", IvyJson::Number(9999.0));
    s.save(&adopted).unwrap();
    assert!(attempt().unwrap());
    let final_row = s.get("old", false).unwrap().unwrap();
    assert_eq!(model::number(&final_row, "state").unwrap(), 42.0);
    assert_eq!(model::number(&final_row, "nextAt").unwrap(), 9999.0);
}
