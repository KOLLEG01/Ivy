#[path = "reference-data.rs"]
mod reference_data;
use super::*;
use ivy_task_board::{
    journal_store::ReadScopes,
    native_driver::DriverRuntime,
    native_orchestrator::{OrchestrationEngine, OrchestrationPolicies, OrchestrationRuntime},
};

struct UnusedPolicies;
impl ivy_task_board::wake::WakeStore for World {
    async fn watched_task(&self, id: &JsString) -> IvyResult<Json> {
        let document = self
            .lifecycle_read("task-board/task", &Json::String(id.clone()), true, true)
            .await?;
        Ok(Json::object([
            ("value", document.document.value),
            ("metadata", document.metadata),
        ]))
    }
    fn deleted(&self, error: &IvyError) -> bool {
        ivy_task_board::wake::WakeFailure::deleted(error)
    }
}
impl OrchestrationPolicies for UnusedPolicies {
    async fn recover_pending_native_operation(
        &self,
        _: &JsString,
        _: &JsString,
    ) -> IvyResult<bool> {
        panic!("Peer observation must not recover native operations")
    }
    async fn sync_native_comments(&self, _: &JsString) -> IvyResult<()> {
        panic!("Peer observation must not admit native comments")
    }
}

#[tokio::test(flavor = "current_thread")]
async fn peer_state_matches_original_paging_pins_paths_order_and_errors() {
    let output = reference_data::read(include_bytes!("task-board-peer-state-cases.json.gz"));
    let cases = parse(
        std::str::from_utf8(&output).unwrap(),
        MANAGEMENT_FRAME_BYTES,
    )
    .unwrap();
    let mut boundaries = 0;
    for row in cases.as_array().unwrap() {
        let expected = f(row, "boundaries").as_array().unwrap();
        boundaries += expected.len();
        let replay = Arc::new(Replay {
            steps: Mutex::new(expected.iter().cloned().collect()),
            name: s(row, "name").clone(),
        });
        let store = World {
            replay: replay.clone(),
            root: Some("root".into()),
        };
        let scopes = ReadScopes::default();
        let driver = DriverRuntime::new(
            Arc::new(NoRemote),
            "caller".into(),
            "task-board-node".into(),
            |_: Json| std::future::ready(Err(IvyError::internal())),
            || panic!("Peer observation must not read the native clock"),
        );
        let runtime = OrchestrationRuntime {
            driver: &driver,
            store: &store,
            policies: &UnusedPolicies,
            read_scopes: &scopes,
            now_millis: || panic!("Peer observation must not read retry time"),
        };
        let result = runtime
            .with_read_cache(runtime.peer_state(f(row, "task"), s(row, "taskId")))
            .await
            .map(Json::String);
        assert_eq!(
            hash_json(&outcome(result)).unwrap(),
            s(row, "expectedHash").to_utf8().unwrap(),
            "{:?}",
            s(row, "name")
        );
        assert!(
            replay.steps.lock().unwrap().is_empty(),
            "{:?}: Unused source boundaries",
            s(row, "name")
        );
    }
    assert_eq!(cases.as_array().unwrap().len(), 37);
    assert_eq!(boundaries, 190);
}

use ivy_store::rusqlite::Connection;
use ivy_task_board::{
    journal_store::JournalStore,
    lifecycle_journal::{LifecycleJournal, LifecyclePages},
    local_journal::{LocalJournal, initialize},
    local_retention::HistoryLimits,
};
use std::cell::{Cell, RefCell};

struct PeerContracts {
    values: Vec<Json>,
    reads: Cell<usize>,
    local: bool,
}
impl Contracts for PeerContracts {
    fn version(&self, key: &str) -> IvyResult<String> {
        assert_eq!(key, "task-board/task");
        Ok("1.8.0".into())
    }
    fn local(&self, key: &str) -> IvyResult<bool> {
        assert_eq!(key, "task-board/task");
        Ok(self.local)
    }
    fn readable_version(&self, key: &str, version: &JsString) -> IvyResult<bool> {
        Ok(version == &JsString::from(self.version(key)?))
    }
    fn validate_record(&self, key: &str, _: &JsString, value: &Json) -> IvyResult<()> {
        assert_eq!(key, "task-board/task");
        self.reads.set(self.reads.get() + 1);
        assert!(
            self.values
                .iter()
                .any(|v| hash_json(v).unwrap() == hash_json(value).unwrap())
        );
        Ok(())
    }
    fn validate_definition(&self, _: &str, _: &Json) -> IvyResult<()> {
        panic!("Peer observation must not admit an action")
    }
}
struct NoRemote;
impl RpcClient for NoRemote {
    fn request<'a>(&'a self, method: &'a str, _: Json, _: RequestOptions) -> RequestFuture<'a> {
        Box::pin(async move { panic!("No external effect in pinned peer observation: {method}") })
    }
}
struct Snapshot {
    pin: RefCell<Json>,
    calls: Cell<usize>,
}
impl LifecyclePages for Snapshot {
    async fn page(&self, key: &str, query: &Json) -> IvyResult<Json> {
        assert_eq!(key, "task-board/task");
        assert_eq!(query.get("limit"), Some(&Json::Number(100.0)));
        assert_eq!(
            f(f(query, "where"), "field"),
            &Json::from("data:/workflowState")
        );
        self.calls.set(self.calls.get() + 1);
        Ok(Json::object([
            (
                "items",
                Json::Array(vec![
                    Json::object([
                        ("objectId", "self".into()),
                        ("revision", Json::Number(99.0)),
                    ]),
                    self.pin.borrow().clone(),
                ]),
            ),
            ("nextCursor", Json::Null),
        ]))
    }
}
fn task(path: &str, state: &str) -> Json {
    Json::object([
        (
            "claim",
            Json::object([(
                "workspace",
                Json::object([("hostId", "host".into()), ("canonicalCwd", path.into())]),
            )]),
        ),
        ("lastRun", Json::Null),
        ("workflowState", state.into()),
    ])
}
#[tokio::test(flavor = "current_thread")]
async fn peer_state_reads_the_paged_sqlite_revision_even_after_a_later_workspace_change() {
    let database = Connection::open_in_memory().unwrap();
    initialize(&database).unwrap();
    let root = JsString::from("root");
    let local = LocalJournal {
        connection: &database,
        root_object_id: Some(root.clone()),
    };
    let old = task("C:\\WORK\\Path\\", "waiting");
    let newer = task("c:/another", "review");
    let at = JsString::from("2026-01-02T00:00:00.000Z");
    let id = JsString::from("peer");
    let first = local
        .create(
            "task-board/task",
            &"Peer".into(),
            Some(&root),
            &old,
            &at,
            &id,
        )
        .unwrap();
    let second = local
        .update("task-board/task", &id, 1.0, &newer, &at)
        .unwrap();
    let rows = || {
        database
            .query_row("SELECT count(*) FROM revisions", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap()
    };
    assert_eq!(rows(), 2);
    let pages = Snapshot {
        pin: RefCell::new(first.clone()),
        calls: Cell::new(0),
    };
    // Finite explicit local Task admission/routing. This does not supply the
    // current Task schema, production page engine or public-record placement.
    let scopes = ReadScopes::default();
    let journal = JournalStore {
        journal: local,
        client: Arc::new(NoRemote),
        contracts: PeerContracts {
            values: vec![old.clone(), newer.clone()],
            reads: Cell::new(0),
            local: true,
        },
        limits: HistoryLimits::default(),
        now: || "2026-01-02T00:00:00.000Z".to_owned(),
        new_id: || -> JsString { panic!("Observation cannot allocate identities") },
        cache: Some(scopes.cache()),
    };
    let store = LifecycleJournal {
        journal: &journal,
        pages: &pages,
    };
    let own = task("c:/work/path", "todo");
    let expected = JsString::from(hash_json(&Json::Array(vec!["peer:waiting".into()])).unwrap());
    let driver = DriverRuntime::new(
        journal.client.clone(),
        "caller".into(),
        "task-board-node".into(),
        |_: Json| std::future::ready(Err(IvyError::internal())),
        || "2026-01-02T00:00:00.000Z".into(),
    );
    let runtime = OrchestrationRuntime {
        driver: &driver,
        store: &store,
        policies: &UnusedPolicies,
        read_scopes: &scopes,
        now_millis: || 0.0,
    };
    assert_eq!(
        runtime
            .with_read_cache(runtime.peer_state(&own, &"self".into()))
            .await
            .unwrap(),
        expected
    );
    assert_eq!(
        hash_json(
            &store
                .read("task-board/task", &Json::String(id), true)
                .await
                .unwrap()
                .value
        )
        .unwrap(),
        hash_json(&newer).unwrap()
    );
    assert_eq!(
        runtime
            .with_read_cache(runtime.peer_state(&own, &"self".into()))
            .await
            .unwrap(),
        expected
    );
    *pages.pin.borrow_mut() = second;
    assert_eq!(
        runtime
            .with_read_cache(runtime.peer_state(&own, &"self".into()))
            .await
            .unwrap(),
        JsString::from(hash_json(&Json::Array(vec![])).unwrap())
    );
    assert_eq!(
        hash_json(
            &store
                .read("task-board/task", &first, true)
                .await
                .unwrap()
                .value
        )
        .unwrap(),
        hash_json(&old).unwrap()
    );
    assert_eq!(rows(), 2);
    assert_eq!(pages.calls.get(), 3);
}

struct CurrentPolicies {
    refusal: RefCell<Option<IvyError>>,
    calls: RefCell<Vec<Json>>,
}
impl OrchestrationPolicies for CurrentPolicies {
    async fn recover_pending_native_operation(
        &self,
        _: &JsString,
        _: &JsString,
    ) -> IvyResult<bool> {
        panic!("Comment synchronization must not recover native operations")
    }
    async fn sync_native_comments(&self, run: &JsString) -> IvyResult<()> {
        self.calls.borrow_mut().push(Json::String(run.clone()));
        if let Some(error) = self.refusal.borrow().clone() {
            return Err(error);
        }
        Ok(())
    }
}
struct PublicPeer {
    values: [Json; 2],
    calls: Mutex<Vec<Json>>,
}
impl RpcClient for PublicPeer {
    fn request<'a>(&'a self, method: &'a str, args: Json, _: RequestOptions) -> RequestFuture<'a> {
        Box::pin(async move {
            assert_eq!(method, "objects.read");
            assert_eq!(f(&args, "objectId"), &Json::from("peer"));
            self.calls.lock().unwrap().push(args.clone());
            let revision = args.get("revision").and_then(Json::as_f64).unwrap_or(2.0);
            assert!([1.0, 2.0].contains(&revision));
            let value = &self.values[revision as usize - 1];
            Ok(Json::object([
                (
                    "object",
                    Json::object([
                        ("id", "peer".into()),
                        ("contractKey", "task-board/task".into()),
                        ("parentId", "root".into()),
                        ("effectivelyArchived", false.into()),
                    ]),
                ),
                (
                    "revision",
                    Json::object([
                        ("revision", Json::Number(revision)),
                        ("contractVersion", "1.8.0".into()),
                        ("mediaType", "application/json".into()),
                        ("contentHash", hash_json(value)?.into()),
                        (
                            "byteLength",
                            Json::Number(
                                ivy_contracts::json::canonical(value, MANAGEMENT_FRAME_BYTES)?.len()
                                    as f64,
                            ),
                        ),
                    ]),
                ),
                (
                    "content",
                    Json::object([("encoding", "json".into()), ("value", value.clone())]),
                ),
            ]))
        })
    }
}
#[tokio::test(flavor = "current_thread")]
async fn orchestration_runtime_isolates_interleaved_journal_actions_and_keeps_current_policy_refusals()
 {
    use std::{
        future::{Future, poll_fn},
        task::Poll,
    };
    let db = Connection::open_in_memory().unwrap();
    initialize(&db).unwrap();
    let scopes = ReadScopes::default();
    let old = task("c:/work/path", "waiting");
    let newer = task("c:/work/path", "review");
    let journal = JournalStore {
        journal: LocalJournal {
            connection: &db,
            root_object_id: Some("root".into()),
        },
        client: Arc::new(PublicPeer {
            values: [old.clone(), newer.clone()],
            calls: Mutex::default(),
        }),
        contracts: PeerContracts {
            values: vec![old.clone(), newer.clone()],
            reads: Cell::new(0),
            local: false,
        },
        limits: HistoryLimits::default(),
        now: || "2026-01-02T00:00:00.000Z".to_owned(),
        new_id: || -> JsString { panic!("Observation cannot allocate identities") },
        cache: Some(scopes.cache()),
    };
    let at = JsString::from("2026-01-02T00:00:00.000Z");
    let pin = Json::object([("objectId", "peer".into()), ("revision", Json::Number(1.0))]);
    let pages = Snapshot {
        pin: RefCell::new(pin.clone()),
        calls: Cell::new(0),
    };
    let store = LifecycleJournal {
        journal: &journal,
        pages: &pages,
    };
    let refusal = IvyError::new_js(
        "current_policy_refused".into(),
        JsString(vec![0xd800, 0x0021]),
    )
    .with_outcome(Outcome::Unknown)
    .with_details(Json::object([("current", true.into())]));
    let policies = CurrentPolicies {
        refusal: RefCell::new(Some(refusal.clone())),
        calls: RefCell::default(),
    };
    let driver = DriverRuntime::new(
        journal.client.clone(),
        "caller".into(),
        "task-board-node".into(),
        |_: Json| std::future::ready(Err(IvyError::internal())),
        || at.clone(),
    );
    let runtime = OrchestrationRuntime {
        driver: &driver,
        store: &store,
        policies: &policies,
        read_scopes: &scopes,
        now_millis: || 0.0,
    };
    // Exact revision pins are cached by the original store. Bare string latest
    // reads intentionally bypass the action cache. Count actual journal
    // admissions to distinguish inherited and independent action contexts.
    let address = pin;
    let held = Cell::new(true);
    let mut first = Box::pin(runtime.with_read_cache(async {
        let before = store.read("task-board/task", &address, true).await?;
        poll_fn(|_| {
            if held.get() {
                Poll::Pending
            } else {
                Poll::Ready(())
            }
        })
        .await;
        let after = runtime
            .with_read_cache(store.read("task-board/task", &address, true))
            .await?;
        Ok((before.pin, after.pin))
    }));
    poll_fn(|cx| {
        assert!(first.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    assert_eq!(journal.contracts.reads.get(), 1);
    let written_changes = db.total_changes();
    let second = runtime
        .with_read_cache(async {
            let before = store.read("task-board/task", &address, true).await?;
            assert_eq!(journal.contracts.reads.get(), 2);
            let after = runtime
                .with_read_cache(store.read("task-board/task", &address, true))
                .await?;
            assert_eq!(journal.contracts.reads.get(), 2);
            let latest = store.read("task-board/task", &"peer".into(), true).await?;
            assert_eq!(f(&latest.pin, "revision"), &Json::Number(2.0));
            Ok((before.pin, after.pin))
        })
        .await
        .unwrap();
    assert_eq!(f(&second.0, "revision"), &Json::Number(1.0));
    assert_eq!(second.0, second.1);
    assert_eq!(journal.contracts.reads.get(), 3);
    held.set(false);
    let first = first.await.unwrap();
    assert_eq!(f(&first.0, "revision"), &Json::Number(1.0));
    assert_eq!(first.0, first.1);
    assert_eq!(journal.contracts.reads.get(), 3);
    store.read("task-board/task", &address, true).await.unwrap();
    assert_eq!(journal.contracts.reads.get(), 4);
    assert_eq!(
        *journal.client.calls.lock().unwrap(),
        vec![
            address.clone(),
            address.clone(),
            Json::object([("objectId", "peer".into())]),
            address,
        ]
    );

    let run = JsString(vec![0x0072, 0xdfff]);
    assert!(!runtime.run_handoff(&old, &run).unwrap());
    let error = runtime
        .with_read_cache(runtime.sync_native_comments(&run))
        .await
        .unwrap_err();
    assert_eq!(error.message_js(), refusal.message_js());
    assert_eq!(error.outcome, refusal.outcome);
    assert_eq!(error.details, refusal.details);
    *policies.refusal.borrow_mut() = None;
    let mut handed_off = old.clone();
    handed_off.insert(
        "comments",
        Json::Array(vec![Json::object([
            ("purpose", "handoff".into()),
            (
                "run",
                Json::object([("objectId", Json::String(run.clone()))]),
            ),
        ])]),
    );
    assert!(runtime.run_handoff(&handed_off, &run).unwrap());
    assert_eq!(
        runtime.claimed_workflow_state(&Json::Null).unwrap(),
        JsString::from("in_progress")
    );
    runtime.sync_native_comments(&run).await.unwrap();
    assert_eq!(
        *policies.calls.borrow(),
        vec![Json::String(run.clone()), Json::String(run)]
    );
    assert_eq!(pages.calls.get(), 0);
    assert_eq!(db.total_changes(), written_changes);
}
