//! Collector engine scenarios at the Rust RPC boundary with real SQLite reopen.
use ivy_contracts::{IvyError, IvyJson, IvyResult, Outcome, json::parse};
use ivy_data_collector::{
    model,
    publication::{MutationIds, Publisher},
    store::{CollectorStore, Limits},
    store_owner::StoreOwner,
};
use ivy_sdk::rpc::{RequestFuture, RequestOptions, RpcClient};
use std::{collections::BTreeMap, path::PathBuf, sync::Mutex};
use tokio_util::sync::CancellationToken;

fn json(s: &str) -> IvyJson {
    parse(s, usize::MAX).unwrap()
}
fn text(v: &IvyJson, k: &str) -> String {
    model::text(v, k).unwrap()
}
fn number(v: &IvyJson, k: &str) -> f64 {
    model::number(v, k).unwrap()
}
fn field<'a>(v: &'a IvyJson, k: &str) -> &'a IvyJson {
    model::field(v, k).unwrap()
}
fn retention() -> IvyJson {
    json(r#"{"maximumCount":10,"maximumAgeDays":7,"maximumBytes":1048576}"#)
}
struct Database(PathBuf);
impl Database {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!(
            "ivy-collector-publication-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&p).unwrap();
        Self(p)
    }
    fn open(&self) -> StoreOwner {
        StoreOwner::new(
            CollectorStore::open(
                &self.0.join("collector.sqlite"),
                Limits::default(),
                "2026-01-01T00:00:00.000Z",
            )
            .unwrap(),
        )
    }
}
impl Drop for Database {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
async fn seed(owner: &StoreOwner, count: usize, events: bool) {
    owner.call(move|s| {
        let mut row=json(r#"{"task":{"id":"task","name":"Original","enabled":true,"script":"source","dependencies":{},"config":{},"intervalSeconds":1},"revision":1,"resultObjectId":null,"nextAt":9000,"state":{"cursor":99},"deleted":false,"lastRunId":"run-0","retentionWarning":null}"#);
        let mut task=field(&row,"task").clone();task.insert("retention",retention());row.insert("task",task.clone());s.save(&row)?;
        for index in 0..count {
            let mut run=json(r#"{"id":"run","taskId":"task","status":"publishing","input":null,"output":{"data":{"value":5},"state":{"cursor":1}},"startedAt":"2025-01-01T00:00:00.000Z","finishedAt":"2025-01-01T00:00:01.000Z","eventIndex":0,"executionCommitted":true,"error":null}"#);
            run.insert("id",format!("run-{index}").into());run.insert("task",task.clone());
            if events { let mut output=field(&run,"output").clone();output.insert("events",json(r#"[{"name":"first","payload":1},{"name":"second","payload":2}]"#));run.insert("output",output); }
            s.save_run(&run)?;
        } Ok(())
    }).await.unwrap();
}
struct Ids(&'static str);
impl MutationIds for Ids {
    fn scoped<'a>(&'a self, scope: IvyJson) -> RequestFuture<'a> {
        Box::pin(async move { Ok(format!("{}:{}", self.0, scope.as_str()?).into()) })
    }
}
#[derive(Default)]
struct Hive {
    objects: BTreeMap<String, IvyJson>,
    calls: Vec<(String, IvyJson)>,
    writes: usize,
    events: BTreeMap<String, IvyJson>,
    fail_write: bool,
    reject: Option<IvyError>,
    fail_event: Option<usize>,
    protected: f64,
    remaining_bytes: f64,
    reject_retention: bool,
}
struct Client {
    hive: Mutex<Hive>,
    change: Mutex<Option<StoreOwner>>,
}
impl Client {
    fn new() -> Self {
        Self {
            hive: Mutex::new(Hive::default()),
            change: Mutex::new(None),
        }
    }
    fn reply(&self, method: &str, p: IvyJson) -> IvyResult<IvyJson> {
        let mut h = self.hive.lock().unwrap();
        h.calls.push((method.to_owned(), p.clone()));
        match method {
            "objects.query" => {
                let contract = text(&p, "contractKey");
                let items = h
                    .objects
                    .values()
                    .filter(|v| text(field(v, "object"), "contractKey") == contract)
                    .map(|v| {
                        IvyJson::object([("objectId", field(field(v, "object"), "id").clone())])
                    })
                    .collect();
                Ok(IvyJson::object([("items", IvyJson::Array(items))]))
            }
            "objects.read" => h
                .objects
                .get(&text(&p, "objectId"))
                .cloned()
                .ok_or_else(|| IvyError::new("not_found", "fixture")),
            "objects.write" => {
                if let Some(error) = &h.reject {
                    return Err(error.clone());
                }
                let id = p
                    .get("objectId")
                    .map(|v| v.as_str().unwrap())
                    .unwrap_or_else(|| format!("result-{}", text(field(&p, "create"), "name")));
                let (object, revision) = if let Some(previous) = h.objects.get(&id) {
                    assert_eq!(
                        number(&p, "expectedRevision"),
                        number(field(previous, "revision"), "revision")
                    );
                    (
                        field(previous, "object").clone(),
                        number(field(previous, "revision"), "revision") + 1.0,
                    )
                } else {
                    let c = field(&p, "create");
                    (
                        IvyJson::object([
                            ("id", id.clone().into()),
                            ("parentId", field(c, "parentId").clone()),
                            ("name", field(c, "name").clone()),
                            ("contractKey", field(c, "contractKey").clone()),
                            ("effectivelyArchived", false.into()),
                        ]),
                        1.0,
                    )
                };
                let value = IvyJson::object([
                    ("object", object),
                    (
                        "revision",
                        IvyJson::object([("revision", IvyJson::Number(revision))]),
                    ),
                    ("content", field(&p, "content").clone()),
                ]);
                h.objects.insert(id, value.clone());
                h.writes += 1;
                if h.fail_write {
                    h.fail_write = false;
                    return Err(IvyError::new("outcome_unknown", "lost write")
                        .with_outcome(Outcome::Unknown));
                }
                Ok(value)
            }
            "events.publish" => {
                let value = field(&p, "payload");
                let index = number(value, "eventIndex") as usize;
                // A consumer deduplicates run/index even when connection IDs change.
                h.events
                    .insert(format!("{}:{index}", text(value, "runId")), value.clone());
                if h.fail_event == Some(index) {
                    h.fail_event = None;
                    Err(IvyError::new("outcome_unknown", "lost event")
                        .with_outcome(Outcome::Unknown))
                } else {
                    Ok(model::object())
                }
            }
            "objects.stat" => {
                if h.reject_retention {
                    return Err(IvyError::new("service_unavailable", "fixture"));
                }
                Ok(IvyJson::object([(
                    "currentRevision",
                    field(
                        field(h.objects.get(&text(&p, "objectId")).unwrap(), "revision"),
                        "revision",
                    )
                    .clone(),
                )]))
            }
            "objects.pruneRevisions" => Ok(IvyJson::object([
                ("protected", IvyJson::Number(h.protected)),
                ("remainingBytes", IvyJson::Number(h.remaining_bytes)),
            ])),
            _ => panic!("Unexpected RPC {method}"),
        }
    }
}
impl RpcClient for Client {
    fn request<'a>(
        &'a self,
        method: &'a str,
        p: IvyJson,
        options: RequestOptions,
    ) -> RequestFuture<'a> {
        Box::pin(async move {
            if options.cancellation.is_cancelled() {
                return Err(options.cancel_error);
            }
            if method == "objects.write" {
                let change = self.change.lock().unwrap().take();
                if let Some(owner) = change {
                    // Would deadlock if publication held its DB mutex over RPC.
                    owner
                        .call(|s| {
                            let mut row = s.get("task", false)?.unwrap();
                            let mut task = field(&row, "task").clone();
                            task.insert("enabled", false.into());
                            task.insert("name", "New revision".into());
                            row.insert("task", task);
                            row.insert("revision", IvyJson::Number(2.0));
                            row.insert("nextAt", IvyJson::Number(12345.0));
                            s.save(&row)
                        })
                        .await?;
                }
            }
            self.reply(method, p)
        })
    }
}
fn make_publisher(owner: StoreOwner) -> Publisher {
    Publisher::new(owner, "root".to_owned(), retention())
}
async fn flush(p: &mut Publisher, c: &Client, ids: &Ids) {
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        p.flush(c, ids, RequestOptions::default(), &CancellationToken::new()),
    )
    .await
    .unwrap()
    .unwrap();
}
async fn run(owner: &StoreOwner, id: &str) -> IvyJson {
    let id = id.to_owned();
    owner.call(move |s| s.run(&id)).await.unwrap().unwrap()
}
async fn row(owner: &StoreOwner) -> IvyJson {
    owner.call(|s| s.get("task", true)).await.unwrap().unwrap()
}

#[tokio::test]
async fn lost_write_reopens_without_duplicate_revision_and_preserves_order() {
    let db = Database::new();
    let owner = db.open();
    seed(&owner, 2, false).await;
    let c = Client::new();
    c.hive.lock().unwrap().fail_write = true;
    let mut p = make_publisher(owner.clone());
    flush(&mut p, &c, &Ids("epoch-one")).await;
    assert_eq!(text(&run(&owner, "run-0").await, "status"), "publishing");
    assert_eq!(text(&run(&owner, "run-1").await, "status"), "publishing");
    assert_eq!(c.hive.lock().unwrap().writes, 1);
    drop(p);
    drop(owner);
    let owner = db.open();
    let mut p = make_publisher(owner.clone());
    flush(&mut p, &c, &Ids("epoch-two")).await;
    assert_eq!(text(&run(&owner, "run-0").await, "status"), "succeeded");
    assert_eq!(text(&run(&owner, "run-1").await, "status"), "succeeded");
    let writes: Vec<_> = c
        .hive
        .lock()
        .unwrap()
        .calls
        .iter()
        .filter(|(m, _)| m == "objects.write")
        .map(|(_, p)| text(field(field(p, "content"), "value"), "runId"))
        .collect();
    assert_eq!(writes, ["run-0", "run-1"]);
    assert_eq!(number(field(&row(&owner).await, "state"), "cursor"), 99.0);
    assert_eq!(number(&row(&owner).await, "nextAt"), 9000.0);
}

#[tokio::test]
async fn events_resume_durable_index_across_restart_and_epoch_change() {
    let db = Database::new();
    let owner = db.open();
    seed(&owner, 1, true).await;
    let c = Client::new();
    c.hive.lock().unwrap().fail_event = Some(1);
    let mut p = make_publisher(owner.clone());
    flush(&mut p, &c, &Ids("epoch-one")).await;
    assert_eq!(number(&run(&owner, "run-0").await, "eventIndex"), 1.0);
    drop(p);
    drop(owner);
    let owner = db.open();
    let mut p = make_publisher(owner.clone());
    flush(&mut p, &c, &Ids("epoch-two")).await;
    let r = run(&owner, "run-0").await;
    assert_eq!(text(&r, "status"), "succeeded");
    assert_eq!(number(&r, "eventIndex"), 2.0);
    assert_eq!(field(&r, "output"), &IvyJson::Null);
    let h = c.hive.lock().unwrap();
    assert_eq!(h.writes, 1);
    assert_eq!(h.events.len(), 2);
    let indices: Vec<_> = h
        .calls
        .iter()
        .filter(|(m, _)| m == "events.publish")
        .map(|(_, p)| number(field(p, "payload"), "eventIndex") as usize)
        .collect();
    assert_eq!(indices, [0, 1, 1]);
}

#[tokio::test]
async fn latest_disable_survives_and_retention_reports_protection_budget_and_retry() {
    let db = Database::new();
    let owner = db.open();
    seed(&owner, 1, false).await;
    let c = Client::new();
    *c.change.lock().unwrap() = Some(owner.clone());
    c.hive.lock().unwrap().protected = 2.0;
    let mut p = make_publisher(owner.clone());
    flush(&mut p, &c, &Ids("epoch")).await;
    let r = row(&owner).await;
    assert!(!model::truth(field(&r, "task"), "enabled"));
    assert_eq!(text(field(&r, "task"), "name"), "New revision");
    assert_eq!(number(&r, "revision"), 2.0);
    assert_eq!(number(&r, "nextAt"), 12345.0);
    assert_eq!(text(&r, "resultObjectId"), "result-task");
    assert_eq!(
        text(&r, "retentionWarning"),
        "Referenced revisions are protected from retention."
    );
    {
        let mut h = c.hive.lock().unwrap();
        h.protected = 0.0;
        h.remaining_bytes = 2_000_000.0;
    }
    p.retain("task", &c, &Ids("epoch"), RequestOptions::default())
        .await
        .unwrap();
    assert!(text(&row(&owner).await, "retentionWarning").contains("byte budget"));
    c.hive.lock().unwrap().reject_retention = true;
    p.retain("task", &c, &Ids("epoch"), RequestOptions::default())
        .await
        .unwrap();
    assert_eq!(
        text(&row(&owner).await, "retentionWarning"),
        "Retention pending: service_unavailable"
    );
    {
        let mut h = c.hive.lock().unwrap();
        h.reject_retention = false;
        h.remaining_bytes = 0.0;
    }
    p.retain("task", &c, &Ids("epoch"), RequestOptions::default())
        .await
        .unwrap();
    assert_eq!(
        field(&row(&owner).await, "retentionWarning"),
        &IvyJson::Null
    );
    let h = c.hive.lock().unwrap();
    let params = &h
        .calls
        .iter()
        .find(|(m, _)| m == "objects.pruneRevisions")
        .unwrap()
        .1;
    assert_eq!(number(params, "maximumCount"), 10.0);
    assert_eq!(number(params, "maximumAgeDays"), 7.0);
    assert_eq!(number(params, "expectedRevision"), 1.0);
}

#[tokio::test]
async fn uncertain_rejection_stays_pending_definitive_failure_stops_the_pass() {
    let db = Database::new();
    let owner = db.open();
    seed(&owner, 2, false).await;
    let c = Client::new();
    c.hive.lock().unwrap().reject =
        Some(IvyError::new("invalid_arguments", "fixture").with_outcome(Outcome::Unknown));
    let mut p = make_publisher(owner.clone());
    flush(&mut p, &c, &Ids("epoch")).await;
    assert_eq!(text(&run(&owner, "run-0").await, "status"), "publishing");
    c.hive.lock().unwrap().reject = Some(IvyError::new("invalid_arguments", "fixture"));
    flush(&mut p, &c, &Ids("epoch")).await;
    assert_eq!(text(&run(&owner, "run-0").await, "status"), "failed");
    assert_eq!(text(&run(&owner, "run-1").await, "status"), "publishing");
    assert_eq!(number(field(&row(&owner).await, "state"), "cursor"), 99.0);
    c.hive.lock().unwrap().reject = None;
    flush(&mut p, &c, &Ids("epoch")).await;
    assert_eq!(text(&run(&owner, "run-1").await, "status"), "succeeded");
}

#[tokio::test]
async fn malformed_recovered_events_fail_before_remote_effects() {
    let db = Database::new();
    let owner = db.open();
    seed(&owner, 1, true).await;
    owner
        .call(|s| {
            let mut r = s.run("run-0")?.unwrap();
            let mut o = field(&r, "output").clone();
            o.insert(
                "events",
                IvyJson::Array(vec![IvyJson::object([
                    ("name", "large".into()),
                    ("payload", "x".repeat(5000).into()),
                ])]),
            );
            r.insert("output", o);
            s.save_run(&r)
        })
        .await
        .unwrap();
    let c = Client::new();
    let mut p = make_publisher(owner.clone());
    flush(&mut p, &c, &Ids("epoch")).await;
    let r = run(&owner, "run-0").await;
    assert_eq!(text(&r, "status"), "failed");
    assert_eq!(text(&r, "error"), "content_too_large");
    assert!(c.hive.lock().unwrap().calls.is_empty());
}

#[tokio::test]
async fn foreign_and_archived_bindings_are_rejected() {
    for foreign in [false, true] {
        let db = Database::new();
        let owner = db.open();
        seed(&owner, 1, false).await;
        let c = Client::new();
        let mut object = json(
            r#"{"object":{"id":"result-task","parentId":"root","name":"task","contractKey":"data-collector/result","effectivelyArchived":false},"revision":{"revision":1},"content":{"encoding":"json","value":{"runId":"previous"}}}"#,
        );
        let mut metadata = field(&object, "object").clone();
        if foreign {
            metadata.insert("parentId", "foreign-root".into());
        } else {
            metadata.insert("effectivelyArchived", true.into());
        }
        object.insert("object", metadata);
        c.hive
            .lock()
            .unwrap()
            .objects
            .insert("result-task".to_owned(), object);
        let mut p = make_publisher(owner.clone());
        flush(&mut p, &c, &Ids("epoch")).await;
        assert_eq!(
            text(&run(&owner, "run-0").await, "error"),
            "target_conflict"
        );
        assert_eq!(c.hive.lock().unwrap().writes, 0);
    }
}

#[tokio::test]
async fn root_is_cached_and_closing_does_not_start_publications() {
    let db = Database::new();
    let owner = db.open();
    let c = Client::new();
    let mut p = make_publisher(owner.clone());
    p.attach(&c, &Ids("epoch"), None, "Root", RequestOptions::default())
        .await
        .unwrap();
    assert_eq!(p.root_id, "result-Root");
    assert_eq!(
        owner
            .call(|s| s.metadata("rootId"))
            .await
            .unwrap()
            .as_deref(),
        Some("result-Root")
    );
    p.attach(
        &c,
        &Ids("epoch"),
        None,
        "Ignored",
        RequestOptions::default(),
    )
    .await
    .unwrap();
    assert_eq!(c.hive.lock().unwrap().writes, 1);
    seed(&owner, 1, false).await;
    let close = CancellationToken::new();
    close.cancel();
    let before = c.hive.lock().unwrap().calls.len();
    p.flush(&c, &Ids("epoch"), RequestOptions::default(), &close)
        .await
        .unwrap();
    assert_eq!(c.hive.lock().unwrap().calls.len(), before);
    assert_eq!(text(&run(&owner, "run-0").await, "status"), "publishing");
}
