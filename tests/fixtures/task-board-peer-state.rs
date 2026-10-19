use super::*;
use ivy_task_board::engine_read::peer_state;

#[tokio::test(flavor = "current_thread")]
async fn peer_state_matches_original_paging_pins_paths_order_and_errors() {
    let output = Command::new("node")
        .args([
            "--experimental-transform-types",
            "--disable-warning=ExperimentalWarning",
            "tests/fixtures/task-board-peer-state-oracle.mjs",
        ])
        .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let cases = parse(
        std::str::from_utf8(&output.stdout).unwrap(),
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
        let result = peer_state(&store, f(row, "task"), s(row, "taskId"))
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
}
impl Contracts for PeerContracts {
    fn version(&self, key: &str) -> IvyResult<String> {
        assert_eq!(key, "task-board/task");
        Ok("1.8.0".into())
    }
    fn local(&self, key: &str) -> IvyResult<bool> {
        assert_eq!(key, "task-board/task");
        Ok(true)
    }
    fn readable_version(&self, key: &str, version: &JsString) -> IvyResult<bool> {
        Ok(version == &JsString::from(self.version(key)?))
    }
    fn validate_record(&self, key: &str, _: &JsString, value: &Json) -> IvyResult<()> {
        assert_eq!(key, "task-board/task");
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
    let journal = JournalStore {
        journal: local,
        client: Arc::new(NoRemote),
        contracts: PeerContracts {
            values: vec![old.clone(), newer.clone()],
        },
        limits: HistoryLimits::default(),
        now: || "2026-01-02T00:00:00.000Z".to_owned(),
        new_id: || -> JsString { panic!("Observation cannot allocate identities") },
        cache: None,
    };
    let store = LifecycleJournal {
        journal: &journal,
        pages: &pages,
    };
    let own = task("c:/work/path", "todo");
    let expected = JsString::from(hash_json(&Json::Array(vec!["peer:waiting".into()])).unwrap());
    assert_eq!(
        peer_state(&store, &own, &"self".into()).await.unwrap(),
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
        peer_state(&store, &own, &"self".into()).await.unwrap(),
        expected
    );
    *pages.pin.borrow_mut() = second;
    assert_eq!(
        peer_state(&store, &own, &"self".into()).await.unwrap(),
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
